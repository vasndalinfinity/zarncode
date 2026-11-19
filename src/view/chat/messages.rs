//! MessageListComponent â€” rendering helpers for chat messages, animations, and streaming content.

use ratatui::widgets::Widget;
use ratatui::{
  layout::Rect,
  text::{Line, Span, Text},
  widgets::{Block, Borders, Paragraph},
};

use crate::llm::types::Message;
use crate::utils::char_display_width;
use crate::utils::colors::{
  BLUE, CRITICAL, GREEN, HIGHLIGHT as HIGHLIGHT_COLOR, MUTED, TEXT as TEXT_COLOR, WARNING,
};
use crate::utils::{HIGHLIGHT, PRIMARY_BORDER, THINKING};
use crate::view::diff::render_diff_panel;

/// Status of a tool call
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallStatus {
  /// Tool call is being executed
  Running,
  /// Tool call completed successfully
  Completed,
  #[allow(dead_code)]
  /// Tool call failed
  Failed,
}

/// A chunk of streaming content from LLM
#[derive(Debug, Clone)]
pub enum StreamingChunk {
  /// Normal response content
  Normal(String),
  /// Thinking/reasoning content
  Thinking(String),
  /// Tool call indicator
  ToolCall {
    /// Tool name
    name: String,
    /// Tool arguments (JSON)
    arguments: String,
    /// Current status
    status: ToolCallStatus,
  },
}

#[allow(dead_code)]
impl StreamingChunk {
  /// Get the content of the chunk
  pub fn content(&self) -> &str {
    match self {
      StreamingChunk::Normal(s) => s,
      StreamingChunk::Thinking(s) => s,
      StreamingChunk::ToolCall { .. } => "",
    }
  }

  /// Check if this is a thinking chunk
  pub fn is_thinking(&self) -> bool {
    matches!(self, StreamingChunk::Thinking(_))
  }

  #[allow(dead_code)]
  /// Check if this is a tool call chunk
  pub fn is_tool_call(&self) -> bool {
    matches!(self, StreamingChunk::ToolCall { .. })
  }
}

/// A message in the chat history
#[derive(Debug, Clone)]
pub enum ChatMessage {
  /// User message
  User { content: String },
  /// AI assistant response (with optional thinking content)
  Assistant {
    /// The main response content
    content: String,
    /// The thinking/reasoning content (if any)
    thinking_content: Option<String>,
  },
  /// Tool call result
  ToolCall {
    /// Tool name
    name: String,
    /// Tool arguments (JSON)
    arguments: String,
    /// Tool execution output (e.g., diff or success message)
    output: Option<String>,
  },
  /// System notification (e.g., compaction warning)
  System {
    /// The notification content
    content: String,
    /// Notification level (info, warning, error)
    level: SystemMessageLevel,
  },
}

impl ChatMessage {
  /// Check if this is a user message
  pub fn is_user(&self) -> bool {
    matches!(self, ChatMessage::User { .. })
  }
}

/// Level for system messages
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemMessageLevel {
  /// Informational message
  Info,
  /// Warning message
  Warning,
  /// Error/Critical message
  Error,
}

/// Convert LLM messages into UI chat history.
///
/// This is used when resuming a persisted session to rebuild the visual chat log.
pub fn llm_messages_to_chat_history(messages: &[Message]) -> Vec<ChatMessage> {
  use crate::llm::types::Role;

  let mut history = Vec::new();
  for msg in messages {
    match msg.role {
      Role::System => {}
      Role::User => {
        history.push(ChatMessage::User {
          content: msg.content.clone(),
        });
      }
      Role::Assistant => {
        let content = msg.content.clone();
        let (thinking_content, content) = if let Some(start) = content.find("<think>")
          && let Some(end) = content.find("</think>")
        {
          let think = content[start + 7..end].to_string();
          let after = content[end + 8..].to_string();
          (Some(think), after)
        } else {
          (None, content)
        };
        if !content.is_empty() || thinking_content.is_some() {
          history.push(ChatMessage::Assistant {
            content,
            thinking_content,
          });
        }
      }
      Role::Tool => {
        // Tool results are not rendered directly in the chat history;
        // the corresponding tool call indicator is added via ToolCallCompleted events.
      }
    }
  }
  history
}

/// Wrap text into lines based on available width.
pub fn wrap_text(text: &str, available_width: u16) -> Vec<String> {
  if available_width == 0 {
    return vec![text.to_string()];
  }
  let available = available_width as usize;
  let mut lines: Vec<String> = vec![];
  let mut current_line = String::new();
  let mut current_width = 0;

  for c in text.chars() {
    let char_width = char_display_width(c);

    if c == '\n' {
      lines.push(current_line);
      current_line = String::new();
      current_width = 0;
    } else if current_width + char_width > available {
      lines.push(current_line);
      current_line = c.to_string();
      current_width = char_width;
    } else {
      current_line.push(c);
      current_width += char_width;
    }
  }

  if !current_line.is_empty() {
    lines.push(current_line);
  }

  lines
}

/// Calculate the number of lines needed to display text with given width.
pub fn calculate_line_count(text: &str, available_width: u16) -> usize {
  wrap_text(text, available_width).len().max(1)
}

/// Render a user message on a quiet surface with a single accent edge.
pub fn render_message_box(buf: &mut ratatui::buffer::Buffer, area: Rect, message: &str) {
  use ratatui::style::Style;
  use ratatui::widgets::{BorderType, Padding};
  let block = Block::default()
    .borders(Borders::LEFT)
    .border_type(BorderType::Thick)
    .border_style(*PRIMARY_BORDER)
    .style(
      Style::default()
        .bg(crate::utils::colors::SURFACE)
        .fg(TEXT_COLOR),
    )
    .padding(Padding::new(1, 1, 1, 1));
  let inner = block.inner(area);
  block.render(area, buf);
  let lines: Vec<Line> = wrap_text(message, inner.width)
    .into_iter()
    .map(Line::from)
    .collect();
  Paragraph::new(lines).style(Style::default().fg(TEXT_COLOR)).render(inner, buf);
}

/// Render the moon animation.
/// Waiting for the first token. The upstream may think for a while before it
/// answers, so the wait has to be visible and timed.
pub fn render_moon_animation(
  buf: &mut ratatui::buffer::Buffer,
  area: Rect,
  moon: char,
  elapsed_secs: u64,
) {
  let text = Text::from(vec![Line::from(vec![
    Span::raw("  "),
    Span::styled(moon.to_string(), *HIGHLIGHT),
    Span::raw("  "),
    Span::styled("Waiting for AI", *HIGHLIGHT),
    Span::styled(format!("  {elapsed_secs}s"), MUTED),
  ])]);

  let widget = Paragraph::new(text);
  widget.render(area, buf);
}

/// Render the thinking indicator ("Thinking..." with spinner and elapsed time).
pub fn render_thinking_indicator(
  buf: &mut ratatui::buffer::Buffer,
  area: Rect,
  spinner: char,
  elapsed_secs: u64,
) {
  let text = Text::from(vec![Line::from(vec![
    Span::raw("  "),
    Span::styled(spinner.to_string(), *THINKING),
    Span::raw(" "),
    Span::styled("Thinking", *THINKING),
    Span::styled(format!("  {elapsed_secs}s"), MUTED),
  ])]);

  let widget = Paragraph::new(text);
  widget.render(area, buf);
}

/// Render a response without a heavy surrounding box.
pub fn render_ai_response(buf: &mut ratatui::buffer::Buffer, area: Rect, response: &str) {
  let lines: Vec<Line> = wrap_text(response, area.width)
    .into_iter()
    .map(Line::from)
    .collect();
  Paragraph::new(lines).style(ratatui::style::Style::default().fg(TEXT_COLOR)).render(area, buf);
}

/// Measure the same units that render_chat_message draws.
pub fn message_height(message: &ChatMessage, width: u16) -> usize {
  match message {
    ChatMessage::User { content } => {
      calculate_line_count(content, width.saturating_sub(3)).max(1) + 3
    }
    ChatMessage::Assistant {
      content,
      thinking_content,
    } => {
      calculate_line_count(content, width)
        + thinking_content
          .as_ref()
          .map(|s| calculate_line_count(s, width))
          .unwrap_or(0)
        + 1
    }
    ChatMessage::ToolCall { output, .. } => {
      2 + output
        .as_ref()
        .map(|s| crate::view::diff::diff_render_height(s))
        .unwrap_or(0)
    }
    ChatMessage::System { content, .. } => calculate_line_count(content, width).max(1) + 1,
  }
}

pub fn render_chat_message(buf: &mut ratatui::buffer::Buffer, area: Rect, message: &ChatMessage) {
  match message {
    ChatMessage::User { content } => render_message_box(buf, area, content),
    ChatMessage::Assistant {
      content,
      thinking_content,
    } => {
      let thinking_height = thinking_content
        .as_ref()
        .map(|s| calculate_line_count(s, area.width))
        .unwrap_or(0)
        .min(usize::from(area.height)) as u16;
      if let Some(thinking) = thinking_content {
        render_thinking_content(
          buf,
          Rect::new(area.x, area.y, area.width, thinking_height),
          thinking,
        );
      }
      render_ai_response(
        buf,
        Rect::new(
          area.x,
          area.y + thinking_height,
          area.width,
          area.height.saturating_sub(thinking_height),
        ),
        content,
      );
    }
    ChatMessage::ToolCall {
      name,
      arguments,
      output,
    } => render_tool_call(buf, area, name, arguments, output.as_deref()),
    ChatMessage::System { content, level } => render_system_message(buf, area, content, *level),
  }
}

/// Keep the most recent complete messages visible instead of shrinking every row.
pub fn render_history(buf: &mut ratatui::buffer::Buffer, area: Rect, entries: &[&ChatMessage]) {
  render_history_scrolled(buf, area, entries, 0)
}

/// Render the tail of the history, optionally skipping the newest `skip_tail`
/// messages so the user can scroll back through older turns.
pub fn render_history_scrolled(
  buf: &mut ratatui::buffer::Buffer,
  area: Rect,
  entries: &[&ChatMessage],
  skip_lines: usize,
) {
  if area.is_empty() {
    return;
  }
  let heights: Vec<usize> = entries
    .iter()
    .map(|message| message_height(message, area.width).max(1))
    .collect();
  let total: usize = heights.iter().sum();
  let viewport = usize::from(area.height);
  let max_scroll = total.saturating_sub(viewport);
  let offset = skip_lines.min(max_scroll);

  if offset == 0 {
    // Bottom anchored: draw as many complete messages as fit, newest last.
    let mut visible = Vec::new();
    let mut remaining = viewport;
    for (message, height) in entries.iter().zip(heights.iter()).rev() {
      if *height > remaining && !visible.is_empty() {
        break;
      }
      let allocated = (*height).min(remaining);
      visible.push((*message, allocated as u16));
      remaining -= allocated;
      if remaining == 0 {
        break;
      }
    }
    let mut y = area.y;
    for (message, height) in visible.into_iter().rev() {
      render_chat_message(
        buf,
        Rect::new(area.x, y, area.width, height.saturating_sub(1).max(1)),
        message,
      );
      y += height;
    }
    return;
  }

  // Line scrolling: render the whole transcript into an offscreen buffer and
  // copy the requested window, so scrolling moves by lines rather than by
  // whole messages.
  let height_u16 = total.min(usize::from(u16::MAX)) as u16;
  let mut temp = ratatui::buffer::Buffer::empty(Rect::new(0, 0, area.width, height_u16));
  let mut y = 0u16;
  for (message, height) in entries.iter().zip(heights.iter()) {
    if y >= height_u16 {
      break;
    }
    let height = (*height).min(usize::from(height_u16 - y)) as u16;
    if height == 0 {
      break;
    }
    render_chat_message(
      &mut temp,
      Rect::new(0, y, area.width, height.saturating_sub(1).max(1)),
      message,
    );
    y += height;
  }

  let start_row = max_scroll.saturating_sub(offset) as u16;
  for row in 0..viewport {
    let src_y = start_row + row as u16;
    if src_y >= height_u16 {
      break;
    }
    for x in 0..area.width {
      if let Some(cell) = temp.cell((x, src_y)).cloned() {
        if let Some(slot) = buf.cell_mut((area.x + x, area.y + row as u16)) {
          *slot = cell;
        }
      }
    }
  }
}

/// Render thinking content with grey italic style.
pub fn render_thinking_content(buf: &mut ratatui::buffer::Buffer, area: Rect, content: &str) {
  let wrapped_lines = wrap_text(content, area.width);
  let lines: Vec<Line> = wrapped_lines
    .into_iter()
    .map(|line| Line::from(vec![Span::styled(line, *THINKING)]))
    .collect();
  let text = Paragraph::new(Text::from(lines));
  text.render(area, buf);
}

/// Render a tool call message.
pub fn render_tool_call(
  buf: &mut ratatui::buffer::Buffer,
  area: Rect,
  name: &str,
  arguments: &str,
  output: Option<&str>,
) {
  use ratatui::style::{Modifier, Style};

  if let Some(out) = output {
    let layout = ratatui::layout::Layout::default()
      .direction(ratatui::layout::Direction::Vertical)
      .constraints([
        ratatui::layout::Constraint::Length(1),
        ratatui::layout::Constraint::Min(0),
      ])
      .split(area);

    // Render title line
    let title_text = Text::from(vec![Line::from(vec![
      Span::styled(
        "â€¢ ",
        Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
      ),
      Span::styled("Used ", Style::default().fg(TEXT_COLOR)),
      Span::styled(name, Style::default().fg(BLUE).add_modifier(Modifier::BOLD)),
      Span::styled("(", Style::default().fg(TEXT_COLOR)),
      Span::styled(arguments, Style::default().fg(HIGHLIGHT_COLOR)),
      Span::styled(")", Style::default().fg(TEXT_COLOR)),
    ])]);
    Paragraph::new(title_text).render(layout[0], buf);

    // Render diff panel
    render_diff_panel(buf, layout[1], out);
  } else {
    let text = Text::from(vec![Line::from(vec![
      Span::styled(
        "â€¢ ",
        Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
      ),
      Span::styled("Used ", Style::default().fg(TEXT_COLOR)),
      Span::styled(name, Style::default().fg(BLUE).add_modifier(Modifier::BOLD)),
      Span::styled("(", Style::default().fg(TEXT_COLOR)),
      Span::styled(arguments, Style::default().fg(HIGHLIGHT_COLOR)),
      Span::styled(")", Style::default().fg(TEXT_COLOR)),
    ])]);
    let widget = Paragraph::new(text);
    widget.render(area, buf);
  }
}

/// Render a system notification message.
pub fn render_system_message(buf: &mut ratatui::buffer::Buffer, area: Rect, content: &str, level: SystemMessageLevel) {
  use ratatui::style::{Modifier, Style};
  let color = match level {
    SystemMessageLevel::Info => HIGHLIGHT_COLOR,
    SystemMessageLevel::Warning => WARNING,
    SystemMessageLevel::Error => CRITICAL,
  };
  let style = Style::default().fg(color).add_modifier(Modifier::BOLD);
  let mut lines: Vec<Line> = Vec::new();
  let safe_content = crate::utils::string::sanitize_for_tui(content);
  for raw in safe_content.lines() {
    for wrapped in wrap_text(raw, area.width) {
      lines.push(Line::from(vec![Span::styled(wrapped, style)]));
    }
  }
  if lines.is_empty() {
    lines.push(Line::from(Span::styled(content, style)));
  }
  // A single short notice reads better centred; multi-line output (quota,
  // usage, model lists) must stay left aligned so columns line up.
  let alignment = if lines.len() == 1 {
    ratatui::layout::Alignment::Center
  } else {
    ratatui::layout::Alignment::Left
  };
  let widget = Paragraph::new(Text::from(lines)).alignment(alignment);
  widget.render(area, buf);
}
