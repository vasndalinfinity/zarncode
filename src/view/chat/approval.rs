//! ApprovalPanel - rendering for tool call approval prompts.

use ratatui::{
  Frame,
  layout::Rect,
  text::{Line, Span, Text},
  widgets::Paragraph,
};

use crate::cli::app::PendingApproval;
use crate::utils::colors::{BLUE, CRITICAL, GREEN, MUTED, TEXT as TEXT_COLOR, WARNING};
use crate::view::diff::{diff_preview_compact_height, render_diff_preview_compact};

/// Approval panel component for rendering tool call approval UI.
pub struct ApprovalPanel;

impl ApprovalPanel {
  /// Calculate the height needed for the approval panel.
  pub fn height(approval: &PendingApproval) -> u16 {
    let diff_lines = approval
      .diff_preview
      .as_ref()
      .map(|d| diff_preview_compact_height(d))
      .unwrap_or(0);
    // four option rows plus a border
    (6 + diff_lines).min(24) as u16
  }

  /// Render the approval card.
  pub fn render(f: &mut Frame, area: Rect, approval: &PendingApproval) {
    use ratatui::style::{Modifier, Style};
    use ratatui::widgets::{Block, Borders, Padding, Widget};

    if area.height == 0 {
      return;
    }

    let block = Block::default()
      .borders(Borders::ALL)
      .border_style(Style::default().fg(WARNING))
      .title(Line::from(vec![
        Span::styled(
          " Tool approval ",
          Style::default().fg(WARNING).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
          format!(
            "{} ({}/{}) ",
            approval.name, approval.position, approval.total
          ),
          Style::default().fg(TEXT_COLOR),
        ),
      ]))
      .padding(Padding::new(1, 1, 0, 0));
    let inner = block.inner(area);
    block.render(area, f.buffer_mut());
    if inner.height == 0 {
      return;
    }

    let options = vec![
      Line::from(vec![
        Span::styled(
          "Allow once",
          Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
        ),
        Span::styled("        Enter / y", Style::default().fg(MUTED)),
      ]),
      Line::from(vec![
        Span::styled(
          format!("Don't ask again for {}", approval.name),
          Style::default().fg(BLUE).add_modifier(Modifier::BOLD),
        ),
        Span::styled("   a", Style::default().fg(MUTED)),
      ]),
      Line::from(vec![
        Span::styled(
          "Allow every tool this session",
          Style::default().fg(BLUE).add_modifier(Modifier::BOLD),
        ),
        Span::styled("   A", Style::default().fg(MUTED)),
      ]),
      Line::from(vec![
        Span::styled(
          "Deny",
          Style::default().fg(CRITICAL).add_modifier(Modifier::BOLD),
        ),
        Span::styled("                     n / Esc", Style::default().fg(MUTED)),
        Span::styled(
          "    Deny and stop",
          Style::default().fg(CRITICAL).add_modifier(Modifier::BOLD),
        ),
        Span::styled("  q", Style::default().fg(MUTED)),
      ]),
    ];

    let options_height = (options.len() as u16).min(inner.height);
    let (options_area, preview_area) = if approval.diff_preview.is_some() {
      (
        Rect::new(inner.x, inner.y, inner.width, options_height),
        Rect::new(
          inner.x,
          inner.y + options_height,
          inner.width,
          inner.height.saturating_sub(options_height),
        ),
      )
    } else {
      (inner, Rect::new(inner.x, inner.y, inner.width, 0))
    };

    Paragraph::new(Text::from(options)).render(options_area, f.buffer_mut());

    if let Some(ref diff) = approval.diff_preview
      && preview_area.height > 0
    {
      render_diff_preview_compact(f.buffer_mut(), preview_area, diff);
    }
  }
}
