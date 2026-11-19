//! Session picker overlay for `/resume`.
//!
//! Lists persisted conversations newest first; Enter asks the app to swap the
//! running session, Escape returns to the chat untouched.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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
use crate::session::SessionStore;
use crate::utils::colors::{CRITICAL, HIGHLIGHT, MUTED, PRIMARY, SUBTLE, TEXT};
use crate::view::{ChatView, View};

struct Entry {
  id: String,
  title: String,
  updated: String,
  messages: usize,
}

/// Modal list of saved chats.
pub struct SessionPickerView {
  session_handle: SessionHandle,
  runtime: std::sync::Arc<Runtime>,
  entries: Vec<Entry>,
  selected: usize,
  scroll: usize,
  status: Option<String>,
  /// Ctrl+D asked to delete the highlighted chat; Enter confirms, Esc cancels.
  confirm_delete: bool,
  current_id: String,
}

impl SessionPickerView {
  pub fn new(session_handle: SessionHandle, runtime: std::sync::Arc<Runtime>) -> Self {
    let current_id = session_handle.id.clone();
    let store = SessionStore::new(&runtime.data_dir());
    let mut entries: Vec<Entry> = Vec::new();
    let mut status = None;
    match store.list() {
      Ok(sessions) => {
        let mut ordered: Vec<_> = sessions.into_iter().collect();
        ordered.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        for meta in ordered {
          let messages = store.load(&meta.id).map(|(_, m)| m.len()).unwrap_or(0);
          entries.push(Entry {
            id: meta.id.clone(),
            title: if meta.title.trim().is_empty() {
              "(untitled)".to_string()
            } else {
              meta.title.clone()
            },
            updated: meta.updated_at.format("%Y-%m-%d %H:%M").to_string(),
            messages,
          });
        }
      }
      Err(error) => status = Some(format!("Could not read sessions: {error}")),
    }
    let selected = entries
      .iter()
      .position(|entry| entry.id == current_id)
      .unwrap_or(0);
    Self {
      session_handle,
      runtime,
      entries,
      selected,
      scroll: 0,
      status,
      confirm_delete: false,
      current_id,
    }
  }

  fn back(&self, data: &AppData) -> Option<Box<dyn View>> {
    Some(Box::new(ChatView::new(
      data,
      self.session_handle.clone(),
      self.runtime.clone(),
    )))
  }
}

impl View for SessionPickerView {
  fn handle_key(&mut self, data: &mut AppData, key: KeyEvent) -> Option<Box<dyn View>> {
    if self.confirm_delete {
      match key.code {
        KeyCode::Enter => {
          if let Some(entry) = self.entries.get(self.selected) {
            let store = SessionStore::new(&self.runtime.data_dir());
            match store.delete(&entry.id) {
              Ok(()) => {
                self.entries.remove(self.selected);
                if self.selected >= self.entries.len() {
                  self.selected = self.entries.len().saturating_sub(1);
                }
                self.status = Some("Chat deleted.".to_string());
              }
              Err(error) => self.status = Some(format!("Could not delete: {error}")),
            }
          }
          self.confirm_delete = false;
        }
        KeyCode::Esc => self.confirm_delete = false,
        _ => {}
      }
      return None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('d') {
      // Deleting the session you are currently inside would leave the actor
      // writing to a file that no longer exists.
      match self.entries.get(self.selected) {
        Some(entry) if entry.id == self.current_id => {
          self.status = Some("This is the chat you are in; switch away first.".to_string());
        }
        Some(_) => self.confirm_delete = true,
        None => {}
      }
      return None;
    }
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
          data.pending_resume = Some(entry.id.clone());
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
      .title(" Resume a chat ")
      .title_alignment(Alignment::Center)
      .border_style(Style::default().fg(PRIMARY));
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    if inner.height == 0 || inner.width == 0 {
      return;
    }

    let rows = usize::from(inner.height.saturating_sub(4)).max(3);
    if self.selected < self.scroll {
      self.scroll = self.selected;
    } else if self.selected >= self.scroll + rows {
      self.scroll = self.selected + 1 - rows;
    }

    let total = self.entries.len();
    let mut lines: Vec<Line> = Vec::new();
    if total == 0 {
      lines.push(Line::from(Span::styled(
        self
          .status
          .clone()
          .unwrap_or_else(|| "No saved chats yet.".to_string()),
        Style::default().fg(MUTED),
      )));
    }
    for (index, entry) in self.entries.iter().enumerate().skip(self.scroll).take(rows) {
      let is_selected = index == self.selected;
      let row_style = if is_selected {
        Style::default().fg(HIGHLIGHT).add_modifier(Modifier::BOLD)
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
        Span::styled(format!("{:<44}", truncate(&entry.title, 44)), row_style),
        Span::styled(format!("{}  ", entry.updated), muted),
        Span::styled(format!("{} msg", entry.messages), muted),
      ]));
    }

    let detail = self
      .entries
      .get(self.selected)
      .map(|entry| {
        vec![
          Line::from(vec![
            Span::styled("Session  ", Style::default().fg(SUBTLE)),
            Span::styled(entry.id.clone(), Style::default().fg(MUTED)),
          ]),
          Line::from(vec![
            Span::styled("Updated  ", Style::default().fg(SUBTLE)),
            Span::styled(entry.updated.clone(), Style::default().fg(MUTED)),
          ]),
        ]
      })
      .unwrap_or_default();

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
    if let Some(status) = self.status.as_ref()
      && !self.confirm_delete
    {
      let status_area = Rect::new(inner.x, inner.y + inner.height.saturating_sub(1), inner.width, 1);
      f.render_widget(
        Paragraph::new(Span::styled(status.clone(), Style::default().fg(SUBTLE)))
          .alignment(Alignment::Center),
        status_area,
      );
    }
    let footer = if self.confirm_delete {
      let target = self
        .entries
        .get(self.selected)
        .map(|entry| entry.title.clone())
        .unwrap_or_default();
      format!("Delete \"{target}\"?   Enter deletes   Esc cancels")
    } else {
      "Enter resume   up/down move   ctrl+d delete   esc cancel".to_string()
    };
    let footer_style = if self.confirm_delete {
      Style::default().fg(CRITICAL)
    } else {
      Style::default().fg(MUTED)
    };
    f.render_widget(
      Paragraph::new(Span::styled(footer, footer_style)).alignment(Alignment::Center),
      layout[3],
    );
  }
}

fn truncate(value: &str, max: usize) -> String {
  if value.chars().count() <= max {
    return value.to_string();
  }
  let mut out: String = value.chars().take(max.saturating_sub(1)).collect();
  out.push('…');
  out
}
