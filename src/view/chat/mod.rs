use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
  Frame,
  layout::{Constraint, Layout, Rect},
  text::{Line, Span, Text},
  widgets::Paragraph,
};

use crate::cli::AppData;
use crate::cli::runtime::Runtime;
use crate::cli::slash::{SLASH_COMMANDS, parse_slash_command};
use crate::history::InputHistoryManager;
use crate::llm::SessionHandle;
use crate::tui::{FrameRequester, TARGET_FRAME_INTERVAL};
use crate::utils::{HIGHLIGHT, MOON_FRAMES, PRIMARY, SPINNER_FRAMES};
use crate::view::chat::input::InputComponent;
use crate::view::{StatusBarInfo, View, render_status_bar};

pub mod approval;
pub mod input;
pub mod messages;
pub mod questions;

pub use messages::{
  ChatMessage, StreamingChunk, SystemMessageLevel, ToolCallStatus, llm_messages_to_chat_history,
};
/// Error when creating ChatView without a valid session
#[derive(Debug)]
#[allow(dead_code)]
pub struct NoSessionError;

/// Chat display state machine
///
/// State transitions:
/// - User submits message Ã¢â€ â€™ Animating (show moon animation)
/// - LLM starts responding with thinking content Ã¢â€ â€™ Thinking (show "Thinking...")
/// - LLM starts responding with normal content Ã¢â€ â€™ Streaming (show streaming content)
/// - Response completed Ã¢â€ â€™ WaitingInput (show spinner waiting for input)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatDisplayState {
  /// Waiting for LLM to start responding: show moon animation
  Animating,
  /// LLM is streaming thinking/reasoning content: show "Thinking..."
  Thinking,
  /// LLM is streaming response: show streaming content
  Streaming,
  /// Waiting for user input: show bottom spinner
  WaitingInput,
}

/// Slash command completion state.
#[derive(Default)]
struct SlashCompletion {
  /// Whether the completion menu is active.
  active: bool,
  /// Indices into [`SLASH_COMMANDS`] of matching commands.
  filtered: Vec<usize>,
  /// Index of the currently selected item in `filtered`.
  selected: usize,
}

/// Chat view state
pub struct ChatView {
  queue_composing: bool,
  /// Input component managing text, cursor, and history
  pub input: InputComponent,
  /// Prompt string (username@directory)
  pub prompt: String,
  /// Frame requester for scheduling animations
  frame_requester: Option<FrameRequester>,
  /// Animation state
  animation_enabled: bool,
  /// Last time the spinner was updated
  last_spinner_update: Instant,
  /// Current spinner frame index
  spinner_frame: usize,
  /// Last time the moon was updated
  last_moon_update: Instant,
  /// Current moon frame index
  moon_frame: usize,
  /// Current display state (state machine driven)
  state: ChatDisplayState,
  /// When the current turn started, so the wait can be shown as a timer.
  turn_started: Option<Instant>,
  /// Session handle for sending messages directly to LLM
  session_handle: SessionHandle,
  /// Status bar info (reused across draws)
  status_bar_info: StatusBarInfo,
  /// Runtime for accessing background manager and config
  runtime: Arc<Runtime>,
  /// Slash command completion state
  slash_completion: SlashCompletion,
  /// Previous Esc press, for the press-twice-to-exit guard.
  last_escape: Option<Instant>,
  /// Messages hidden from the bottom (PageUp scrolls back).
  scroll_offset: usize,
}

impl ChatView {
  /// Create a new chat view
  ///
  /// Initialize state machine based on AppData state:
  /// - Has streaming response Ã¢â€ â€™ Streaming state
  /// - Has user messages but waiting for AI response Ã¢â€ â€™ Animating state
  /// - Otherwise Ã¢â€ â€™ WaitingInput state
  ///
  /// # Arguments
  /// * `data` - Application data for determining initial state
  /// * `session_handle` - Handle to the chat session (must be valid)
  pub fn new(data: &AppData, session_handle: SessionHandle, runtime: Arc<Runtime>) -> Self {
    let prompt = Self::build_prompt();

    // Check if waiting for AI response (last message is from user)
    let waiting_for_ai = data
      .chat_history
      .last()
      .map(|msg| msg.is_user())
      .unwrap_or(false);

    // Determine initial state
    let state = if !data.streaming_response.is_empty() {
      // Check if currently streaming thinking content
      if data.streaming_response.iter().any(|c| c.is_thinking()) {
        ChatDisplayState::Thinking
      } else {
        ChatDisplayState::Streaming
      }
    } else if waiting_for_ai {
      ChatDisplayState::Animating
    } else {
      ChatDisplayState::WaitingInput
    };

    log::debug!("ChatView created with initial state: {:?}", state);

    // Initialize status bar info
    let status_bar_info = StatusBarInfo::from_app_data(data, &session_handle.id, state, &runtime);

    Self {
      queue_composing: false,
      input: InputComponent::new(InputHistoryManager::with_config(&runtime)),
      prompt,
      frame_requester: None,
      animation_enabled: true,
      last_spinner_update: Instant::now(),
      spinner_frame: 0,
      last_moon_update: Instant::now(),
      moon_frame: 0,
      state,
      turn_started: None,
      session_handle,
      status_bar_info,
      runtime,
      slash_completion: SlashCompletion::default(),
      last_escape: None,
      scroll_offset: 0,
    }
  }

  #[allow(dead_code)]
  /// Get current state
  pub fn state(&self) -> ChatDisplayState {
    self.state
  }

  /// State transition: enter Animating state
  fn enter_animating(&mut self) {
    let old_state = self.state;
    self.state = ChatDisplayState::Animating;
    self.turn_started = Some(Instant::now());
    log::debug!(
      "State transition: {:?} Ã¢â€ â€™ {:?}",
      old_state,
      self.state
    );
    // Reset moon animation frame
    self.moon_frame = 0;
    self.last_moon_update = Instant::now();
  }

  /// State transition: enter Thinking state
  fn enter_thinking(&mut self) {
    let old_state = self.state;
    self.state = ChatDisplayState::Thinking;
    log::debug!(
      "State transition: {:?} Ã¢â€ â€™ {:?}",
      old_state,
      self.state
    );
    // Reset spinner animation for "Thinking..." indicator
    self.spinner_frame = 0;
    self.last_spinner_update = Instant::now();
  }

  /// State transition: enter Streaming state
  fn enter_streaming(&mut self) {
    let old_state = self.state;
    self.state = ChatDisplayState::Streaming;
    log::debug!(
      "State transition: {:?} Ã¢â€ â€™ {:?}",
      old_state,
      self.state
    );
  }

  /// State transition: enter WaitingInput state
  fn enter_waiting_input(&mut self) {
    let old_state = self.state;
    self.state = ChatDisplayState::WaitingInput;
    self.turn_started = None;
    log::debug!(
      "State transition: {:?} Ã¢â€ â€™ {:?}",
      old_state,
      self.state
    );
  }

  /// Build the prompt string (username@current_dir)
  fn build_prompt() -> String {
    let username = env::var("USER")
      .or_else(|_| env::var("USERNAME"))
      .unwrap_or_else(|_| "user".to_string());

    let current_dir = env::current_dir()
      .ok()
      .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
      .unwrap_or_else(|| "~".to_string());

    format!("{}@{}", username, current_dir)
  }

  /// Get the current spinner character based on animation state
  fn current_spinner(&self) -> char {
    if self.animation_enabled {
      SPINNER_FRAMES[self.spinner_frame % SPINNER_FRAMES.len()]
    } else {
      '>'
    }
  }

  pub fn insert_char(&mut self, c: char) {
    self.input.insert_char(c);
  }

  /// Save current input to history
  fn save_to_history(&mut self) {
    self.input.save_to_history();
  }

  /// Enter steers active work; ordinary idle input starts a new task.
  pub fn submit_message(&mut self, data: &mut AppData) {
    if self.input.is_empty() {
      return;
    }
    if let Err(content) = data.input_queue.validate_new(self.input.text()) {
      data.chat_history.push(ChatMessage::System {
        content,
        level: SystemMessageLevel::Warning,
      });
      return;
    }
    let content = self.input.text().to_string();
    let busy = self.state != ChatDisplayState::WaitingInput
      || data.pending_approval.is_some()
      || data.pending_questions.is_some();
    let result = if busy {
      self
        .session_handle
        .queue_input(crate::llm::session::input_queue::InputKind::Steer, content)
        .map(|_| ())
    } else {
      self.session_handle.send_message_checked(content)
    };
    if let Err(content) = result {
      data.chat_history.push(ChatMessage::System {
        content,
        level: SystemMessageLevel::Error,
      });
      return;
    }
    self.save_to_history();
    self.input.take_text();
    self.input.move_cursor_home();
    self.queue_composing = false;
    if !busy {
      self.enter_animating();
    }
  }

  fn submit_follow_up(&mut self, data: &mut AppData) {
    if self.input.is_empty() {
      return;
    }
    if let Err(content) = data.input_queue.validate_new(self.input.text()) {
      data.chat_history.push(ChatMessage::System {
        content,
        level: SystemMessageLevel::Warning,
      });
      return;
    }
    let content = self.input.text().to_string();
    match self.session_handle.queue_input(
      crate::llm::session::input_queue::InputKind::FollowUp,
      content,
    ) {
      Ok(_) => {
        self.save_to_history();
        self.input.take_text();
        self.input.move_cursor_home();
        self.queue_composing = false;
      }
      Err(content) => data.chat_history.push(ChatMessage::System {
        content,
        level: SystemMessageLevel::Error,
      }),
    }
  }

  /// Handle a slash command intercepted before message submission.
  fn handle_slash_command(
    &mut self,
    data: &mut AppData,
    cmd: crate::cli::slash::SlashCommandCall,
  ) -> Option<Box<dyn View>> {
    match cmd.name.as_str() {
      "steer" | "followup" => {
        let kind = if cmd.name == "steer" {
          crate::llm::session::input_queue::InputKind::Steer
        } else {
          crate::llm::session::input_queue::InputKind::FollowUp
        };
        if cmd.args.trim().is_empty() {
          data.chat_history.push(ChatMessage::System {
            content: format!("Usage: /{} <message>", cmd.name),
            level: SystemMessageLevel::Warning,
          });
        } else if let Err(content) = self.session_handle.queue_input(kind, cmd.args) {
          data.chat_history.push(ChatMessage::System {
            content,
            level: SystemMessageLevel::Error,
          });
        }
        self.queue_composing = false;
        None
      }
      "queue" => {
        let mut parts = cmd.args.trim().splitn(3, ' ');
        match parts.next().unwrap_or("") {
          "" | "list" => self.session_handle.inspect_queue(),
          "pause" => self.session_handle.pause_queue(true),
          "resume" => self.session_handle.pause_queue(false),
          "drop" => {
            if let Some(id) = parts.next() {
              self.session_handle.remove_queued_input(id.to_string());
            }
          }
          "edit" => {
            if let (Some(id), Some(content)) = (parts.next(), parts.next()) {
              self
                .session_handle
                .edit_queued_input(id.to_string(), content.to_string());
            }
          }
          _ => data.chat_history.push(ChatMessage::System {
            content: "Usage: /queue [list|pause|resume|drop ID|edit ID message]".into(),
            level: SystemMessageLevel::Warning,
          }),
        }
        self.queue_composing = false;
        None
      }
      "usage" => {
        data.chat_history.push(ChatMessage::System {
          content: self.runtime.usage.snapshot().report(),
          level: crate::view::chat::messages::SystemMessageLevel::Info,
        });
        None
      }
      "account" => {
        data.chat_history.push(ChatMessage::System {
          content: self.account_report(),
          level: SystemMessageLevel::Info,
        });
        None
      }
      "exit" | "quit" => {
        data.should_exit = true;
        None
      }
      "model" => {
        let trimmed = cmd.args.trim();
        if trimmed.is_empty() {
          // No argument: open the picker so nobody has to guess model names.
          return Some(Box::new(crate::view::ModelPickerView::new(
            self.session_handle.clone(),
            self.runtime.clone(),
          )));
        }
        self.handle_model_command(data, trimmed);
        None
      }
      "thinking" => {
        let current = self.runtime.default_thinking();
        let enabled = match cmd.args.trim().to_ascii_lowercase().as_str() {
          "on" | "true" | "1" | "show" => true,
          "off" | "false" | "0" | "hide" => false,
          _ => !current,
        };
        self.runtime.set_thinking(enabled);
        data.chat_history.push(ChatMessage::System {
          content: if enabled {
            "Reasoning shown. The model thinks before it answers, so the first token can take a while.".to_string()
          } else {
            "Reasoning hidden. The model answers directly, which is much faster.".to_string()
          },
          level: SystemMessageLevel::Info,
        });
        None
      }
      "new" => {
        data.pending_new_session = true;
        None
      }
      "resume" => {
        return Some(Box::new(crate::view::SessionPickerView::new(
          self.session_handle.clone(),
          self.runtime.clone(),
        )));
      }
      "rename" => {
        self.handle_rename_command(data, cmd.args.trim());
        None
      }
      "help" => {
        let mut lines = vec!["Slash commands".to_string()];
        for command in SLASH_COMMANDS {
          lines.push(format!("/{:<9} {}", command.name, command.description));
        }
        data.chat_history.push(ChatMessage::System {
          content: lines.join("\n"),
          level: crate::view::chat::messages::SystemMessageLevel::Info,
        });
        None
      }
      "login" => {
        self.handle_login_command(data, cmd.args.trim());
        None
      }
      "logout" => {
        let path = crate::account::cli_auth_path(&self.runtime.config_dir());
        let result = crate::account::remove_auth_file(&path);
        data.chat_history.push(ChatMessage::System {
          content: match result {
            Ok(()) => format!("Signed out. Removed {}.", path.display()),
            Err(error) => format!("Could not sign out: {error}"),
          },
          level: SystemMessageLevel::Info,
        });
        None
      }
      "quota" => {
        data.chat_history.push(ChatMessage::System {
          content: self.runtime.quota.snapshot().report(),
          level: crate::view::chat::messages::SystemMessageLevel::Info,
        });
        None
      }
      "task" => {
        if !cmd.args.is_empty() {
          data.chat_history.push(ChatMessage::System {
            content: "Usage: /task (opens the interactive task browser)".to_string(),
            level: crate::view::chat::messages::SystemMessageLevel::Warning,
          });
          return None;
        }
        Some(Box::new(crate::view::task_browser::TaskBrowserView::new(
          self.runtime.background_manager().clone(),
          self.session_handle.clone(),
          self.runtime.clone(),
        )))
      }
      "plan" => {
        self.session_handle.plan_slash_command(cmd.args.trim());
        None
      }
      "clear" | "reset" => {
        self.session_handle.clear_history();
        data.chat_history.clear();
        data.chat_history.push(ChatMessage::System {
          content: "Context cleared.".to_string(),
          level: crate::view::chat::messages::SystemMessageLevel::Info,
        });
        None
      }
      _ => {
        data.chat_history.push(ChatMessage::System {
          content: format!("Unknown slash command: /{}", cmd.name),
          level: crate::view::chat::messages::SystemMessageLevel::Error,
        });
        None
      }
    }
  }

  /// Get the current moon character
  fn current_moon(&self) -> char {
    MOON_FRAMES[self.moon_frame % MOON_FRAMES.len()]
  }

  fn update_slash_completion(&mut self) {
    let text = self.input.text();
    let cursor = self.input.cursor();

    // Must start with `/` and cursor must be after it.
    if !text.starts_with('/') || cursor == 0 {
      self.slash_completion.active = false;
      return;
    }

    // Collect characters between `/` and cursor.
    let prefix: String = text.chars().take(cursor).skip(1).collect();

    // If there's a space before cursor, the user has finished the command name.
    if prefix.contains(' ') {
      self.slash_completion.active = false;
      return;
    }

    let prefix_lower = prefix.to_lowercase();
    self.slash_completion.filtered = SLASH_COMMANDS
      .iter()
      .enumerate()
      .filter(|(_, cmd)| cmd.name.starts_with(&prefix_lower))
      .map(|(i, _)| i)
      .collect();

    self.slash_completion.selected = 0;
    self.slash_completion.active = !self.slash_completion.filtered.is_empty();
  }

  /// Accept the currently selected slash completion item.
  fn accept_slash_completion(&mut self) {
    if let Some(&idx) = self
      .slash_completion
      .filtered
      .get(self.slash_completion.selected)
    {
      let cmd = &SLASH_COMMANDS[idx];
      self.input.replace_text(format!("/{0} ", cmd.name));
      self.slash_completion.active = false;
    }
  }

  /// Account summary for `/account`: identity, tier, status, quota, relay.
  fn account_report(&self) -> String {
    let mut lines = vec!["Zarnkina account".to_string()];
    lines.push(format!(
      "Relay       {}",
      crate::account::relay_base(&self.runtime.config)
    ));
    let stale = self
      .runtime
      .account_state_at
      .lock()
      .ok()
      .and_then(|guard| guard.map(|at| at.elapsed().as_secs()))
      .map(|secs| format!(" ({}s ago)", secs))
      .unwrap_or_else(|| " (not fetched yet)".to_string());
    match self
      .runtime
      .account_state
      .lock()
      .ok()
      .and_then(|guard| guard.clone())
    {
      Some(state) => {
        lines.push(format!(
          "Email       {}",
          if state.email.is_empty() {
            "-"
          } else {
            &state.email
          }
        ));
        lines.push(format!(
          "Tier        {}",
          if state.pro { "pro" } else { "free" }
        ));
        lines.push(format!(
          "Access      {}{}",
          if state.allowed { "allowed" } else { "denied" },
          if state.status.is_empty() {
            String::new()
          } else {
            format!(" ({})", state.status)
          }
        ));
        if let Some(reason) = state.reason.as_ref().or(state.message.as_ref()) {
          lines.push(format!("Reason      {reason}"));
        }
        lines.push(format!("Models      {} in catalogue", state.models.len()));
      }
      None => lines.push(format!("Status      account check pending{stale}")),
    }
    lines.push(format!(
      "Credential  {}",
      if self.runtime.credentials.api_key.is_some() {
        "api key"
      } else if self.runtime.credentials.session_token.is_some() {
        "session token"
      } else {
        "none - run /login"
      }
    ));
    lines.push(String::new());
    lines.push(self.runtime.quota.snapshot().report());
    lines.join("\n")
  }

  /// `/rename <title>` - retitle the current saved chat.
  fn handle_rename_command(&mut self, data: &mut AppData, title: &str) {
    if title.is_empty() {
      data.chat_history.push(ChatMessage::System {
        content: "Usage: /rename <new title>".to_string(),
        level: SystemMessageLevel::Warning,
      });
      return;
    }
    let store = crate::session::SessionStore::new(&self.runtime.data_dir());
    let id = self.session_handle.id.clone();
    let result = store.load(&id).and_then(|(mut meta, _)| {
      meta.title = title.to_string();
      meta.updated_at = chrono::Local::now();
      store.update_meta(&meta)
    });
    data.chat_history.push(ChatMessage::System {
      content: match result {
        Ok(()) => format!("Renamed this chat to \"{title}\"."),
        Err(error) => format!("Could not rename the chat: {error}"),
      },
      level: SystemMessageLevel::Info,
    });
  }

  /// `/login` - start the browser flow in a separate window.
  ///
  /// Keys are not part of the product UI: people sign in with their account,
  /// and API keys stay an owner-side mechanism (`zarncode auth login --api-key`).
  fn handle_login_command(&mut self, data: &mut AppData, args: &str) {
    let message = if !args.trim().is_empty() {
      "Sign-in is account based. Run /login without arguments to open the browser flow.".to_string()
    } else {
      let binary = std::env::current_exe().unwrap_or_else(|_| "zarncode".into());
      let mut command = if cfg!(target_os = "windows") {
        let mut cmd = std::process::Command::new("cmd");
        cmd.args([
          "/C",
          "start",
          "",
          &binary.to_string_lossy(),
          "auth",
          "login",
        ]);
        cmd
      } else {
        let mut cmd = std::process::Command::new(&binary);
        cmd.args(["auth", "login"]);
        cmd
      };
      match command.spawn() {
        Ok(_) => "Opening browser sign-in in a new window.\nFinish there, then restart zarncode (Ctrl+C, relaunch) so the session is picked up.".to_string(),
        Err(error) => format!(
          "Could not open the sign-in window ({error}).\nRun this in a terminal:\n  zarncode auth login"
        ),
      }
    };
    data.chat_history.push(ChatMessage::System {
      content: message,
      level: SystemMessageLevel::Info,
    });
  }

  /// `/model` Ã¢â‚¬â€ list the configured models or switch to one.
  ///
  /// Accepts a list index, a config alias, or the relay model id, so a user can
  /// type whatever they see in `zarncode models`.
  fn handle_model_command(&mut self, data: &mut AppData, args: &str) {
    let mut aliases: Vec<String> = self.runtime.config.models.keys().cloned().collect();
    aliases.sort();
    if aliases.is_empty() {
      data.chat_history.push(ChatMessage::System {
        content: "No models are configured. Add [models.*] entries to config.toml.".to_string(),
        level: SystemMessageLevel::Warning,
      });
      return;
    }
    let current = self
      .runtime
      .active_model
      .lock()
      .ok()
      .and_then(|guard| guard.clone())
      .unwrap_or_else(|| self.runtime.default_model());

    if args.is_empty() {
      let mut lines = vec![format!("Models Ã¢â‚¬â€ current: {current}")];
      for (index, alias) in aliases.iter().enumerate() {
        let model_id = self
          .runtime
          .config
          .get_model(alias)
          .map(|entry| entry.model.clone())
          .unwrap_or_default();
        let marker = if alias == &current { "*" } else { " " };
        lines.push(format!(
          "{marker} {:>2}) {:<28} {}",
          index + 1,
          alias,
          model_id
        ));
      }
      lines.push("Switch with /model <number> or /model <alias>.".to_string());
      data.chat_history.push(ChatMessage::System {
        content: lines.join("\n"),
        level: SystemMessageLevel::Info,
      });
      return;
    }

    let wanted = args.trim();
    let resolved = wanted
      .parse::<usize>()
      .ok()
      .and_then(|index| index.checked_sub(1))
      .and_then(|index| aliases.get(index).cloned())
      .or_else(|| {
        aliases
          .iter()
          .find(|alias| alias.eq_ignore_ascii_case(wanted))
          .cloned()
      })
      .or_else(|| {
        self
          .runtime
          .config
          .models
          .iter()
          .find(|(_, entry)| entry.model.eq_ignore_ascii_case(wanted))
          .map(|(alias, _)| alias.clone())
      });

    match resolved {
      Some(alias) => {
        self.session_handle.switch_model(alias.clone());
        self.status_bar_info.model_name = alias.clone();
        data.chat_history.push(ChatMessage::System {
          content: format!("Model switched to {alias}."),
          level: SystemMessageLevel::Info,
        });
      }
      None => data.chat_history.push(ChatMessage::System {
        content: format!(
          "'{wanted}' is not a configured model. Run /model to list them, or `zarncode models` for the relay catalogue."
        ),
        level: SystemMessageLevel::Warning,
      }),
    }
  }

  /// Render the slash command completion menu.
  fn render_slash_completion(&self, f: &mut Frame, area: Rect) {
    if area.height == 0 || self.slash_completion.filtered.is_empty() {
      return;
    }

    use ratatui::style::Style;

    // Keep the menu inside the terminal: show a window of rows around the
    // selection instead of drawing past the bottom edge.
    let visible = area.height as usize;
    let selected = self.slash_completion.selected;
    let start = selected.saturating_sub(visible.saturating_sub(1));

    let items: Vec<Line> = self
      .slash_completion
      .filtered
      .iter()
      .enumerate()
      .skip(start)
      .take(visible)
      .map(|(i, &cmd_idx)| {
        let cmd = &SLASH_COMMANDS[cmd_idx];
        let is_selected = i == selected;
        let name_style = if is_selected { *HIGHLIGHT } else { *PRIMARY };
        let desc_style = if is_selected {
          *HIGHLIGHT
        } else {
          Style::default().fg(crate::utils::colors::MUTED)
        };

        Line::from(vec![
          Span::styled(format!("/{}", cmd.name), name_style),
          Span::raw("  "),
          Span::styled(cmd.description, desc_style),
        ])
      })
      .collect();

    let widget = Paragraph::new(Text::from(items));
    f.render_widget(widget, area);
  }

  /// Update status bar info with current state
  ///
  /// This method updates the mutable fields of status_bar_info.
  /// Static fields (session_id, model_name) are set once during construction.
  fn update_status_bar_info(&mut self, data: &AppData) {
    use crate::utils::token_counter::estimate_chat_messages_tokens;
    use crate::view::status_bar::calculate_compaction_warning_level;

    self.status_bar_info.state = self.state;
    self.status_bar_info.usage = self.runtime.usage.snapshot();
    if let Some(alias) = self
      .runtime
      .active_model
      .lock()
      .ok()
      .and_then(|guard| guard.clone())
    {
      if let Some(model) = self.runtime.config.get_model(&alias) {
        self.status_bar_info.max_context_size = model
          .max_context_size
          .unwrap_or(self.status_bar_info.max_context_size);
      }
      self.status_bar_info.model_name = alias;
    }
    // Use precise token count from API if available, otherwise estimate
    self.status_bar_info.token_count = data
      .precise_token_count
      .map(|t| t as usize)
      .unwrap_or_else(|| estimate_chat_messages_tokens(&data.chat_history));

    // Update compaction warning level from AppData
    self.status_bar_info.compaction_warning =
      calculate_compaction_warning_level(&data.compaction_warning, true);

    // Update plan mode indicator
    self.status_bar_info.plan_mode = data.plan_mode;
  }
}

impl View for ChatView {
  fn handle_mouse(
    &mut self,
    data: &mut AppData,
    mouse: crossterm::event::MouseEvent,
  ) -> bool {
    use crossterm::event::MouseEventKind;
    match mouse.kind {
      MouseEventKind::ScrollUp => {
        self.scroll_offset = self.scroll_offset.saturating_add(3);
        true
      }
      MouseEventKind::ScrollDown => {
        self.scroll_offset = self.scroll_offset.saturating_sub(3);
        true
      }
      _ => false,
    }
  }

  fn handle_key(&mut self, data: &mut AppData, key: KeyEvent) -> Option<Box<dyn View>> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
      if let Some(approval) = &data.pending_approval {
        self
          .runtime
          .approval_runtime()
          .respond(&approval.approval_id, false, true);
      }
      self.session_handle.cancel();
      data.pending_approval = None;
      data.pending_questions = None;
      self.queue_composing = false;
      self.enter_waiting_input();
      return None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('g') {
      self.queue_composing = !self.queue_composing;
      return None;
    }
    if self.queue_composing && key.code == KeyCode::Esc {
      self.queue_composing = false;
      return None;
    }
    if !self.queue_composing
      && (data.pending_approval.is_some() || data.pending_questions.is_some())
      && key.code == KeyCode::Enter
      && key.modifiers.contains(KeyModifiers::ALT)
    {
      return None;
    }
    // Handle pending tool call approval first
    if !self.queue_composing
      && let Some(ref approval) = data.pending_approval
    {
      match key.code {
        KeyCode::Char('y') | KeyCode::Enter => {
          self
            .runtime
            .approval_runtime()
            .respond(&approval.approval_id, true, false);
          data.pending_approval = None;
        }
        KeyCode::Char('n') | KeyCode::Esc => {
          self
            .runtime
            .approval_runtime()
            .respond(&approval.approval_id, false, false);
          data.pending_approval = None;
        }
        KeyCode::Char('a') => {
          // "Don't ask again": allow this tool for the rest of the session.
          let name = approval.name.clone();
          self
            .runtime
            .approval_runtime()
            .respond(&approval.approval_id, true, false);
          self.session_handle.allow_tool_for_session(name);
          data.pending_approval = None;
        }
        KeyCode::Char('A') | KeyCode::Char('Y') => {
          // Allow everything for the rest of this session.
          self.session_handle.enable_session_yolo();
          self
            .runtime
            .approval_runtime()
            .respond(&approval.approval_id, true, false);
          data.pending_approval = None;
        }
        KeyCode::Char('q') => {
          self
            .runtime
            .approval_runtime()
            .respond(&approval.approval_id, false, true);
          data.pending_approval = None;
        }
        _ => {}
      }
      return None;
    }

    // Handle pending structured questions
    if !self.queue_composing
      && let Some(ref mut questions) = data.pending_questions
    {
      // Check if current question is a confirmation dialog
      let is_confirmation = questions
        .questions
        .get(questions.current_question_idx)
        .map(|q| q.confirmation)
        .unwrap_or(false);

      match key.code {
        KeyCode::Char('q') | KeyCode::Esc => {
          self
            .session_handle
            .answer_questions(&questions.tool_call_id, Vec::new(), true);
          data.pending_questions = None;
        }
        KeyCode::Char('y') if is_confirmation => {
          let q_idx = questions.current_question_idx;
          while questions.answers.len() <= q_idx {
            questions.answers.push(Vec::new());
          }
          questions.answers[q_idx] = vec![0]; // Yes = index 0
          questions.current_question_idx += 1;
          questions.selected_option_idx = 0;
          if questions.current_question_idx >= questions.questions.len() {
            let answers = std::mem::take(&mut questions.answers);
            let tool_call_id = questions.tool_call_id.clone();
            self
              .session_handle
              .answer_questions(tool_call_id, answers, false);
            data.pending_questions = None;
          }
        }
        KeyCode::Char('n') if is_confirmation => {
          let q_idx = questions.current_question_idx;
          while questions.answers.len() <= q_idx {
            questions.answers.push(Vec::new());
          }
          questions.answers[q_idx] = vec![1]; // No = index 1
          questions.current_question_idx += 1;
          questions.selected_option_idx = 0;
          if questions.current_question_idx >= questions.questions.len() {
            let answers = std::mem::take(&mut questions.answers);
            let tool_call_id = questions.tool_call_id.clone();
            self
              .session_handle
              .answer_questions(tool_call_id, answers, false);
            data.pending_questions = None;
          }
        }
        KeyCode::Up => {
          if !is_confirmation && questions.selected_option_idx > 0 {
            questions.selected_option_idx -= 1;
          }
        }
        KeyCode::Down => {
          if !is_confirmation
            && let Some(q) = questions.questions.get(questions.current_question_idx)
            && questions.selected_option_idx + 1 < q.options.len()
          {
            questions.selected_option_idx += 1;
          }
        }
        KeyCode::Char(' ') => {
          if let Some(q) = questions.questions.get(questions.current_question_idx)
            && q.multi_select
          {
            let q_idx = questions.current_question_idx;
            while questions.answers.len() <= q_idx {
              questions.answers.push(Vec::new());
            }
            let selected = questions.selected_option_idx;
            let ans = &mut questions.answers[q_idx];
            if let Some(pos) = ans.iter().position(|&x| x == selected) {
              ans.remove(pos);
            } else {
              ans.push(selected);
            }
          }
        }
        KeyCode::Enter => {
          if let Some(q) = questions.questions.get(questions.current_question_idx) {
            let q_idx = questions.current_question_idx;
            while questions.answers.len() <= q_idx {
              questions.answers.push(Vec::new());
            }
            // Check required validation
            if q.required && questions.answers[q_idx].is_empty() {
              // Don't advance, keep the question open
              // (UI will show it's still pending)
              return None;
            }
            if !q.multi_select {
              questions.answers[q_idx] = vec![questions.selected_option_idx];
            }
            questions.current_question_idx += 1;
            questions.selected_option_idx = questions
              .questions
              .get(questions.current_question_idx)
              .and_then(|q| q.default.first().copied())
              .unwrap_or(0);
            if questions.current_question_idx >= questions.questions.len() {
              let answers = std::mem::take(&mut questions.answers);
              let tool_call_id = questions.tool_call_id.clone();
              self
                .session_handle
                .answer_questions(tool_call_id, answers, false);
              data.pending_questions = None;
            }
          }
        }
        KeyCode::Char(c) if c.is_ascii_digit() => {
          let digit = (c as usize).saturating_sub('1' as usize);
          if let Some(q) = questions.questions.get(questions.current_question_idx)
            && digit < q.options.len()
          {
            questions.selected_option_idx = digit;
            let q_idx = questions.current_question_idx;
            while questions.answers.len() <= q_idx {
              questions.answers.push(Vec::new());
            }
            if q.multi_select {
              let ans = &mut questions.answers[q_idx];
              if let Some(pos) = ans.iter().position(|&x| x == digit) {
                ans.remove(pos);
              } else {
                ans.push(digit);
              }
            } else {
              questions.answers[q_idx] = vec![digit];
              questions.current_question_idx += 1;
              questions.selected_option_idx = questions
                .questions
                .get(questions.current_question_idx)
                .and_then(|q| q.default.first().copied())
                .unwrap_or(0);
              if questions.current_question_idx >= questions.questions.len() {
                let answers = std::mem::take(&mut questions.answers);
                let tool_call_id = questions.tool_call_id.clone();
                self
                  .session_handle
                  .answer_questions(tool_call_id, answers, false);
                data.pending_questions = None;
              }
            }
          }
        }
        _ => {}
      }
      return None;
    }

    // Slash command completion navigation.
    if self.slash_completion.active {
      match key.code {
        KeyCode::Up => {
          if self.slash_completion.selected > 0 {
            self.slash_completion.selected -= 1;
          }
          return None;
        }
        KeyCode::Down => {
          if self.slash_completion.selected + 1 < self.slash_completion.filtered.len() {
            self.slash_completion.selected += 1;
          }
          return None;
        }
        KeyCode::Enter | KeyCode::Tab => {
          self.accept_slash_completion();
          return None;
        }
        KeyCode::Esc => {
          self.slash_completion.active = false;
          return None;
        }
        _ => {}
      }
    }

    match key.code {
      KeyCode::PageUp => {
        let max = data.chat_history.len().saturating_sub(1);
        self.scroll_offset = (self.scroll_offset + 10).min(max.saturating_mul(60));
      }
      KeyCode::PageDown => {
        self.scroll_offset = self.scroll_offset.saturating_sub(10);
      }
      KeyCode::End => {
        self.scroll_offset = 0;
      }
      KeyCode::Esc => {
        // Escape cancels work or clears the draft; leaving the app needs a
        // second press (or /exit, or Ctrl+C).
        if matches!(
          self.state,
          ChatDisplayState::Streaming | ChatDisplayState::Animating | ChatDisplayState::Thinking
        ) {
          self.session_handle.cancel();
          self.last_escape = None;
          return None;
        }
        if !self.input.text().is_empty() {
          self.input.take_text();
          self.input.move_cursor_home();
          self.last_escape = None;
          return None;
        }
        match self.last_escape.take() {
          Some(at) if at.elapsed() < std::time::Duration::from_secs(2) => {
            data.should_exit = true;
          }
          _ => {
            self.last_escape = Some(std::time::Instant::now());
            data.chat_history.push(ChatMessage::System {
              content: "Press Esc again to exit, or use /exit.".to_string(),
              level: SystemMessageLevel::Info,
            });
          }
        }
      }
      KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
        data.should_exit = true;
      }
      KeyCode::Enter if key.modifiers.contains(KeyModifiers::ALT) => {
        self.submit_follow_up(data);
      }
      KeyCode::Enter => {
        // Shift+Enter inserts a newline; Alt+Enter queues a follow-up.
        if key.modifiers.contains(KeyModifiers::SHIFT) {
          self.insert_char('\n');
          self.update_slash_completion();
        } else if !self.input.is_empty() {
          let text = self.input.text();
          if let Some(cmd) = parse_slash_command(text) {
            self.input.take_text();
            self.input.move_cursor_home();
            self.slash_completion.active = false;
            return self.handle_slash_command(data, cmd);
          }
          self.submit_message(data);
          self.slash_completion.active = false;
        }
      }
      KeyCode::Up => {
        self.input.navigate_up();
      }
      KeyCode::Down => {
        self.input.navigate_down();
      }
      KeyCode::Backspace => {
        self.input.backspace();
        self.update_slash_completion();
      }
      KeyCode::Delete => {
        self.input.delete();
        self.update_slash_completion();
      }
      KeyCode::Left => {
        self.input.move_cursor_left();
        self.update_slash_completion();
      }
      KeyCode::Right => {
        self.input.move_cursor_right();
        self.update_slash_completion();
      }
      KeyCode::Home => {
        self.input.move_cursor_home();
        self.update_slash_completion();
      }
      KeyCode::End => {
        self.input.move_cursor_end();
        self.update_slash_completion();
      }
      KeyCode::Char(c) => {
        self.input.insert_char(c);
        self.update_slash_completion();
      }
      _ => {}
    }
    None
  }

  fn on_frame(&mut self, frame_requester: &FrameRequester, data: &AppData) {
    if !self.animation_enabled {
      return;
    }

    // Check if last message is from user (waiting for AI response)
    let last_is_user = data
      .chat_history
      .last()
      .map(|msg| msg.is_user())
      .unwrap_or(false);

    // State machine transition logic
    match self.state {
      ChatDisplayState::Animating => {
        // Check if LLM started responding
        if !data.streaming_response.is_empty() {
          // Animating Ã¢â€ â€™ Thinking: first chunk is thinking content
          // Animating Ã¢â€ â€™ Streaming: first chunk is normal content
          if data.streaming_response.iter().any(|c| c.is_thinking()) {
            self.enter_thinking();
          } else {
            self.enter_streaming();
          }
        }
        // Animating Ã¢â€ â€™ WaitingInput: AI response added to history
        else if data.streaming_response.is_empty() && !last_is_user {
          self.enter_waiting_input();
        }
      }
      ChatDisplayState::Thinking => {
        // Thinking Ã¢â€ â€™ Streaming: received normal content (thinking completed)
        // or stream ended but we have thinking content to display
        if data.streaming_response.iter().any(|c| !c.is_thinking()) {
          self.enter_streaming();
        }
        // Thinking Ã¢â€ â€™ WaitingInput: streaming response completed and empty
        else if data.streaming_response.is_empty() {
          self.enter_waiting_input();
        }
        // Note: if stream ends with only thinking content, it will be handled
        // when streaming_response becomes empty (moved to history)
      }
      ChatDisplayState::Streaming => {
        // Streaming Ã¢â€ â€™ WaitingInput: streaming response completed
        if data.streaming_response.is_empty() {
          self.enter_waiting_input();
        }
      }
      ChatDisplayState::WaitingInput => {
        // WaitingInput Ã¢â€ â€™ Animating: user submits message (handled in submit_message)
        // Additional logic: check if need to enter Animating (e.g., when switching from HomeView)
        if last_is_user && data.streaming_response.is_empty() {
          self.enter_animating();
        }
      }
    }

    let now = Instant::now();

    // Update spinner animation (for waiting user input prompt)
    let elapsed = now.duration_since(self.last_spinner_update);
    // Update spinner frame every 200ms (relaxed rotation)
    const SPINNER_INTERVAL: Duration = Duration::from_millis(200);
    if elapsed >= SPINNER_INTERVAL {
      self.spinner_frame = (self.spinner_frame + 1) % SPINNER_FRAMES.len();
      self.last_spinner_update = now;
    }

    // Update moon animation (only in Animating state)
    if self.state == ChatDisplayState::Animating {
      let moon_elapsed = now.duration_since(self.last_moon_update);
      // Moon cycles slower than spinner - one phase every 300ms
      const MOON_INTERVAL: Duration = Duration::from_millis(300);
      if moon_elapsed >= MOON_INTERVAL {
        self.moon_frame = (self.moon_frame + 1) % MOON_FRAMES.len();
        self.last_moon_update = now;
      }
    }

    // Update thinking spinner animation (only in Thinking state)
    if self.state == ChatDisplayState::Thinking {
      let spinner_elapsed = now.duration_since(self.last_spinner_update);
      const THINKING_SPINNER_INTERVAL: Duration = Duration::from_millis(200);
      if spinner_elapsed >= THINKING_SPINNER_INTERVAL {
        self.spinner_frame = (self.spinner_frame + 1) % SPINNER_FRAMES.len();
        self.last_spinner_update = now;
      }
    }

    // Schedule next frame for smooth animation
    frame_requester.schedule_frame_in(TARGET_FRAME_INTERVAL);
  }

  fn draw(&mut self, f: &mut Frame, data: &AppData) {
    use crate::view::chrome;
    chrome::background(f);
    let completion_height = if self.slash_completion.active {
      self.slash_completion.filtered.len().min(6) as u16
    } else {
      0
    };
    let layout = chrome::layout(f.area(), self.input.text(), completion_height);
    chrome::header(f, layout.header, &self.prompt, false);

    // Panels have their own reserved space, independent of transcript length.
    let approval_height = data
      .pending_approval
      .as_ref()
      .map(approval::ApprovalPanel::height)
      .unwrap_or(0);
    let question_height = data
      .pending_questions
      .as_ref()
      .map(|q| questions::QuestionsPanel::height(q, layout.transcript.width))
      .unwrap_or(0);
    let activity_height = u16::from(matches!(
      self.state,
      ChatDisplayState::Animating | ChatDisplayState::Thinking
    ));
    let queue_height = if data.input_queue.entries.is_empty()
      && data.pending_approval.is_none()
      && data.pending_questions.is_none()
    {
      0
    } else {
      (data.input_queue.entries.len().min(2) + 1) as u16
    };
    let parts = Layout::vertical([
      Constraint::Min(0),
      Constraint::Length(activity_height),
      Constraint::Length(approval_height),
      Constraint::Length(question_height),
      Constraint::Length(queue_height),
    ])
    .split(layout.transcript);

    // Merge streaming fragments before sizing and rendering so a chunk cannot
    // consume a rectangle reserved for a pending question or approval.
    let mut streamed: Vec<ChatMessage> = Vec::new();
    if self.state == ChatDisplayState::Streaming {
      for chunk in data.streaming_response.iter() {
        match chunk {
          StreamingChunk::Normal(text) => {
            if let Some(ChatMessage::Assistant {
              content,
              thinking_content: None,
            }) = streamed.last_mut()
            {
              content.push_str(text);
            } else {
              streamed.push(ChatMessage::Assistant {
                content: text.clone(),
                thinking_content: None,
              });
            }
          }
          StreamingChunk::Thinking(text) => {
            if let Some(ChatMessage::Assistant {
              thinking_content: Some(thinking),
              ..
            }) = streamed.last_mut()
            {
              thinking.push_str(text);
            } else {
              streamed.push(ChatMessage::Assistant {
                content: String::new(),
                thinking_content: Some(text.clone()),
              });
            }
          }
          StreamingChunk::ToolCall { .. } => {}
        }
      }
    }
    let entries: Vec<&ChatMessage> = data.chat_history.iter().chain(streamed.iter()).collect();
    if entries.is_empty()
      && self.state == ChatDisplayState::WaitingInput
      && data.pending_approval.is_none()
      && data.pending_questions.is_none()
    {
      chrome::welcome(f, parts[0], false);
    } else {
      // Clamp to the transcript height so the counter cannot run away and the
      // banner always describes what is actually on screen.
      let total_lines: usize = entries
        .iter()
        .map(|message| messages::message_height(message, parts[0].width).max(1))
        .sum();
      let max_scroll = total_lines.saturating_sub(usize::from(parts[0].height));
      self.scroll_offset = self.scroll_offset.min(max_scroll);
      messages::render_history_scrolled(f.buffer_mut(), parts[0], &entries, self.scroll_offset);
      if self.scroll_offset > 0 {
        let at_top = self.scroll_offset == max_scroll;
        let hint = format!(
          "scrolled back {} of {} line(s){} - PageDown or End to return",
          self.scroll_offset,
          max_scroll,
          if at_top { " (top)" } else { "" }
        );
        let banner = Rect::new(parts[0].x, parts[0].y, parts[0].width, 1);
        f.render_widget(
          Paragraph::new(hint)
            .style(ratatui::style::Style::default().fg(crate::utils::colors::WARNING)),
          banner,
        );
      }
    }
    match self.state {
      ChatDisplayState::Animating => {
        let waited = self
          .turn_started
          .map(|start| start.elapsed().as_secs())
          .unwrap_or(0);
        messages::render_moon_animation(f.buffer_mut(), parts[1], self.current_moon(), waited)
      }
      ChatDisplayState::Thinking => {
        let waited = self
          .turn_started
          .map(|start| start.elapsed().as_secs())
          .unwrap_or(0);
        messages::render_thinking_indicator(
          f.buffer_mut(),
          parts[1],
          self.current_spinner(),
          waited,
        )
      }
      _ => {}
    }
    if let Some(approval) = &data.pending_approval {
      approval::ApprovalPanel::render(f, parts[2], approval);
    }
    if let Some(questions) = &data.pending_questions {
      questions::QuestionsPanel::render(f, parts[3], questions);
    }

    if queue_height > 0 {
      let mut lines = vec![Line::from(format!(
        "Queued {}{}  Ã‚Â·  ctrl+g compose/return  Ã‚Â·  /queue",
        data.input_queue.entries.len(),
        if data.input_queue.paused {
          " (paused)"
        } else {
          ""
        }
      ))];
      for input in data.input_queue.entries.iter().take(2) {
        let preview: String = input
          .content
          .split_whitespace()
          .collect::<Vec<_>>()
          .join(" ")
          .chars()
          .take(64)
          .collect();
        lines.push(Line::from(format!(
          "{} {:?}  {}",
          input.id.chars().take(8).collect::<String>(),
          input.kind,
          preview
        )));
      }
      f.render_widget(
        Paragraph::new(lines)
          .style(ratatui::style::Style::default().fg(crate::utils::colors::MUTED)),
        parts[4],
      );
    }

    let focused =
      self.queue_composing || (data.pending_approval.is_none() && data.pending_questions.is_none());
    chrome::composer(
      f,
      layout.composer,
      self.input.text(),
      self.input.cursor(),
      focused,
    );
    if completion_height > 0 {
      self.render_slash_completion(f, layout.completion);
    }
    self.update_status_bar_info(data);
    render_status_bar(f, layout.footer, &self.status_bar_info);
  }

  fn set_frame_requester(&mut self, frame_requester: FrameRequester) {
    self.frame_requester = Some(frame_requester.clone());
    // Start animation loop immediately
    if self.animation_enabled {
      frame_requester.schedule_frame_in(TARGET_FRAME_INTERVAL);
    }
  }
}

// Note: ChatView cannot implement Default because it requires a SessionHandle.
// Use ChatView::new(data, session_handle) to create an instance.

#[cfg(test)]
mod tests;
