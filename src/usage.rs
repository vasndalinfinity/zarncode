//! Provider-reported token accounting. Local windows are not subscription quotas.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{LlmError, Result};
use crate::llm::provider::{LLMProvider, LlmResponseStream, LlmStreamEvent};
use crate::llm::types::Message;

const FIVE_HOURS: u64 = 5 * 60 * 60;
const WEEK: u64 = 7 * 24 * 60 * 60;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenCounts {
  pub input: u64,
  pub output: u64,
  /// Cache reads are already included in input; never add them to total again.
  pub cache: Option<u64>,
  pub total: u64,
}

impl Default for TokenCounts {
  fn default() -> Self {
    Self {
      input: 0,
      output: 0,
      cache: Some(0),
      total: 0,
    }
  }
}

impl TokenCounts {
  fn add(&mut self, other: Self) {
    self.input = self.input.saturating_add(other.input);
    self.output = self.output.saturating_add(other.output);
    self.total = self.total.saturating_add(other.total);
    self.cache = self
      .cache
      .zip(other.cache)
      .map(|(a, b)| a.saturating_add(b));
  }
}

#[derive(Debug, Clone)]
pub struct UsageSnapshot {
  pub last: Option<TokenCounts>,
  pub five_hour: TokenCounts,
  pub week: TokenCounts,
  pub all_time: TokenCounts,
  /// Totals per model id, busiest first.
  pub by_model: Vec<(String, TokenCounts)>,
  pub available: bool,
}

impl UsageSnapshot {
  pub fn report(&self) -> String {
    if !self.available {
      return "Token usage is unavailable: the local usage ledger could not be read or written."
        .into();
    }
    fn line(label: &str, value: Option<TokenCounts>) -> String {
      match value {
        Some(v) => format!(
          "{label}: Input {} | Output {} | Cache {} | Total {}",
          v.input,
          v.output,
          v.cache
            .map(|n| n.to_string())
            .unwrap_or_else(|| "not reported".into()),
          v.total
        ),
        None => format!("{label}: not reported"),
      }
    }
    [
      "Provider-reported tokens (local tracking)".to_string(),
      line("Last request", self.last),
      line("5h rolling", Some(self.five_hour)),
      line("Week (7d rolling)", Some(self.week)),
      line("All tracked", Some(self.all_time)),
      "Busiest models".into(),
      if self.by_model.is_empty() {
        "  no per-model data yet".to_string()
      } else {
        self
          .by_model
          .iter()
          .take(5)
          .map(|(model, counts)| {
            format!(
              "  {:<32} total {:>10}  in {:>10}  out {:>9}",
              model, counts.total, counts.input, counts.output
            )
          })
          .collect::<Vec<_>>()
          .join("\n")
      },
      "Cache reads are a subset of input, not extra tokens.".into(),
      "These are local token totals, not account quota percentages or reset times.".into(),
      "Older usage before tracking began is not reconstructed.".into(),
    ]
    .join("\n")
  }
}

impl Default for UsageSnapshot {
  fn default() -> Self {
    Self {
      last: None,
      five_hour: TokenCounts::default(),
      week: TokenCounts::default(),
      all_time: TokenCounts::default(),
      by_model: Vec::new(),
      available: true,
    }
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UsageRecord {
  id: String,
  timestamp: u64,
  tokens: TokenCounts,
  /// Model id that produced these tokens; older records have none.
  #[serde(default)]
  model: Option<String>,
}

#[derive(Debug, Default)]
struct UsageState {
  write_failed: bool,
  records: Vec<UsageRecord>,
  as_of: Option<u64>,
  snapshot: UsageSnapshot,
}

#[derive(Debug)]
pub struct UsageStore {
  path: Option<PathBuf>,
  state: Mutex<UsageState>,
}

impl UsageStore {
  pub fn new(data_dir: &Path) -> Self {
    Self {
      path: Some(data_dir.join("usage.jsonl")),
      state: Mutex::new(UsageState::default()),
    }
  }

  #[cfg(test)]
  pub fn in_memory() -> Self {
    Self {
      path: None,
      state: Mutex::new(UsageState::default()),
    }
  }

  fn record(&self, record: UsageRecord) -> io::Result<()> {
    let result = (|| -> io::Result<()> {
      if let Some(path) = &self.path {
        if let Some(parent) = path.parent() {
          fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
          .create(true)
          .read(true)
          .append(true)
          .open(path)?;
        file.lock()?;
        let has_partial_tail = if file.metadata()?.len() > 0 {
          file.seek(SeekFrom::End(-1))?;
          let mut tail = [0];
          file.read_exact(&mut tail)?;
          tail[0] != b'\n'
        } else {
          false
        };
        let mut line = serde_json::to_vec(&record)?;
        line.push(b'\n');
        // Separate a truncated final record from the next complete append.
        if has_partial_tail {
          file.write_all(b"\n")?;
        }
        file.write_all(&line)?;
        file.sync_data()?;
      }
      Ok(())
    })();
    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
    state.write_failed = result.is_err();
    state.as_of = None;
    if result.is_ok() && self.path.is_none() {
      state.records.push(record);
    }
    result
  }

  pub fn snapshot(&self) -> UsageSnapshot {
    self.snapshot_at(now())
  }

  fn snapshot_at(&self, timestamp: u64) -> UsageSnapshot {
    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
    if state.as_of == Some(timestamp) {
      return state.snapshot.clone();
    }
    if let Some(path) = &self.path {
      match read_records(path) {
        Ok(records) => state.records = records,
        Err(error) if error.kind() == io::ErrorKind::NotFound => state.records.clear(),
        Err(_) => {
          state.snapshot.available = false;
          state.as_of = Some(timestamp);
          return state.snapshot.clone();
        }
      }
    }
    let mut snapshot = UsageSnapshot {
      available: !state.write_failed,
      ..UsageSnapshot::default()
    };
    let mut seen = HashSet::new();
    let mut latest = 0;
    let mut per_model: std::collections::HashMap<String, TokenCounts> =
      std::collections::HashMap::new();
    for record in &state.records {
      if record.timestamp > timestamp || !seen.insert(&record.id) {
        continue;
      }
      snapshot.all_time.add(record.tokens);
      let age = timestamp - record.timestamp;
      if age < FIVE_HOURS {
        snapshot.five_hour.add(record.tokens);
      }
      if age < WEEK {
        snapshot.week.add(record.tokens);
      }
      if snapshot.last.is_none() || record.timestamp >= latest {
        latest = record.timestamp;
        snapshot.last = Some(record.tokens);
      }
      if let Some(model) = record.model.as_ref() {
        per_model
          .entry(model.clone())
          .or_default()
          .add(record.tokens);
      }
    }
    let mut by_model: Vec<(String, TokenCounts)> = per_model.into_iter().collect();
    by_model.sort_by(|a, b| b.1.total.cmp(&a.1.total));
    snapshot.by_model = by_model;
    state.snapshot = snapshot.clone();
    state.as_of = Some(timestamp);
    snapshot
  }
}

fn read_records(path: &Path) -> io::Result<Vec<UsageRecord>> {
  let mut file = File::open(path)?;
  file.lock_shared()?;
  let mut contents = String::new();
  file.read_to_string(&mut contents)?;
  let records: Vec<UsageRecord> = contents
    .lines()
    .filter_map(|line| serde_json::from_str(line).ok())
    .collect();
  if !contents.trim().is_empty() && records.is_empty() {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "No valid usage records",
    ));
  }
  Ok(records)
}

fn now() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap_or_default()
    .as_secs()
}

pub struct TrackedProvider {
  inner: Box<dyn LLMProvider>,
  store: Arc<UsageStore>,
  model: String,
}

impl TrackedProvider {
  pub fn new(
    inner: Box<dyn LLMProvider>,
    store: Arc<UsageStore>,
    model: impl Into<String>,
  ) -> Self {
    Self {
      inner,
      store,
      model: model.into(),
    }
  }
}

#[async_trait]
impl LLMProvider for TrackedProvider {
  async fn chat_stream(&self, messages: Vec<Message>) -> Result<LlmResponseStream> {
    let inner = self.inner.chat_stream(messages).await?;
    Ok(Box::pin(UsageStream {
      inner,
      store: self.store.clone(),
      latest: None,
      recorded: false,
      id: Uuid::new_v4().to_string(),
      model: self.model.clone(),
    }))
  }
  fn name(&self) -> &str {
    self.inner.name()
  }
  fn max_context_size(&self) -> usize {
    self.inner.max_context_size()
  }
  async fn on_retryable_error(&mut self, error: &LlmError) {
    self.inner.on_retryable_error(error).await;
  }
}

struct UsageStream {
  inner: LlmResponseStream,
  store: Arc<UsageStore>,
  latest: Option<(u64, TokenCounts)>,
  recorded: bool,
  id: String,
  model: String,
}

impl UsageStream {
  fn finish(&mut self) {
    if self.recorded {
      return;
    }
    self.recorded = true;
    if let Some((timestamp, tokens)) = self.latest
      && let Err(error) = self.store.record(UsageRecord {
        id: self.id.clone(),
        timestamp,
        tokens,
        model: Some(self.model.clone()),
      })
    {
      log::warn!("Could not persist reported token usage: {error}");
    }
  }
}

impl Stream for UsageStream {
  type Item = Result<LlmStreamEvent>;
  fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
    let this = self.get_mut();
    let event = this.inner.as_mut().poll_next(cx);
    if let Poll::Ready(Some(Ok(LlmStreamEvent::Usage {
      prompt_tokens,
      completion_tokens,
      cached_tokens,
      total_tokens,
    }))) = &event
    {
      this.latest = Some((
        now(),
        TokenCounts {
          input: u64::from(*prompt_tokens),
          output: u64::from(*completion_tokens),
          cache: cached_tokens.map(u64::from),
          total: u64::from(*total_tokens),
        },
      ));
    }
    if matches!(event, Poll::Ready(None)) {
      this.finish();
    }
    event
  }
}

impl Drop for UsageStream {
  fn drop(&mut self) {
    self.finish();
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use futures::StreamExt;

  fn tokens() -> TokenCounts {
    TokenCounts {
      input: 100,
      output: 20,
      cache: Some(40),
      total: 120,
    }
  }
  fn record(id: &str, timestamp: u64) -> UsageRecord {
    UsageRecord {
      id: id.into(),
      timestamp,
      tokens: tokens(),
      model: Some("zarnkina-glm-5.3-flash".into()),
    }
  }

  #[test]
  fn rolling_windows_expire_at_exact_boundaries_and_deduplicate() {
    let store = UsageStore::in_memory();
    let timestamp = 1_000_000;
    for r in [
      record("new", timestamp),
      record("new", timestamp),
      record("five", timestamp - FIVE_HOURS),
      record("week", timestamp - WEEK),
      record("future", timestamp + 1),
    ] {
      store.record(r).unwrap();
    }
    let usage = store.snapshot_at(timestamp);
    assert_eq!(usage.five_hour.total, 120);
    assert_eq!(usage.week.total, 240);
    assert_eq!(usage.all_time.total, 360);
    assert_eq!(usage.all_time.input, 300);
    assert_eq!(usage.all_time.cache, Some(120));
  }

  #[test]
  fn persistence_combines_workers_and_survives_partial_tail() {
    let directory = tempfile::tempdir().unwrap();
    let a = UsageStore::new(directory.path());
    let b = UsageStore::new(directory.path());
    a.record(record("a", 100)).unwrap();
    OpenOptions::new()
      .append(true)
      .open(directory.path().join("usage.jsonl"))
      .unwrap()
      .write_all(b"{partial")
      .unwrap();
    b.record(record("b", 101)).unwrap();
    assert_eq!(
      UsageStore::new(directory.path())
        .snapshot_at(102)
        .all_time
        .total,
      240
    );
  }

  #[test]
  fn missing_cache_is_not_a_reported_zero_or_added_to_total() {
    let store = UsageStore::in_memory();
    let mut r = record("a", 100);
    r.tokens.cache = None;
    store.record(r).unwrap();
    let usage = store.snapshot_at(100);
    assert_eq!(usage.last.unwrap().cache, None);
    assert_eq!(usage.all_time.total, 120);
  }

  struct MockProvider;
  #[async_trait]
  impl LLMProvider for MockProvider {
    async fn chat_stream(&self, _: Vec<Message>) -> Result<LlmResponseStream> {
      Ok(Box::pin(futures::stream::iter([10, 20].into_iter().map(
        |output| {
          Ok(LlmStreamEvent::Usage {
            prompt_tokens: 100,
            completion_tokens: output,
            cached_tokens: Some(40),
            total_tokens: 100 + output,
          })
        },
      ))))
    }
    fn name(&self) -> &str {
      "mock"
    }
    fn max_context_size(&self) -> usize {
      1000
    }
  }

  #[tokio::test]
  async fn cumulative_updates_are_recorded_once_per_request() {
    let store = Arc::new(UsageStore::in_memory());
    let provider = TrackedProvider::new(Box::new(MockProvider), store.clone(), "mock-model");
    let stream = provider.chat_stream(Vec::new()).await.unwrap();
    assert_eq!(stream.collect::<Vec<_>>().await.len(), 2);
    assert_eq!(store.snapshot().all_time.total, 120);
    assert_eq!(store.snapshot().last.unwrap().output, 20);
    let mut stream = provider.chat_stream(Vec::new()).await.unwrap();
    stream.next().await.unwrap().unwrap();
    drop(stream);
    assert_eq!(store.snapshot().all_time.total, 230);
  }

  #[test]
  fn concurrent_writers_preserve_all_worker_usage() {
    let directory = tempfile::tempdir().unwrap();
    let workers: Vec<_> = (0..8)
      .map(|i| {
        let path = directory.path().to_path_buf();
        std::thread::spawn(move || {
          let store = UsageStore::new(&path);
          for j in 0..3 {
            store.record(record(&format!("{i}-{j}"), 100)).unwrap();
          }
        })
      })
      .collect();
    for worker in workers {
      worker.join().unwrap();
    }
    assert_eq!(
      UsageStore::new(directory.path())
        .snapshot_at(101)
        .all_time
        .total,
      24 * 120
    );
  }

  #[test]
  fn unreadable_or_failed_storage_is_not_reported_as_zero_usage() {
    let directory = tempfile::tempdir().unwrap();
    let bad = directory.path().join("file");
    fs::write(&bad, "not a directory").unwrap();
    let store = UsageStore::new(&bad);
    assert!(store.record(record("a", 100)).is_err());
    assert!(!store.snapshot_at(101).available);
    fs::write(directory.path().join("usage.jsonl"), "{broken").unwrap();
    assert!(!UsageStore::new(directory.path()).snapshot_at(101).available);
  }
}
