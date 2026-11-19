//! Status bar component for displaying session and system information.

use ratatui::{
  Frame,
  layout::{Constraint, Layout, Rect},
  style::{Modifier, Style},
  text::{Line, Span},
  widgets::Paragraph,
};

use crate::cli::AppData;
use crate::cli::app::CompactionWarning;
use crate::cli::runtime::Runtime;
use crate::utils::colors::{CRITICAL, MUTED, PRIMARY, SUBTLE, TEXT, WARNING};
use crate::utils::token_counter::estimate_chat_messages_tokens;
use crate::view::chat::ChatDisplayState;

/// Model, token breakdown, local rolling windows, and keyboard/context metadata.
pub const STATUS_BAR_HEIGHT: u16 = 4;

/// Token usage percentage threshold for warning level (yellow)
pub const COMPACTION_WARNING_THRESHOLD: usize = 75;
/// Token usage percentage threshold for critical level (red)
pub const COMPACTION_CRITICAL_THRESHOLD: usize = 85;

/// Calculate compaction warning level from warning data
///
/// # Arguments
/// * `warning` - Optional compaction warning data
/// * `log` - Whether to log debug information
///
/// # Returns
/// The appropriate warning level based on usage percentage
pub fn calculate_compaction_warning_level(
  warning: &Option<CompactionWarning>,
  log: bool,
) -> CompactionWarningLevel {
  if let Some(warning) = warning {
    let percentage = warning.usage_percentage();
    if log {
      log::info!(
        "calculate_compaction_warning_level: current_tokens={}, max_context_size={}, percentage={}",
        warning.current_tokens,
        warning.max_context_size,
        percentage
      );
    }
    if percentage >= COMPACTION_CRITICAL_THRESHOLD {
      if log {
        log::info!("Setting compaction_warning to Critical");
      }
      CompactionWarningLevel::Critical
    } else if percentage >= COMPACTION_WARNING_THRESHOLD {
      if log {
        log::info!("Setting compaction_warning to Warning");
      }
      CompactionWarningLevel::Warning
    } else {
      CompactionWarningLevel::None
    }
  } else {
    CompactionWarningLevel::None
  }
}

/// Compaction warning level
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionWarningLevel {
  /// No warning (below threshold)
  None,
  /// Warning (approaching threshold, >= 75%)
  Warning,
  /// Critical (at or above threshold, >= 85%)
  Critical,
}

/// Information to display in the status bar
#[derive(Debug, Clone)]
pub struct StatusBarInfo {
  pub max_steps: usize,
  pub agent_name: Option<String>,
  /// Last request and locally tracked rolling totals; not provider quotas.
  pub usage: crate::usage::UsageSnapshot,
  /// Short session ID (first 8 chars)
  pub session_id: String,
  /// Current model name
  pub model_name: String,
  /// Current display state
  pub state: ChatDisplayState,
  /// Estimated token count for all messages
  pub token_count: usize,
  /// Maximum context size for the model
  pub max_context_size: usize,
  /// Whether history navigation is active
  #[allow(dead_code)]
  pub history_active: bool,
  /// Compaction warning level based on token usage
  pub compaction_warning: CompactionWarningLevel,
  /// Whether plan mode is active
  pub plan_mode: bool,
  /// Zarnkina quota snapshot (5h/weekly) for the status bar
  pub quota: Option<crate::account::quota::QuotaSnapshot>,
}

impl StatusBarInfo {
  /// Create status bar info from AppData
  pub fn from_app_data(
    data: &AppData,
    session_id: &str,
    state: ChatDisplayState,
    runtime: &Runtime,
  ) -> Self {
    // Get short session ID (first 8 characters)
    let short_id = session_id.chars().take(8).collect();

    // Get model name and max context size from config
    let model_name = runtime.default_model();
    let max_context_size = runtime.default_model_max_context_size();

    // Calculate estimated token count from all messages
    let token_count = estimate_chat_messages_tokens(&data.chat_history);

    // Determine compaction warning level
    let compaction_warning = calculate_compaction_warning_level(&data.compaction_warning, false);

    Self {
      max_steps: runtime.max_steps,
      agent_name: runtime.active_agent.clone(),
      usage: runtime.usage.snapshot(),
      session_id: short_id,
      model_name,
      state,
      token_count,
      max_context_size,
      history_active: false, // Will be set by ChatView if needed
      compaction_warning,
      plan_mode: false, // Will be set by ChatView if needed
      quota: Some(runtime.quota.snapshot()),
    }
  }
}

/// Keep usage separate from context occupancy and model metadata.
pub fn render_status_bar(f: &mut Frame, area: Rect, info: &StatusBarInfo) {
  let rows = Layout::vertical([
    Constraint::Length(1),
    Constraint::Length(1),
    Constraint::Length(1),
    Constraint::Length(1),
  ])
  .split(area);
  crate::view::chrome::rule(f, rows[0]);
  render_first_line(f, rows[1], info);
  render_second_line(f, rows[2], info);
  render_third_line(f, rows[3], info);
}

fn format_tokens(value: u64) -> String {
  if value >= 1_000_000 {
    format!("{:.1}m", value as f64 / 1_000_000.0)
  } else if value >= 10_000 {
    format!("{:.1}k", value as f64 / 1_000.0)
  } else {
    value.to_string()
  }
}

fn render_token_usage(f: &mut Frame, area: Rect, usage: &crate::usage::UsageSnapshot) {
  if !usage.available {
    f.render_widget(
      Paragraph::new("Usage unavailable").style(Style::default().fg(WARNING)),
      area,
    );
    return;
  }
  let last = usage.last;
  let mut spans = vec![Span::styled("in ", Style::default().fg(SUBTLE))];
  spans.push(Span::styled(
    last.map(|u| format_tokens(u.input)).unwrap_or_else(|| "n/a".into()),
    Style::default().fg(TEXT),
  ));
  spans.push(Span::styled("  out ", Style::default().fg(SUBTLE)));
  spans.push(Span::styled(
    last.map(|u| format_tokens(u.output)).unwrap_or_else(|| "n/a".into()),
    Style::default().fg(TEXT),
  ));
  spans.push(Span::styled("  cache ", Style::default().fg(SUBTLE)));
  spans.push(Span::styled(
    last.and_then(|u| u.cache).map(format_tokens).unwrap_or_else(|| "n/a".into()),
    Style::default().fg(TEXT),
  ));
  spans.push(Span::styled("  total ", Style::default().fg(SUBTLE)));
  spans.push(Span::styled(
    last.map(|u| format_tokens(u.total)).unwrap_or_else(|| "n/a".into()),
    Style::default().fg(TEXT),
  ));
  f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_first_line(f: &mut Frame, area: Rect, info: &StatusBarInfo) {
  let right_width = if area.width >= 40 { 20 } else { 0 };
  let chunks =
    Layout::horizontal([Constraint::Min(0), Constraint::Length(right_width)]).split(area);
  let mode = if info.plan_mode {
    "Plan"
  } else {
    info.agent_name.as_deref().unwrap_or("Build")
  };
  let mut spans = vec![
    Span::styled(
      format!("{mode}  "),
      Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD),
    ),
    Span::styled(&info.model_name, Style::default().fg(TEXT)),
  ];
  if info.compaction_warning != CompactionWarningLevel::None {
    spans.push(Span::styled(
      "  COMPACT",
      Style::default().fg(
        if info.compaction_warning == CompactionWarningLevel::Critical {
          CRITICAL
        } else {
          WARNING
        },
      ),
    ));
  }
  f.render_widget(Paragraph::new(Line::from(spans)), chunks[0]);
  f.render_widget(
    Paragraph::new(format_state(&info.state))
      .right_aligned()
      .style(Style::default().fg(state_color(&info.state))),
    chunks[1],
  );
}

fn render_second_line(f: &mut Frame, area: Rect, info: &StatusBarInfo) {
  let context = info
    .token_count
    .saturating_mul(100)
    .checked_div(info.max_context_size)
    .map(|percentage| format!("{percentage}% context"))
    .unwrap_or_else(|| {
      if info.session_id == "preview" {
        "local preview".into()
      } else {
        "context unknown".into()
      }
    });
  let right_width = if area.width >= 64 { 40 } else { 0 };
  let chunks =
    Layout::horizontal([Constraint::Min(0), Constraint::Length(right_width)]).split(area);
  f.render_widget(
    Paragraph::new(Line::from(vec![
      Span::styled(info.session_id.clone(), Style::default().fg(MUTED)),
      Span::styled("  ·  ", Style::default().fg(SUBTLE)),
      Span::styled(context, Style::default().fg(SUBTLE)),
      Span::styled(
        if info.session_id == "preview" {
          String::new()
        } else {
          format!("  ·  cap {} steps", info.max_steps)
        },
        Style::default().fg(SUBTLE),
      ),
    ])),
    chunks[0],
  );
  if right_width > 0 {
    render_token_usage(f, chunks[1], &info.usage);
  }
}

/// Quota and rolling local usage on the left, keyboard hints on the right.
fn render_third_line(f: &mut Frame, area: Rect, info: &StatusBarInfo) {
  let hint_width = if area.width >= 70 {
    48
  } else if area.width >= 40 {
    26
  } else {
    0
  };
  let chunks = Layout::horizontal([Constraint::Min(0), Constraint::Length(hint_width)]).split(area);
  let mut spans: Vec<Span> = Vec::new();
  if let Some(quota) = info.quota.as_ref() {
    if quota.available {
      spans.push(Span::styled("prompts ", Style::default().fg(SUBTLE)));
      spans.push(Span::styled(
        if quota.limit > 0 {
          format!("{}/{}", quota.used, quota.limit)
        } else {
          format!("{}", quota.used)
        },
        Style::default().fg(TEXT),
      ));
    } else {
      spans.push(Span::styled(
        "quota ledger unreadable",
        Style::default().fg(WARNING),
      ));
    }
  }
  if info.usage.available {
    if !spans.is_empty() {
      spans.push(Span::styled("  ·  ", Style::default().fg(SUBTLE)));
    }
    spans.push(Span::styled("used ", Style::default().fg(SUBTLE)));
    spans.push(Span::styled(
      format_tokens(info.usage.five_hour.total),
      Style::default().fg(MUTED),
    ));
    spans.push(Span::styled(" in 5h", Style::default().fg(SUBTLE)));
  }
  f.render_widget(Paragraph::new(Line::from(spans)), chunks[0]);
  render_hints(f, chunks[1], chunks[1].width);
}

/// Keyboard hints sit at the far right of the bottom row.
fn render_hints(f: &mut Frame, area: Rect, width: u16) {
  let hint = if width >= 46 {
    "enter send  ·  shift+enter newline  ·  / commands"
  } else if width >= 24 {
    "enter send  ·  / commands"
  } else {
    "enter send"
  };
  f.render_widget(
    Paragraph::new(hint)
      .right_aligned()
      .style(Style::default().fg(MUTED)),
    area,
  );
}

/// Format the display state for the status bar
fn format_state(state: &ChatDisplayState) -> String {
  match state {
    ChatDisplayState::Animating => "Waiting for AI...".to_string(),
    ChatDisplayState::Thinking => "Thinking...".to_string(),
    ChatDisplayState::Streaming => "Generating...".to_string(),
    ChatDisplayState::WaitingInput => "Ready".to_string(),
  }
}

fn state_color(state: &ChatDisplayState) -> ratatui::style::Color {
  match state {
    ChatDisplayState::Animating | ChatDisplayState::Thinking => PRIMARY,
    ChatDisplayState::Streaming => crate::utils::colors::GREEN,
    ChatDisplayState::WaitingInput => MUTED,
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_format_state() {
    assert_eq!(
      format_state(&ChatDisplayState::Animating),
      "Waiting for AI..."
    );
    assert_eq!(format_state(&ChatDisplayState::Thinking), "Thinking...");
    assert_eq!(format_state(&ChatDisplayState::Streaming), "Generating...");
    assert_eq!(format_state(&ChatDisplayState::WaitingInput), "Ready");
  }

  #[test]
  fn usage_footer_reports_token_breakdown_and_time_windows() {
    use ratatui::{Terminal, backend::TestBackend};
    let runtime = Runtime::for_test(crate::config::Config::default());
    let info = StatusBarInfo::from_app_data(
      &AppData::new(),
      "test",
      ChatDisplayState::WaitingInput,
      &runtime,
    );
    let mut terminal = Terminal::new(TestBackend::new(120, 4)).unwrap();
    terminal
      .draw(|f| render_status_bar(f, f.area(), &info))
      .unwrap();
    let buffer = terminal.backend().buffer();
    let text: String = (0..4)
      .flat_map(|y| (0..120).map(move |x| buffer[(x, y)].symbol()))
      .collect();
    for label in ["in", "out", "cache", "total", "prompts"] {
      assert!(text.contains(label), "missing {label}: {text}");
    }
  }
}
