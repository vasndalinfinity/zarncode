//! Local prompt ledger.
//!
//! v5 counts prompts, not tokens: one request is one prompt. The relay is the
//! authority (50 a day free, 150 for pro); this file only keeps the CLI's own
//! tally under a lock so the status bar and `zarncode quota` can show it while
//! two processes never double-count. A limit is enforced locally only when
//! `ZARNKINA_PROMPTS` is set, otherwise the ledger is informative.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const DAY: u64 = 24 * 60 * 60;

fn now() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap_or_default()
    .as_secs()
}

/// Next midnight, UTC. The relay resets on the same boundary.
fn next_midnight(now: u64) -> u64 {
  now - (now % DAY) + DAY
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct DailyCounter {
  #[serde(default)]
  used: u32,
  #[serde(default)]
  reset_at: u64,
}

impl DailyCounter {
  fn roll(&mut self, now: u64) {
    if self.reset_at == 0 || now >= self.reset_at {
      self.used = 0;
      self.reset_at = next_midnight(now);
    }
  }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct AccountQuota {
  #[serde(default)]
  daily: DailyCounter,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LedgerFile {
  #[serde(default = "default_version")]
  version: u32,
  #[serde(default)]
  accounts: BTreeMap<String, AccountQuota>,
}

fn default_version() -> u32 {
  2
}

impl Default for LedgerFile {
  fn default() -> Self {
    Self {
      version: 2,
      accounts: BTreeMap::new(),
    }
  }
}

/// Errors that stop a turn before any tokens are spent.
#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error(
    "quota ledger at {path} is unreadable ({reason}); run `zarncode quota reset` to rebuild it"
  )]
  Corrupt { path: String, reason: String },
  #[error("daily prompt limit reached; resets in {0}")]
  Daily(String),
  #[error("quota storage error: {0}")]
  Storage(#[from] std::io::Error),
}

/// Snapshot shown by `/quota` and the status bar.
#[derive(Debug, Clone)]
pub struct QuotaSnapshot {
  pub label: String,
  pub used: u32,
  /// 0 means "no local ceiling"; the relay still enforces its own.
  pub limit: u32,
  pub remaining: u32,
  pub reset_in: u64,
  pub available: bool,
  pub enforcement: bool,
}

impl QuotaSnapshot {
  pub fn report(&self) -> String {
    if !self.available {
      return "Quota ledger is unavailable: the file could not be read. Run `zarncode quota reset`."
        .into();
    }
    let limit_line = if self.limit > 0 {
      format!("Daily prompts  {}/{} used | resets in {}", self.used, self.limit, humanize(self.reset_in))
    } else {
      format!("Daily prompts  {} used | resets in {}", self.used, humanize(self.reset_in))
    };
    [
      format!("Zarnkina quota - {}", self.label),
      limit_line,
      format!(
        "Enforcement    {}",
        if self.enforcement && self.limit > 0 {
          "on (ZARNKINA_PROMPTS)"
        } else {
          "off (the relay enforces 50 free / 150 pro per day)"
        }
      ),
      "One request is one prompt; model size no longer changes the cost.".to_string(),
    ]
    .join("\n")
  }
}

fn humanize(seconds: u64) -> String {
  let hours = seconds / 3600;
  let minutes = (seconds % 3600) / 60;
  if hours > 0 {
    format!("{hours}h {minutes}m")
  } else {
    format!("{minutes}m")
  }
}

fn effective_limit() -> u32 {
  std::env::var("ZARNKINA_PROMPTS")
    .ok()
    .and_then(|value| value.trim().parse::<u32>().ok())
    .unwrap_or(0)
}

struct MemoryLedger {
  file: LedgerFile,
}

/// File-backed prompt ledger shared by the main agent and its subagents.
pub struct QuotaLedger {
  path: Option<PathBuf>,
  label: String,
  enforce: bool,
  memory: Mutex<MemoryLedger>,
}

impl std::fmt::Debug for QuotaLedger {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("QuotaLedger")
      .field("label", &self.label)
      .field("enforce", &self.enforce)
      .finish()
  }
}

impl QuotaLedger {
  pub fn new(data_dir: &Path, label: impl Into<String>, enforce: bool) -> Self {
    Self {
      path: Some(data_dir.join("quota.json")),
      label: label.into(),
      enforce,
      memory: Mutex::new(MemoryLedger {
        file: LedgerFile::default(),
      }),
    }
  }

  #[cfg(test)]
  pub fn in_memory(enforce: bool) -> Self {
    Self {
      path: None,
      label: "test".into(),
      enforce,
      memory: Mutex::new(MemoryLedger {
        file: LedgerFile::default(),
      }),
    }
  }

  pub fn enforced(&self) -> bool {
    self.enforce
  }

  /// Read-modify-write under an exclusive lock; the file stays the source of
  /// truth so parallel processes cannot double-count a day.
  fn mutate<T>(&self, apply: impl FnOnce(&mut AccountQuota) -> T) -> Result<T, Error> {
    let now_ts = now();
    let Some(path) = self.path.as_ref() else {
      let mut guard = self.memory.lock().unwrap_or_else(|e| e.into_inner());
      let entry = guard.file.accounts.entry(self.label.clone()).or_default();
      entry.daily.roll(now_ts);
      return Ok(apply(entry));
    };

    if let Some(parent) = path.parent() {
      fs::create_dir_all(parent)?;
    }
    let lock_path = path.with_extension("lock");
    let lock = OpenOptions::new()
      .create(true)
      .truncate(false)
      .write(true)
      .open(&lock_path)?;
    lock.lock()?;

    let mut file = match fs::read_to_string(path) {
      Ok(raw) => match serde_json::from_str::<LedgerFile>(&raw) {
        Ok(parsed) => parsed,
        Err(error) => {
          return Err(Error::Corrupt {
            path: path.display().to_string(),
            reason: error.to_string(),
          });
        }
      },
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => LedgerFile::default(),
      Err(error) => return Err(error.into()),
    };

    let entry = file.accounts.entry(self.label.clone()).or_default();
    entry.daily.roll(now_ts);
    let output = apply(entry);
    write_ledger(path, &file)?;
    Ok(output)
  }

  /// Spend one prompt. The reservation refunds itself unless the stream
  /// produced content and called `commit`.
  pub fn reserve(self: &Arc<Self>, _model: &str) -> Result<Reservation, Error> {
    let limit = effective_limit();
    self.mutate(|entry| {
      if limit > 0 && entry.daily.used >= limit {
        return Err(Error::Daily(humanize(entry.daily.reset_at.saturating_sub(now()))));
      }
      entry.daily.used = entry.daily.used.saturating_add(1);
      Ok(())
    })??;
    Ok(Reservation {
      ledger: Arc::clone(self),
      committed: false,
    })
  }

  pub fn snapshot(&self) -> QuotaSnapshot {
    let now_ts = now();
    let computed = self.mutate(|entry| {
      (
        entry.daily.used,
        entry.daily.reset_at.saturating_sub(now_ts),
      )
    });
    let limit = effective_limit();
    match computed {
      Ok((used, reset_in)) => QuotaSnapshot {
        label: self.label.clone(),
        used,
        limit,
        remaining: if limit > 0 { limit.saturating_sub(used) } else { 0 },
        reset_in,
        available: true,
        enforcement: self.enforce,
      },
      Err(_) => QuotaSnapshot {
        label: self.label.clone(),
        used: 0,
        limit,
        remaining: 0,
        reset_in: 0,
        available: false,
        enforcement: self.enforce,
      },
    }
  }

  /// Rebuild the ledger: today's counter goes back to zero.
  pub fn reset(&self) -> Result<(), Error> {
    let now_ts = now();
    self.mutate(|entry| {
      entry.daily = DailyCounter {
        used: 0,
        reset_at: next_midnight(now_ts),
      };
    })?;
    Ok(())
  }

  /// Remove a corrupt ledger so the next turn starts clean.
  pub fn repair(&self) -> Result<(), Error> {
    if let Some(path) = self.path.as_ref() {
      match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
      }
    }
    let mut guard = self.memory.lock().unwrap_or_else(|e| e.into_inner());
    guard.file = LedgerFile::default();
    Ok(())
  }

  fn refund(&self) {
    let _ = self.mutate(|entry| {
      entry.daily.used = entry.daily.used.saturating_sub(1);
    });
  }
}

fn write_ledger(path: &Path, file: &LedgerFile) -> std::io::Result<()> {
  let body = serde_json::to_vec_pretty(file)?;
  let tmp = path.with_extension("json.tmp");
  {
    let mut handle = File::create(&tmp)?;
    handle.write_all(&body)?;
    handle.sync_all()?;
  }
  fs::rename(&tmp, path)
}

/// One prompt; refunded on drop unless committed.
pub struct Reservation {
  ledger: Arc<QuotaLedger>,
  committed: bool,
}

impl Reservation {
  /// The stream produced content, so the prompt is really spent.
  pub fn commit(&mut self) {
    self.committed = true;
  }
}

impl Drop for Reservation {
  fn drop(&mut self) {
    if !self.committed {
      self.ledger.refund();
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  fn ledger(dir: &Path) -> Arc<QuotaLedger> {
    Arc::new(QuotaLedger::new(dir, "user@example.com", true))
  }

  #[test]
  fn reserve_commits_and_refunds() {
    let dir = tempfile::tempdir().unwrap();
    let quota = ledger(dir.path());
    {
      let mut reservation = quota.reserve("zarnkina-glm-5.3-flash").unwrap();
      reservation.commit();
    }
    assert_eq!(quota.snapshot().used, 1);
    drop(quota.reserve("zarnkina-glm-5.3-flash").unwrap());
    assert_eq!(quota.snapshot().used, 1);
  }

  #[test]
  fn reset_clears_the_day() {
    let quota = Arc::new(QuotaLedger::in_memory(true));
    for _ in 0..10 {
      let mut reservation = quota.reserve("zarnkina-glm-5.3-flash").unwrap();
      reservation.commit();
    }
    assert_eq!(quota.snapshot().used, 10);
    quota.reset().unwrap();
    assert_eq!(quota.snapshot().used, 0);
  }

  #[test]
  fn corrupt_file_fails_closed_until_repaired() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("quota.json"), "{broken").unwrap();
    let quota = ledger(dir.path());
    assert!(matches!(
      quota.reserve("zarnkina-glm-5.3-flash"),
      Err(Error::Corrupt { .. })
    ));
    assert!(!quota.snapshot().available);
    quota.repair().unwrap();
    assert!(quota.reserve("zarnkina-glm-5.3-flash").is_ok());
  }
}
