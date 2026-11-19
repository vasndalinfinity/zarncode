//! Offline preview using the production message and status-bar renderers.

use std::io::{self, IsTerminal};

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use ratatui::{Frame, Terminal, backend::TestBackend};

use crate::tui::{Tui, TuiEvent, init_terminal, restore_terminal};
use crate::view::chat::ChatDisplayState;
use crate::view::status_bar::{CompactionWarningLevel, StatusBarInfo, render_status_bar};

const INTRO: &str = "This is the real terminal renderer in offline preview mode.
No provider connected. No API requests, tools, or files are used.
Type a message and press Enter to try the input. Escape exits.";

struct Preview {
  input: String,
  prompt: String,
  response: String,
}

impl Default for Preview {
  fn default() -> Self {
    Self {
      input: String::new(),
      prompt: String::new(),
      response: INTRO.to_string(),
    }
  }
}

impl Preview {
  fn handle_key(&mut self, key: KeyEvent) -> bool {
    if key.kind == KeyEventKind::Release {
      return false;
    }
    match key.code {
      KeyCode::Esc => return true,
      KeyCode::Char('c' | 'd') if key.modifiers.contains(KeyModifiers::CONTROL) => return true,
      KeyCode::Enter => {
        if matches!(self.input.trim(), "/quit" | "/exit") {
          return true;
        }
        if !self.input.trim().is_empty() {
          self.prompt = std::mem::take(&mut self.input);
          self.response =
            "Preview input received. This is a local demonstration, not an AI response.
Run zarncode without --preview with a configured provider for a live session."
              .to_string();
        }
      }
      KeyCode::Backspace => {
        self.input.pop();
      }
      KeyCode::Char(c)
        if !key
          .modifiers
          .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
      {
        self.input.push(c);
      }
      _ => {}
    }
    false
  }

  fn draw(&self, frame: &mut Frame) {
    use crate::view::chrome;
    chrome::background(frame);
    let layout = chrome::layout(frame.area(), &self.input, 0);
    chrome::header(frame, layout.header, "terminal preview", true);
    if self.prompt.is_empty() {
      chrome::welcome(frame, layout.transcript, true);
    } else {
      let user = crate::view::chat::ChatMessage::User {
        content: self.prompt.clone(),
      };
      let assistant = crate::view::chat::ChatMessage::Assistant {
        content: self.response.clone(),
        thinking_content: None,
      };
      crate::view::chat::messages::render_history(frame.buffer_mut(), layout.transcript, &[&user, &assistant]);
    }
    chrome::composer(
      frame,
      layout.composer,
      &self.input,
      self.input.chars().count(),
      true,
    );
    render_status_bar(
      frame,
      layout.footer,
      &StatusBarInfo {
        max_steps: crate::llm::session::actor::DEFAULT_MAX_STEPS_PER_TURN,
        agent_name: None,
        usage: crate::usage::UsageSnapshot::default(),
        session_id: "preview".to_string(),
        model_name: "No provider connected".to_string(),
        state: ChatDisplayState::WaitingInput,
        token_count: 0,
        max_context_size: 0,
        history_active: false,
        compaction_warning: CompactionWarningLevel::None,
        plan_mode: false,
        quota: None,
      },
    );
  }
}

fn snapshot() -> Result<String> {
  let mut terminal = Terminal::new(TestBackend::new(100, 30))?;
  terminal.draw(|frame| Preview::default().draw(frame))?;
  let buffer = terminal.backend().buffer();
  let mut output = String::new();
  for y in 0..buffer.area.height {
    let line: String = (0..buffer.area.width)
      .map(|x| buffer[(x, y)].symbol())
      .collect();
    output.push_str(line.trim_end());
    output.push('\n');
  }
  Ok(output)
}

struct RestoreTerminal;

impl Drop for RestoreTerminal {
  fn drop(&mut self) {
    let _ = restore_terminal();
  }
}

pub async fn run() -> Result<()> {
  if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
    print!("{}", snapshot()?);
    return Ok(());
  }
  let _restore = RestoreTerminal;
  init_terminal()?;
  let mut tui = Tui::new()?;
  let mut events = tui.create_event_stream();
  let mut preview = Preview::default();
  tui.draw(|frame| preview.draw(frame))?;
  while let Some(event) = events.next().await {
    match event {
      TuiEvent::Key(key) if preview.handle_key(key) => break,
      TuiEvent::Paste(text) => preview
        .input
        .extend(text.chars().filter(|c| !c.is_control())),
      _ => {}
    }
    tui.draw(|frame| preview.draw(frame))?;
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn preview_accepts_input_without_executing_it() {
    let mut preview = Preview::default();
    for c in "hello 世界".chars() {
      assert!(!preview.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)));
    }
    assert!(!preview.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
    assert_eq!(preview.prompt, "hello 世界");
    assert!(preview.input.is_empty());
    assert!(preview.response.contains("not an AI response"));
    assert!(preview.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
  }

  #[test]
  fn preview_handles_small_terminals() {
    for (width, height) in [(20, 6), (1, 1), (80, 24)] {
      let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
      terminal
        .draw(|frame| Preview::default().draw(frame))
        .unwrap();
    }
  }

  /// Manual visual artifact export; not part of normal tests or CLI startup.
  #[test]
  #[ignore]
  fn export_visual_preview() {
    use ratatui::style::Color;
    use std::fmt::Write;
    fn color(c: Color) -> String {
      match c {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        _ => "#e5e2dc".to_string(),
      }
    }
    std::fs::create_dir_all("artifacts").unwrap();
    for conversation in [false, true] {
      let mut preview = Preview::default();
      if conversation {
        preview.prompt = "Help me understand this codebase.".into();
        preview.response = "This is Zarncode's conversation layout.\n\nUser requests sit on a quiet surface. Responses stay readable,\nand your input remains anchored below the conversation.\n\nLocal UI demonstration only. No inference or tools have run.".into();
      }
      let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
      terminal.draw(|f| preview.draw(f)).unwrap();
      let buffer = terminal.backend().buffer();
      let mut svg = String::from(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1040\" height=\"640\" viewBox=\"0 0 1040 640\"><rect width=\"1040\" height=\"640\" fill=\"#17171c\"/><g font-family=\"Consolas, monospace\" font-size=\"16\">",
      );
      for y in 0..30 {
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
              if cell.modifier.contains(ratatui::style::Modifier::BOLD) {
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
      std::fs::write(
        if conversation {
          "artifacts/zarncode-conversation.svg"
        } else {
          "artifacts/zarncode-welcome.svg"
        },
        svg,
      )
      .unwrap();
    }
  }
}
