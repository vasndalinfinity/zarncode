//! Model picker overlay for `/model`.
//!
//! Lists every model alias from the config with its provider and relay id, marks
//! the current one, paginates when the list is long, and switches the running
//! session on Enter.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
  Frame,
  layout::{Alignment, Constraint, Layout, Rect},
  style::{Modifier, Style},
  text::{Line, Span},
  widgets::{Block, Borders, Clear, Paragraph},
};

use crate::cli::AppData;
use crate::cli::runtime::Runtime;
use crate::llm::SessionHandle;
use crate::utils::colors::{HIGHLIGHT, MUTED, PRIMARY, SUBTLE, TEXT};
use crate::view::{ChatView, View};

struct Entry {
  alias: String,
  model_id: String,
  provider: String,
  context: Option<usize>,
  max_tokens: Option<u32>,
}

/// Modal list of configured models.
pub struct ModelPickerView {
  session_handle: SessionHandle,
  runtime: std::sync::Arc<Runtime>,
  entries: Vec<Entry>,
  current: Option<String>,
  selected: usize,
  scroll: usize,
}

/// The models the picker offers.
///
/// The relay catalogue is the source of truth for what can be served. A model
/// it retired disappears from the list; a model it published shows up even
/// when the config file predates it. Local overrides still win.
fn build_entries(runtime: &Runtime) -> Vec<Entry> {
  let mut entries: Vec<Entry> = runtime
    .config
    .models
    .iter()
    .map(|(alias, model)| Entry {
      alias: alias.clone(),
      model_id: model.model.clone(),
      provider: model.provider.clone(),
      context: model.max_context_size,
      max_tokens: model.max_tokens,
    })
    .collect();

  let live: Vec<(String, crate::config::ModelConfig)> = runtime
    .live_models
    .lock()
    .ok()
    .map(|map| map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
    .unwrap_or_default();
  if live.is_empty() {
    return entries;
  }

  let live_ids: Vec<String> = live.iter().map(|(_, cfg)| cfg.model.clone()).collect();
  let live_aliases: Vec<String> = live.iter().map(|(alias, _)| alias.clone()).collect();
  let live_provider = live.first().map(|(_, cfg)| cfg.provider.clone());
  entries.retain(|entry| {
    // A live entry replaces the configured one instead of standing next to it.
    if live_aliases.contains(&entry.alias) {
      return false;
    }
    match &live_provider {
      Some(provider) if &entry.provider == provider => live_ids.contains(&entry.model_id),
      _ => true,
    }
  });
  for (alias, cfg) in live {
    let local = runtime.config.get_model(&alias);
    let context = local
      .and_then(|c| c.max_context_size)
      .or(cfg.max_context_size);
    let max_tokens = local.and_then(|c| c.max_tokens).or(cfg.max_tokens);
    entries.push(Entry {
      alias,
      model_id: cfg.model,
      provider: cfg.provider,
      context,
      max_tokens,
    });
  }
  entries
}

impl ModelPickerView {
  pub fn new(session_handle: SessionHandle, runtime: std::sync::Arc<Runtime>) -> Self {
    let current = runtime
      .active_model
      .lock()
      .ok()
      .and_then(|guard| guard.clone())
      .or_else(|| Some(runtime.default_model()));
    let mut entries = build_entries(&runtime);
    entries.sort_by(|a, b| a.alias.cmp(&b.alias));
    let selected = current
      .as_ref()
      .and_then(|alias| entries.iter().position(|entry| &entry.alias == alias))
      .unwrap_or(0);
    Self {
      session_handle,
      runtime,
      entries,
      current,
      selected,
      scroll: 0,
    }
  }

  fn back(&self, data: &AppData) -> Option<Box<dyn View>> {
    Some(Box::new(ChatView::new(
      data,
      self.session_handle.clone(),
      self.runtime.clone(),
    )))
  }

  /// Rows available for the list, leaving room for the detail and hint lines.
  fn visible_rows(inner: Rect) -> usize {
    usize::from(inner.height.saturating_sub(4)).max(3)
  }
}

impl View for ModelPickerView {
  fn handle_key(&mut self, data: &mut AppData, key: KeyEvent) -> Option<Box<dyn View>> {
    let rows = self.entries.len();
    match key.code {
      KeyCode::Esc | KeyCode::Char('q') => return self.back(data),
      KeyCode::Up | KeyCode::Char('k') => {
        if self.selected > 0 {
          self.selected -= 1;
        }
      }
      KeyCode::Down | KeyCode::Char('j') => {
        if self.selected + 1 < rows {
          self.selected += 1;
        }
      }
      KeyCode::PageUp => self.selected = self.selected.saturating_sub(10),
      KeyCode::PageDown => {
        self.selected = (self.selected + 10).min(rows.saturating_sub(1));
      }
      KeyCode::Enter => {
        if let Some(entry) = self.entries.get(self.selected) {
          self.session_handle.switch_model(entry.alias.clone());
          if let Ok(mut guard) = self.runtime.active_model.lock() {
            *guard = Some(entry.alias.clone());
          }
        }
        return self.back(data);
      }
      _ => {}
    }
    None
  }

  fn draw(&mut self, f: &mut Frame, _data: &AppData) {
    let area = f.area();
    let width = area.width.saturating_mul(8) / 10;
    let height = area.height.saturating_mul(8) / 10;
    let popup = Rect {
      x: area.x + (area.width.saturating_sub(width)) / 2,
      y: area.y + (area.height.saturating_sub(height)) / 2,
      width,
      height,
    };
    f.render_widget(Clear, popup);

    let block = Block::default()
      .borders(Borders::ALL)
      .title(" Select model ")
      .title_alignment(Alignment::Center)
      .border_style(Style::default().fg(PRIMARY));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    if inner.height == 0 || inner.width == 0 {
      return;
    }

    let rows = Self::visible_rows(inner);
    if self.selected < self.scroll {
      self.scroll = self.selected;
    } else if self.selected >= self.scroll + rows {
      self.scroll = self.selected + 1 - rows;
    }

    let total = self.entries.len();
    let mut lines: Vec<Line> = Vec::new();
    for (index, entry) in self.entries.iter().enumerate().skip(self.scroll).take(rows) {
      let is_current = self
        .current
        .as_ref()
        .map(|alias| alias == &entry.alias)
        .unwrap_or(false);
      let is_selected = index == self.selected;
      let row_style = if is_selected {
        Style::default().fg(HIGHLIGHT).add_modifier(Modifier::BOLD)
      } else if is_current {
        Style::default().fg(PRIMARY)
      } else {
        Style::default().fg(TEXT)
      };
      let muted = if is_selected {
        Style::default().fg(HIGHLIGHT)
      } else {
        Style::default().fg(MUTED)
      };
      lines.push(Line::from(vec![
        Span::styled(
          if is_selected { "> " } else { "  " },
          Style::default().fg(HIGHLIGHT),
        ),
        Span::styled(
          if is_current { "x " } else { "  " },
          Style::default().fg(PRIMARY),
        ),
        Span::styled(format!("{:<32}", entry.alias), row_style),
        Span::styled(format!(" [{}]", entry.provider), muted),
      ]));
    }

    // Detail block for the highlighted model.
    let selected = self.entries.get(self.selected);
    let mut detail: Vec<Line> = Vec::new();
    if let Some(entry) = selected {
      detail.push(Line::from(vec![
        Span::styled("Model   ", Style::default().fg(SUBTLE)),
        Span::styled(entry.alias.clone(), Style::default().fg(TEXT)),
        Span::styled(
          if self.current.as_deref() == Some(entry.alias.as_str()) {
            "  (current)"
          } else {
            ""
          },
          Style::default().fg(PRIMARY),
        ),
      ]));
      detail.push(Line::from(vec![
        Span::styled("Id      ", Style::default().fg(SUBTLE)),
        Span::styled(entry.model_id.clone(), Style::default().fg(MUTED)),
      ]));
      let context = entry
        .context
        .map(|value| format!("{}k", value / 1000))
        .unwrap_or_else(|| "-".to_string());
      let output = entry
        .max_tokens
        .map(|value| format!("{}k", value / 1000))
        .unwrap_or_else(|| "-".to_string());
      detail.push(Line::from(vec![
        Span::styled("Limits  ", Style::default().fg(SUBTLE)),
        Span::styled(
          format!("context {context}  output {output}"),
          Style::default().fg(MUTED),
        ),
      ]));
    }

    let layout = Layout::vertical([
      Constraint::Min(1),
      Constraint::Length(1),
      Constraint::Length(detail.len() as u16),
      Constraint::Length(1),
    ])
    .split(inner);

    f.render_widget(Paragraph::new(lines), layout[0]);
    f.render_widget(
      Paragraph::new(Span::styled(
        format!("({}/{})", self.selected + 1, total.max(1)),
        Style::default().fg(SUBTLE),
      ))
      .alignment(Alignment::Right),
      layout[1],
    );
    f.render_widget(Paragraph::new(detail), layout[2]);
    f.render_widget(
      Paragraph::new(Span::styled(
        "Enter select   up/down move   esc cancel",
        Style::default().fg(MUTED),
      ))
      .alignment(Alignment::Center),
      layout[3],
    );
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::config::{Config, ModelConfig, ProviderConfig};

  fn relay_provider() -> ProviderConfig {
    ProviderConfig {
      provider_type: "zarnkina".into(),
      base_url: "https://127.0.0.1:1".into(),
      api_key: None,
      email: None,
      api_version: None,
    }
  }

  fn model(provider: &str, id: &str) -> ModelConfig {
    ModelConfig {
      provider: provider.into(),
      model: id.into(),
      max_context_size: Some(1048576),
      temperature: None,
      max_tokens: Some(32768),
      supports_streaming: true,
      supports_vision: false,
    }
  }

  #[test]
  fn picker_follows_the_relay_catalogue() {
    let mut config = Config::default();
    config
      .providers
      .insert("zarnkina".into(), relay_provider());
    config.models.insert(
      "zarnkina/retired-model".into(),
      model("zarnkina", "zarnkina-retired-model"),
    );
    config.models.insert(
      "other/kept-model".into(),
      model("other", "other-kept-model"),
    );
    let runtime = Runtime::for_test(config);

    runtime.apply_live_catalogue(&[crate::account::ModelInfo {
      id: "zarnkina-fresh-model".into(),
      name: "Fresh".into(),
      tier: "free".into(),
      status: "active".into(),
      disabled: false,
      context_window: Some(262144),
      max_output_tokens: Some(32768),
      usage_tier: Some("Standard".into()),
      reasoning: Some(true),
      inputs: Some("text".into()),
      tag: None,
    }]);

    let entries = build_entries(&runtime);
    let aliases: Vec<&str> = entries.iter().map(|e| e.alias.as_str()).collect();
    assert!(
      !aliases.contains(&"zarnkina/retired-model"),
      "retired model still listed: {aliases:?}"
    );
    assert!(
      aliases.contains(&"zarnkina/fresh-model"),
      "published model missing: {aliases:?}"
    );
    assert!(
      aliases.contains(&"other/kept-model"),
      "model of another provider was dropped: {aliases:?}"
    );
    let fresh = entries
      .iter()
      .find(|e| e.alias == "zarnkina/fresh-model")
      .expect("fresh entry");
    assert_eq!(fresh.context, Some(262144));
    assert_eq!(fresh.model_id, "zarnkina-fresh-model");
  }

  #[test]
  fn picker_never_lists_a_model_twice() {
    let mut config = Config::default();
    config
      .providers
      .insert("zarnkina".into(), relay_provider());
    // The config already knows the model; the catalogue repeats it.
    config.models.insert(
      "zarnkina/fresh-model".into(),
      model("zarnkina", "zarnkina-fresh-model"),
    );
    let runtime = Runtime::for_test(config);
    runtime.apply_live_catalogue(&[crate::account::ModelInfo {
      id: "zarnkina-fresh-model".into(),
      name: "Fresh".into(),
      tier: "free".into(),
      status: "active".into(),
      disabled: false,
      context_window: Some(262144),
      max_output_tokens: Some(32768),
      usage_tier: Some("Standard".into()),
      reasoning: Some(true),
      inputs: Some("text".into()),
      tag: None,
    }]);
    let entries = build_entries(&runtime);
    let mut seen: Vec<&str> = entries.iter().map(|e| e.alias.as_str()).collect();
    let total = seen.len();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), total, "duplicate entries: {seen:?}");
  }

  #[test]
  fn picker_keeps_the_config_list_when_the_catalogue_is_unknown() {
    let mut config = Config::default();
    config
      .providers
      .insert("zarnkina".into(), relay_provider());
    config.models.insert(
      "zarnkina/old-model".into(),
      model("zarnkina", "zarnkina-old-model"),
    );
    let runtime = Runtime::for_test(config);
    let entries = build_entries(&runtime);
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].alias, "zarnkina/old-model");
  }
}
