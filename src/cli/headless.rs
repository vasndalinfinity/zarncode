//! Non-interactive client of the same session actor used by the TUI.

use std::io::{self, IsTerminal, Read, Write};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc};

use super::args::{Args, OutputFormat};
use super::runtime::Runtime;
use crate::hooks::{HookDecision, HookEventType, WireHookDispatcher, WireHookHandle, events};
use crate::llm::ChatSession;
use crate::session::{SessionMode, SessionStore};
use crate::wire::{WireBus, WireMessage};

const MAX_STDIN_BYTES: u64 = 16 * 1024 * 1024;

fn prompt_from_input(prompt: Option<&str>, piped: Option<&str>) -> Result<String> {
  let prompt = prompt.unwrap_or("");
  let piped = piped.unwrap_or("");
  if prompt.trim().is_empty() && piped.trim().is_empty() {
    bail!("Provide a prompt or pipe UTF-8 input to --print");
  }
  Ok(if prompt.trim().is_empty() {
    piped.to_string()
  } else if piped.trim().is_empty() {
    prompt.to_string()
  } else {
    format!("{prompt}\n\n--- stdin ---\n{piped}")
  })
}

fn read_prompt(args: &Args) -> Result<String> {
  let mut input = String::new();
  if !io::stdin().is_terminal() {
    let mut bytes = Vec::new();
    io::stdin()
      .take(MAX_STDIN_BYTES + 1)
      .read_to_end(&mut bytes)
      .context("Read piped input")?;
    if bytes.len() as u64 > MAX_STDIN_BYTES {
      bail!("Piped input exceeds the 16 MiB limit");
    }
    input = String::from_utf8(bytes).context("Piped input must be valid UTF-8")?;
  }
  prompt_from_input(args.prompt.as_deref(), Some(&input))
}

fn printable(value: &str) -> String {
  value
    .chars()
    .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
    .collect()
}

struct Outcome {
  code: u8,
  status: &'static str,
  message: Option<String>,
}

impl Outcome {
  fn success() -> Self {
    Self {
      code: 0,
      status: "success",
      message: None,
    }
  }

  fn completed(blocked: bool) -> Self {
    if blocked {
      Self::failure(
        3,
        "blocked",
        "A submission hook blocked the task before inference",
      )
    } else {
      Self::success()
    }
  }
  fn failure(code: u8, status: &'static str, message: impl Into<String>) -> Self {
    Self {
      code,
      status,
      message: Some(message.into()),
    }
  }
}

struct Output<W> {
  writer: W,
  format: OutputFormat,
  session_id: Option<String>,
  text: String,
  usage: Option<Value>,
}

impl<W: Write> Output<W> {
  fn new(writer: W, format: OutputFormat) -> Self {
    Self {
      writer,
      format,
      session_id: None,
      text: String::new(),
      usage: None,
    }
  }

  fn emit(&mut self, mut event: Value) -> io::Result<()> {
    if self.format != OutputFormat::Jsonl {
      return Ok(());
    }
    event["schema_version"] = json!(1);
    event["session_id"] = json!(self.session_id);
    serde_json::to_writer(&mut self.writer, &event)?;
    self.writer.write_all(b"\n")?;
    self.writer.flush()
  }

  fn event(&mut self, event: &WireMessage) -> io::Result<()> {
    match event {
      WireMessage::InputQueueChanged { queue } => {
        self.emit(json!({"type":"input_queue","pending":queue.entries.len(),"paused":queue.paused}))
      }
      WireMessage::InputDelivered { id, kind, .. } => {
        self.emit(json!({"type":"input_delivered","id":id,"kind":kind}))
      }
      WireMessage::TurnBegin => {
        self.text.clear();
        self.emit(json!({"type":"turn_start"}))
      }
      WireMessage::ContentChunk { text } => {
        self.text.push_str(text);
        self.emit(json!({"type":"content_delta","text":text}))
      }
      WireMessage::ToolCallBegin {
        id,
        name,
        arguments,
      } => {
        self.text.clear();
        self.emit(json!({"type":"tool_start","id":id,"name":name,"arguments":arguments}))
      }
      WireMessage::ToolCallEnd { id, name, output } => {
        self.emit(json!({"type":"tool_end","id":id,"name":name,"output":output}))
      }
      WireMessage::Usage {
        prompt_tokens,
        completion_tokens,
        cached_tokens,
        total_tokens,
      } => {
        let usage = json!({"input":prompt_tokens,"output":completion_tokens,"cache":cached_tokens,"total":total_tokens});
        self.usage = Some(usage.clone());
        self.emit(json!({"type":"usage","tokens":usage}))
      }
      WireMessage::CompactionCompleted {
        before,
        after,
        tokens,
      } => self
        .emit(json!({"type":"compaction","before":before,"after":after,"context_tokens":tokens})),
      WireMessage::SubagentEvent {
        agent_id,
        subagent_type,
        ..
      } => self
        .emit(json!({"type":"subagent_activity","agent_id":agent_id,"agent_type":subagent_type})),
      _ => Ok(()),
    }
  }

  fn finish(&mut self, outcome: &Outcome) -> io::Result<()> {
    if self.format == OutputFormat::Jsonl {
      self.emit(json!({"type":"result","status":outcome.status,"exit_code":outcome.code,"text":self.text,"message":outcome.message,"last_request_usage":self.usage}))
    } else {
      if outcome.code == 0 {
        let text = printable(&self.text);
        self.writer.write_all(text.as_bytes())?;
        if !text.ends_with('\n') {
          self.writer.write_all(b"\n")?;
        }
        self.writer.flush()?;
      }
      Ok(())
    }
  }
}

/// Client-side hooks cannot silently grant permissions when no client can answer.
struct HeadlessHooks {
  denied: mpsc::UnboundedSender<String>,
}

#[async_trait]
impl WireHookDispatcher for HeadlessHooks {
  async fn dispatch_wire_hook(&self, handle: WireHookHandle) {
    let reason = format!("Hook {} requires an interactive client", handle.id);
    handle.resolve("block", &reason);
    let _ = self.denied.send(reason);
  }
}

struct BackgroundCleanup(Arc<Runtime>);
impl Drop for BackgroundCleanup {
  fn drop(&mut self) {
    self
      .0
      .background_manager
      .kill_all_active("Headless CLI run ended");
  }
}

async fn collect<W: Write>(
  session: &ChatSession,
  runtime: &Runtime,
  subscriber: &mut broadcast::Receiver<WireMessage>,
  approvals: &mut mpsc::UnboundedReceiver<crate::approval::runtime::ApprovalRequest>,
  denied_hooks: &mut mpsc::UnboundedReceiver<String>,
  output: &mut Output<W>,
) -> Result<Outcome> {
  loop {
    tokio::select! {
      biased;
      Some(reason) = denied_hooks.recv() => return Ok(Outcome::failure(3, "interaction_required", reason)),
      Some(request) = approvals.recv() => {
        runtime.approval_runtime().respond(&request.approval_id, false, true);
        output.emit(json!({"type":"approval_required","name":request.name,"tool_call_id":request.tool_call_id}))?;
        return Ok(Outcome::failure(3, "approval_required", "A tool requires approval; use interactive mode or explicitly opt in with --yolo"));
      }
      event = subscriber.recv() => {
        let event = event.context("Session event stream closed or lagged; refusing to return incomplete output")?;
        output.event(&event)?;
        match event {
          WireMessage::TurnCancelled => return Ok(Outcome::failure(130, "cancelled", "Interrupted")),
          WireMessage::TurnComplete { blocked } => return Ok(Outcome::completed(blocked)),
          WireMessage::Error { message } => return Ok(Outcome::failure(1, "error", message)),
          WireMessage::QuestionsAsked { tool_call_id, .. } => {
            session.handle.answer_questions(tool_call_id, Vec::new(), true);
            return Ok(Outcome::failure(3, "interaction_required", "The agent requested user input; run this task interactively"));
          }
          _ => {}
        }
      }
    }
  }
}

async fn execute<W: Write>(args: &Args, prompt: String, output: &mut Output<W>) -> Result<Outcome> {
  let mut config = args.effective_config()?;
  // Neither a persisted config nor a resumed session may grant unattended access.
  config.yolo = args.yolo;
  config.auto_approve.clear();
  let model = config.default_model.clone();
  let runtime_options = args.runtime_options(&config)?;
  let data_dir = crate::config::loader::data_dir(&config);
  crate::init_logging(&config);
  let runtime = Runtime::new_with_options(
    &data_dir,
    Arc::new(config),
    args.config_dir(),
    runtime_options,
  )?;
  let runtime = Arc::new(runtime);
  let bus = WireBus::new(WireBus::DEFAULT_CAPACITY);
  let mut subscriber = bus.subscriber();
  let (approval_tx, mut approval_rx) = mpsc::unbounded_channel();
  runtime.approval_runtime().bind_ui(approval_tx);
  let (hook_tx, mut denied_hooks) = mpsc::unbounded_channel();
  runtime
    .hook_engine()
    .set_dispatcher(Arc::new(HeadlessHooks { denied: hook_tx }));
  let mode = if let Some(id) = &args.session {
    SessionMode::ResumeById(id.clone())
  } else if args.r#continue {
    SessionMode::ResumeLatest
  } else {
    SessionMode::New
  };
  let (mut session, _) = ChatSession::create_or_resume(
    runtime.clone(),
    runtime.render_system_prompt(),
    Arc::new(SessionStore::new(&data_dir)),
    mode.clone(),
    bus.publisher(),
  )?;
  let id = session.handle.id.clone();
  output.session_id = Some(id.clone());
  runtime.background_manager.bind_session(&id);
  runtime.notification_manager.bind_session(&id);
  let _cleanup = BackgroundCleanup(runtime.clone());
  let run = async {
    output.emit(json!({"type":"init","model":model,"agent":runtime.active_agent,"max_steps":runtime.max_steps,"max_concurrent_agents":runtime.agent_slots.available_permits()}))?;
    let source = if matches!(mode, SessionMode::New) {
      "startup"
    } else {
      "resume"
    };
    let results = runtime
      .hook_engine()
      .trigger(
        HookEventType::SessionStart,
        source,
        events::session_start(&id, &runtime.args.work_dir, source),
      )
      .await;
    if let Some(reason) = results.into_iter().find_map(|r| {
      if let HookDecision::Block { reason } = r.decision {
        Some(reason)
      } else {
        None
      }
    }) {
      return Ok(Outcome::failure(3, "blocked", reason));
    }
    session.handle.send_message(prompt);
    collect(
      &session,
      &runtime,
      &mut subscriber,
      &mut approval_rx,
      &mut denied_hooks,
      output,
    )
    .await
  }
  .await;
  session.close().await;
  let _ = tokio::time::timeout(
    Duration::from_secs(5),
    runtime.hook_engine().trigger(
      HookEventType::SessionEnd,
      "exit",
      events::session_end(&id, &runtime.args.work_dir, "exit"),
    ),
  )
  .await;
  run
}

pub async fn run(args: &Args) -> u8 {
  let mut output = Output::new(
    io::stdout(),
    args.output_format.unwrap_or(OutputFormat::Text),
  );
  let outcome = match read_prompt(args) {
    Err(error) => Outcome::failure(2, "invalid_input", error.to_string()),
    Ok(prompt) => {
      tokio::select! {
        result = execute(args, prompt, &mut output) => match result {
          Ok(outcome) => outcome,
          Err(error) if error.downcast_ref::<io::Error>().is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe) => return 0,
          Err(error) => Outcome::failure(1, "error", format!("{error:#}")),
        },
        signal = tokio::signal::ctrl_c() => match signal {
          Ok(()) => Outcome::failure(130, "cancelled", "Interrupted"),
          Err(error) => Outcome::failure(1, "error", error.to_string()),
        },
      }
    }
  };
  if let Some(message) = &outcome.message {
    eprintln!("{}", printable(message));
  }
  match output.finish(&outcome) {
    Ok(()) => outcome.code,
    Err(error) if error.kind() == io::ErrorKind::BrokenPipe => 0,
    Err(error) => {
      eprintln!("Could not write CLI output: {error}");
      1
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  #[test]
  fn prompts_preserve_piped_context_and_reject_empty_tasks() {
    assert!(prompt_from_input(None, Some(" \n")).is_err());
    assert_eq!(prompt_from_input(None, Some("code\n")).unwrap(), "code\n");
    assert_eq!(
      prompt_from_input(Some("review"), Some("code")).unwrap(),
      "review\n\n--- stdin ---\ncode"
    );
  }
  #[test]
  fn json_events_escape_untrusted_content_and_preserve_cache_usage() {
    let mut out = Output::new(Vec::new(), OutputFormat::Jsonl);
    out
      .event(&WireMessage::ContentChunk {
        text: "hello\n\"injected\"\u{1b}[31m".into(),
      })
      .unwrap();
    out
      .event(&WireMessage::Usage {
        prompt_tokens: 10,
        completion_tokens: 2,
        cached_tokens: Some(3),
        total_tokens: 12,
      })
      .unwrap();
    out.finish(&Outcome::success()).unwrap();
    let text = String::from_utf8(out.writer).unwrap();
    assert!(!text.contains('\u{1b}'));
    let events: Vec<Value> = text
      .lines()
      .map(|s| serde_json::from_str(s).unwrap())
      .collect();
    assert_eq!(events.len(), 3);
    assert_eq!(events[2]["last_request_usage"]["cache"], 3);
    assert_eq!(events[2]["exit_code"], 0);
  }
  #[tokio::test]
  async fn client_hooks_are_denied_without_an_interactive_client() {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let dispatcher = HeadlessHooks { denied: tx };
    let handle = WireHookHandle::new(
      "test",
      "hook",
      HookEventType::PreToolUse,
      "WriteFile",
      json!({}),
    );
    dispatcher.dispatch_wire_hook(handle.clone()).await;
    assert!(matches!(
      handle.wait().await.decision,
      HookDecision::Block { .. }
    ));
    assert!(rx.recv().await.unwrap().contains("interactive"));
  }

  #[test]
  fn a_policy_block_is_never_reported_as_a_successful_task() {
    assert_eq!(Outcome::completed(false).code, 0);
    assert_eq!(Outcome::completed(true).code, 3);
    assert_eq!(Outcome::completed(true).status, "blocked");
  }
}
