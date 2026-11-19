//! Shared terminal shell for live sessions and the offline preview.

use ratatui::{
  Frame,
  layout::{Constraint, Layout, Margin, Rect},
  style::{Modifier, Style},
  text::{Line, Span},
  widgets::{Block, BorderType, Padding, Paragraph},
};

use crate::utils::{
  char_display_width,
  colors::{BACKGROUND, MUTED, PRIMARY, SUBTLE, SURFACE, TEXT},
  string_display_width,
};

pub struct ChatLayout {
  pub header: Rect,
  pub transcript: Rect,
  pub composer: Rect,
  pub completion: Rect,
  pub footer: Rect,
}

/// Long lines are hard to scan. The app keeps a readable column and centres
/// it instead of stretching every row across the whole terminal.
pub const MAX_CONTENT_WIDTH: u16 = 118;

fn content_column(area: Rect) -> Rect {
  if area.width <= MAX_CONTENT_WIDTH {
    return area;
  }
  let width = MAX_CONTENT_WIDTH;
  Rect {
    x: area.x + (area.width - width) / 2,
    y: area.y,
    width,
    height: area.height,
  }
}

pub fn layout(area: Rect, input: &str, completion: u16) -> ChatLayout {
  let column = content_column(area);
  let area = column.inner(Margin::new(if column.width >= 40 { 2 } else { 0 }, 0));
  let rows = compose(input, input.chars().count(), area.width.saturating_sub(4))
    .0
    .len()
    .clamp(1, 8) as u16;
  let parts = Layout::vertical([
    Constraint::Length(if area.height >= 14 { 2 } else { 1 }),
    Constraint::Min(0),
    Constraint::Length(rows + 2),
    Constraint::Length(completion.min(6)),
    Constraint::Length(crate::view::STATUS_BAR_HEIGHT),
  ])
  .split(area);
  ChatLayout {
    header: parts[0],
    transcript: parts[1],
    composer: parts[2],
    completion: parts[3],
    footer: parts[4],
  }
}

pub fn background(frame: &mut Frame) {
  frame.render_widget(
    Block::default().style(Style::default().bg(BACKGROUND).fg(TEXT)),
    frame.area(),
  );
}

/// A dim rule that separates a band from the one above it.
pub fn rule(frame: &mut Frame, area: Rect) {
  if area.width == 0 || area.height == 0 {
    return;
  }
  let line = Line::from(vec![Span::styled(
    "─".repeat(usize::from(area.width)),
    Style::default().fg(SUBTLE),
  )]);
  frame.render_widget(Paragraph::new(line), area);
}

pub fn header(frame: &mut Frame, area: Rect, workspace: &str, preview: bool) {
  if area.height == 0 {
    return;
  }
  let chunks = Layout::horizontal([
    Constraint::Min(0),
    Constraint::Length(if preview { 18 } else { 10 }),
  ])
  .split(area);
  let line = Line::from(vec![
    Span::styled(
      "zarncode",
      Style::default().fg(PRIMARY).add_modifier(Modifier::BOLD),
    ),
    Span::styled("  /  ", Style::default().fg(SUBTLE)),
    Span::styled(workspace, Style::default().fg(TEXT)),
  ]);
  frame.render_widget(Paragraph::new(line), chunks[0]);
  let badge = if preview {
    "OFFLINE PREVIEW".to_string()
  } else {
    format!("v{}", env!("CARGO_PKG_VERSION"))
  };
  frame.render_widget(
    Paragraph::new(badge)
      .right_aligned()
      .style(Style::default().fg(MUTED)),
    chunks[1],
  );
  if area.height > 1 {
    rule(frame, Rect::new(area.x, area.y + 1, area.width, 1));
  }
}

pub fn welcome(frame: &mut Frame, area: Rect, preview: bool) {
  if area.height < 6 || area.width < 24 {
    return;
  }
  let width = area.width.min(74);

  let accent = Style::default().fg(PRIMARY);
  let text = Style::default().fg(TEXT);
  let muted = Style::default().fg(MUTED);
  let mut lines = vec![
    Line::from(vec![
      Span::styled("▀▀▀▀▀▀▀▀▀▀  ", accent),
      Span::styled("Zarncode", text.add_modifier(Modifier::BOLD)),
    ]),
    Line::from(Span::styled(
      "Your workspace. Your models.",
      muted,
    )),
    Line::from(Span::styled("Code with intent.", muted)),
    Line::default(),
    Line::from(Span::styled(
      "What will you build today?",
      text.add_modifier(Modifier::BOLD),
    )),
    Line::from(Span::styled(
      "Explore a codebase. Ship a change. Fix what's broken.",
      muted,
    )),
    Line::default(),
  ];
  if preview {
    lines.push(Line::from(Span::styled(
      "No provider connected. This is a local UI preview.",
      muted,
    )));
    lines.push(Line::from(Span::styled(
      "Type a message to try it. Escape exits. No tools execute.",
      Style::default().fg(SUBTLE),
    )));
  } else {
    lines.push(Line::from(vec![
      Span::styled("/plan", accent),
      Span::styled("  think it through     ", muted),
      Span::styled("/task", accent),
      Span::styled("  follow your agents", muted),
    ]));
    lines.push(Line::from(Span::styled(
      "enter send  ·  shift+enter newline  ·  / commands",
      Style::default().fg(SUBTLE),
    )));
  }

  // The card is exactly as tall as its content: borders plus one row of
  // breathing room on top and bottom.
  let height = (lines.len() as u16 + 4).min(area.height);
  let content = Rect::new(
    area.x + (area.width - width) / 2,
    area.y + (area.height.saturating_sub(height)) / 2,
    width,
    height,
  );

  let block = Block::default()
    .borders(ratatui::widgets::Borders::ALL)
    .border_type(BorderType::Rounded)
    .border_style(Style::default().fg(SUBTLE))
    .style(Style::default().bg(BACKGROUND))
    .padding(Padding::new(2, 2, 1, 1));
  let inner = block.inner(content);
  frame.render_widget(block, content);
  if inner.width == 0 || inner.height == 0 {
    return;
  }
  frame.render_widget(Paragraph::new(lines), inner);
}

/// Character-cell wrapping shared by input sizing, drawing, and cursor positioning.
fn compose(input: &str, cursor: usize, width: u16) -> (Vec<String>, (usize, usize)) {
  let width = usize::from(width.max(1));
  let prefix = if width >= 2 { "› " } else { "" };
  let text = format!("{prefix}{input}");
  let target = string_display_width(prefix) + cursor.min(input.chars().count());
  let mut lines = vec![String::new()];
  let mut column = 0;
  let mut position = (0, 0);
  for (index, c) in text.chars().enumerate() {
    let cell_width = char_display_width(c);
    if c != '\n' && column + cell_width > width {
      lines.push(String::new());
      column = 0;
    }
    if index == target {
      position = (lines.len() - 1, column);
    }
    if c == '\n' {
      lines.push(String::new());
      column = 0;
    } else {
      lines.last_mut().unwrap().push(c);
      column += cell_width;
    }
  }
  if target == text.chars().count() {
    if column >= width {
      lines.push(String::new());
      column = 0;
    }
    position = (lines.len() - 1, column);
  }
  (lines, position)
}

pub fn composer(frame: &mut Frame, area: Rect, input: &str, cursor: usize, focused: bool) {
  if area.width == 0 || area.height == 0 {
    return;
  }
  let block = Block::default()
    .borders(ratatui::widgets::Borders::ALL)
    .border_type(BorderType::Rounded)
    .border_style(Style::default().fg(if focused { PRIMARY } else { SUBTLE }))
    .style(Style::default().bg(SURFACE).fg(TEXT))
    .padding(Padding::new(1, 1, 0, 0));
  let inner = block.inner(area);
  frame.render_widget(block, area);
  if inner.width == 0 || inner.height == 0 {
    return;
  }
  let (lines, (row, col)) = compose(input, cursor, inner.width);
  let offset = row.saturating_sub(usize::from(inner.height).saturating_sub(1));
  let lines: Vec<Line> = lines.into_iter().skip(offset).map(Line::from).collect();
  if input.is_empty() {
    frame.render_widget(
      Paragraph::new(Line::from(vec![
        Span::styled("› ", Style::default().fg(PRIMARY)),
        Span::styled("Ask anything about your code...", Style::default().fg(MUTED)),
      ])),
      inner,
    );
  } else {
    frame.render_widget(
      Paragraph::new(lines).style(Style::default().fg(TEXT)),
      inner,
    );
  }
  if focused {
    frame.set_cursor_position((
      inner.x + (col as u16).min(inner.width - 1),
      inner.y + ((row - offset) as u16).min(inner.height - 1),
    ));
  }
}

/// Reproducible color export of real terminal cells, used only by manual render tests.
#[cfg(test)]
pub(crate) fn export_snapshot_svg(path: &str, draw: impl FnOnce(&mut Frame)) {
  use ratatui::{Terminal, backend::TestBackend, style::Color};
  use std::fmt::Write;
  fn color(value: Color) -> String {
    match value {
      Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
      _ => "#e5e2dc".into(),
    }
  }
  let mut terminal = Terminal::new(TestBackend::new(100, 32)).unwrap();
  terminal.draw(draw).unwrap();
  let buffer = terminal.backend().buffer();
  let mut svg = String::from(
    "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1040\" height=\"680\" viewBox=\"0 0 1040 680\"><rect width=\"1040\" height=\"680\" fill=\"#17171c\"/><g font-family=\"Consolas, monospace\" font-size=\"16\">",
  );
  for y in 0..32 {
    for x in 0..100 {
      let cell = &buffer[(x, y)];
      let px = 20 + x * 10;
      let py = 20 + y * 20;
      if let Color::Rgb(..) = cell.bg {
        write!(
          svg,
          "<rect x=\"{px}\" y=\"{py}\" width=\"10\" height=\"20\" fill=\"{}\"/>",
          color(cell.bg)
        )
        .unwrap();
      }
      let text = cell
        .symbol()
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
      if !text.trim().is_empty() {
        write!(
          svg,
          "<text x=\"{px}\" y=\"{}\" fill=\"{}\"{}>{text}</text>",
          py + 15,
          color(cell.fg),
          if cell.modifier.contains(Modifier::BOLD) {
            " font-weight=\"bold\""
          } else {
            ""
          }
        )
        .unwrap();
      }
    }
  }
  svg.push_str("</g></svg>");
  std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap()).unwrap();
  std::fs::write(path, svg).unwrap();
}

#[cfg(test)]
mod tests {
  use super::*;
  use ratatui::{Terminal, backend::TestBackend};
  #[test]
  fn unicode_cursor_and_wrapping_share_cell_geometry() {
    let (lines, cursor) = compose("ab世界\nhello", 10, 8);
    assert_eq!(lines, vec!["› ab世界", "hello"]);
    assert_eq!(cursor, (1, 5));
    assert_eq!(compose("abcdef", 6, 8).1, (1, 0));
  }
  #[test]
  fn shell_handles_small_screens_and_long_input() {
    for (w, h) in [(1, 1), (20, 6), (40, 12), (100, 30)] {
      let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
      terminal
        .draw(|f| {
          background(f);
          let areas = layout(f.area(), &"界".repeat(300), 4);
          header(f, areas.header, "workspace", false);
          welcome(f, areas.transcript, false);
          composer(f, areas.composer, &"界".repeat(300), 300, true);
        })
        .unwrap();
    }
  }
}
