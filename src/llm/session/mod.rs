//! Session module.
//!
//! Re-exports public session types and delegates actor implementation.

use tokio::sync::mpsc;

pub mod actor;
pub mod approval;
pub mod compaction;
pub mod context;
pub mod input_queue;
pub mod persistence;
pub mod plan_injection;
pub mod state;
pub mod stream;
pub mod tool_exec;

pub use actor::ChatSession;

#[allow(unused_imports)]
pub(crate) use actor::generate_session_id;

/// An option for a structured user question
#[derive(Debug, Clone)]
pub struct QuestionOption {
  /// Concise display text
  pub label: String,
  /// Brief explanation of trade-offs
  pub description: String,
}

/// A structured question to ask the user
#[derive(Debug, Clone)]
pub struct Question {
  /// The question text
  pub question: String,
  /// Short category tag
  pub header: String,
  /// Available options
  pub options: Vec<QuestionOption>,
  /// Whether multiple options can be selected
  pub multi_select: bool,
  /// Whether this is a yes/no confirmation dialog
  pub confirmation: bool,
  /// Default selected option indices (0-based)
  pub default: Vec<usize>,
  /// Whether the user must select at least one option
  pub required: bool,
}

/// Commands sent to the session actor
#[derive(Debug)]
pub enum SessionCommand {
  /// Begin the preloaded prompt of a newly constructed subagent.
  StartExistingContext,
  EnqueueInput {
    id: String,
    kind: input_queue::InputKind,
    content: String,
  },
  InspectInputQueue,
  EditQueuedInput {
    id: String,
    content: String,
  },
  RemoveQueuedInput {
    id: String,
  },
  PauseInputQueue {
    paused: bool,
  },
  /// Send a user message
  SendMessage {
    content: String,
  },
  /// Cancel the current streaming request
  Cancel,
  /// Stop asking for approval of this tool for the rest of the session
  AllowToolForSession {
    /// Tool name
    name: String,
  },
  /// Switch the provider to another configured model alias
  SwitchModel {
    /// Model alias from the `[models]` section
    alias: String,
  },
  /// Clear conversation history
  ClearHistory,
  /// Approve or deny a pending tool call
  ApproveToolCall {
    /// Tool call ID being approved
    tool_call_id: String,
    /// Whether the user approved the execution
    approved: bool,
    /// When true and approved is false, reject all remaining tools in the queue
    reject_remaining: bool,
  },
  /// Answer pending structured questions
  AnswerQuestions {
    /// Tool call ID that asked the questions
    tool_call_id: String,
    /// Selected option indices for each question
    answers: Vec<Vec<usize>>,
    /// Whether the user dismissed without answering
    dismissed: bool,
  },
  /// Enable YOLO mode for the current session (persisted to meta)
  EnableSessionYolo,
  /// Execute a `/plan` slash command (on|off|view|clear|toggle)
  PlanSlashCommand {
    subcmd: String,
  },
  /// Shutdown the session actor
  Shutdown,
}

/// Events emitted by the chat session
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum SessionEvent {
  /// A chunk of content received from the stream
  ContentChunk(String),
  /// A chunk of thinking/reasoning content received from the stream
  ThinkingChunk(String),
  /// A tool call was received from the model
  ToolCallReceived {
    id: String,
    name: String,
    arguments: String,
  },
  /// A tool execution completed
  ToolCallCompleted { name: String, output: String },
  /// Stream completed successfully
  Completed,
  /// Error occurred during streaming
  Error(String),
  /// Stream was interrupted mid-way; the actor may attempt to retry
  StreamInterrupted {
    /// Error message
    error: String,
    /// Whether the actor should attempt to retry
    is_retryable: bool,
  },
  /// A tool call requires user approval before execution
  ApprovalNeeded {
    /// Tool call ID
    id: String,
    /// Tool name
    name: String,
    /// Tool arguments
    arguments: String,
    /// Optional diff preview for file-modifying tools
    diff_preview: Option<String>,
  },
  /// Session has been shutdown
  Shutdown,
  /// Compaction is needed (token limit approaching)
  CompactionNeeded {
    /// Current estimated token count
    current_tokens: usize,
    /// Token threshold that triggered this notification
    threshold: usize,
    /// Maximum context size for the current model
    max_context_size: usize,
  },
  /// Compaction completed (messages were compacted)
  CompactionCompleted {
    /// Number of messages before compaction
    message_count_before: usize,
    /// Number of messages after compaction
    message_count_after: usize,
    /// New estimated token count
    new_token_count: usize,
  },
  /// Structured questions need to be presented to the user
  QuestionsAsked {
    /// Tool call ID
    tool_call_id: String,
    /// Questions to present
    questions: Vec<Question>,
  },
  /// Token usage information from the API (sent when stream finishes)
  Usage {
    /// Total tokens in the conversation (prompt + completion)
    total_tokens: u32,
    /// Tokens in the prompt (input)
    prompt_tokens: u32,
    /// Tokens in the completion (output)
    completion_tokens: u32,
    cached_tokens: Option<u32>,
  },
}

/// Handle to interact with a running session actor
#[derive(Debug, Clone)]
pub struct SessionHandle {
  abort_handle: Option<tokio::task::AbortHandle>,
  /// Session ID
  pub id: String,
  /// Channel to send commands to the session
  cmd_tx: mpsc::UnboundedSender<SessionCommand>,
}

/// Keeps a spawned actor subordinate to its owning foreground/background job.
pub(crate) struct SessionTaskGuard(SessionHandle);
impl Drop for SessionTaskGuard {
  fn drop(&mut self) {
    self.0.abort();
  }
}

#[allow(dead_code)]
impl SessionHandle {
  /// Send a user message to the session
  pub fn send_message(&self, content: impl Into<String>) {
    let _ = self.cmd_tx.send(SessionCommand::SendMessage {
      content: content.into(),
    });
  }

  pub fn send_message_checked(&self, content: String) -> std::result::Result<(), String> {
    self
      .cmd_tx
      .send(SessionCommand::SendMessage { content })
      .map_err(|_| "Session is closed; input was not sent".into())
  }

  pub(crate) fn start_existing_context(&self) {
    let _ = self.cmd_tx.send(SessionCommand::StartExistingContext);
  }
  pub fn queue_input(
    &self,
    kind: input_queue::InputKind,
    content: String,
  ) -> std::result::Result<String, String> {
    let id = uuid::Uuid::new_v4().to_string();
    self
      .cmd_tx
      .send(SessionCommand::EnqueueInput {
        id: id.clone(),
        kind,
        content,
      })
      .map_err(|_| "Session is closed; input was not queued".to_string())?;
    Ok(id)
  }
  pub fn inspect_queue(&self) {
    let _ = self.cmd_tx.send(SessionCommand::InspectInputQueue);
  }
  pub fn edit_queued_input(&self, id: String, content: String) {
    let _ = self
      .cmd_tx
      .send(SessionCommand::EditQueuedInput { id, content });
  }
  pub fn remove_queued_input(&self, id: String) {
    let _ = self.cmd_tx.send(SessionCommand::RemoveQueuedInput { id });
  }
  pub fn pause_queue(&self, paused: bool) {
    let _ = self.cmd_tx.send(SessionCommand::PauseInputQueue { paused });
  }

  /// Cancel the current streaming request
  pub fn cancel(&self) {
    let _ = self.cmd_tx.send(SessionCommand::Cancel);
  }

  /// Clear conversation history
  pub fn clear_history(&self) {
    let _ = self.cmd_tx.send(SessionCommand::ClearHistory);
  }

  /// Approve this tool type for the rest of the session.
  pub fn allow_tool_for_session(&self, name: impl Into<String>) {
    let _ = self.cmd_tx.send(SessionCommand::AllowToolForSession {
      name: name.into(),
    });
  }

  /// Switch to another configured model alias.
  pub fn switch_model(&self, alias: impl Into<String>) {
    let _ = self.cmd_tx.send(SessionCommand::SwitchModel {
      alias: alias.into(),
    });
  }

  /// Shutdown the session actor
  pub fn shutdown(&self) {
    let _ = self.cmd_tx.send(SessionCommand::Shutdown);
  }

  pub(crate) fn abort(&self) {
    if let Some(handle) = &self.abort_handle {
      handle.abort();
    }
  }

  pub(crate) fn abort_on_drop(&self) -> SessionTaskGuard {
    SessionTaskGuard(self.clone())
  }

  /// Approve or deny a pending tool call
  pub fn approve_tool_call(
    &self,
    tool_call_id: impl Into<String>,
    approved: bool,
    reject_remaining: bool,
  ) {
    let _ = self.cmd_tx.send(SessionCommand::ApproveToolCall {
      tool_call_id: tool_call_id.into(),
      approved,
      reject_remaining,
    });
  }

  /// Enable YOLO mode for the current session
  pub fn enable_session_yolo(&self) {
    let _ = self.cmd_tx.send(SessionCommand::EnableSessionYolo);
  }

  /// Send a `/plan` slash command to the session actor
  pub fn plan_slash_command(&self, subcmd: impl Into<String>) {
    let _ = self.cmd_tx.send(SessionCommand::PlanSlashCommand {
      subcmd: subcmd.into(),
    });
  }

  /// Send answers to pending structured questions
  pub fn answer_questions(
    &self,
    tool_call_id: impl Into<String>,
    answers: Vec<Vec<usize>>,
    dismissed: bool,
  ) {
    let _ = self.cmd_tx.send(SessionCommand::AnswerQuestions {
      tool_call_id: tool_call_id.into(),
      answers,
      dismissed,
    });
  }
}

#[cfg(test)]
impl SessionHandle {
  /// Create a test session handle with a given command sender.
  pub fn test_new(id: String, cmd_tx: mpsc::UnboundedSender<SessionCommand>) -> Self {
    Self {
      id,
      cmd_tx,
      abort_handle: None,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_plan_slash_command_sends_command() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let handle = SessionHandle::test_new("test".to_string(), tx);

    handle.plan_slash_command("view");

    match rx.try_recv() {
      Ok(SessionCommand::PlanSlashCommand { subcmd }) => assert_eq!(subcmd, "view"),
      other => panic!("Expected PlanSlashCommand, got {:?}", other),
    }
  }
}
