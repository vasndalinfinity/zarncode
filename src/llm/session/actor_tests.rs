use super::*;
#[test]
fn session_ids_are_portable_unique_and_usable_as_directory_names() {
  let directory = tempfile::tempdir().unwrap();
  let first = generate_session_id();
  let second = generate_session_id();
  assert!(
    !first
      .chars()
      .any(|c| "<>:\"/\\|?*".contains(c) || c.is_control()),
    "non-portable id: {first}"
  );
  assert_ne!(first, second);
  std::fs::create_dir(directory.path().join(first)).unwrap();
}

struct IdleProvider;
#[async_trait::async_trait]
impl LLMProvider for IdleProvider {
  async fn chat_stream(&self, _: Vec<Message>) -> Result<crate::llm::provider::LlmResponseStream> {
    Ok(Box::pin(futures::stream::empty()))
  }
  fn name(&self) -> &str {
    "fixture"
  }
  fn max_context_size(&self) -> usize {
    1000
  }
}

#[tokio::test]
async fn stop_hook_guard_is_consumed_only_when_continuation_finishes() {
  let bus = crate::wire::WireBus::new(16);
  let mut events = bus.subscriber();
  let (_tx, rx) = mpsc::unbounded_channel();
  let mut actor = SessionActor::new(SessionActorConfig {
    id: "guard-test".into(),
    provider: Box::new(IdleProvider),
    messages: vec![],
    wire_publisher: bus.publisher(),
    cmd_rx: rx,
    session_store: None,
    meta: SessionMeta::new("guard-test", ""),
    runtime: Arc::new(Runtime::for_test(crate::config::Config::default())),
    tool_policy: None,
  });
  actor.stop_hook_active = true;
  actor.agent_loop_end().await;
  assert!(
    !actor.stop_hook_active,
    "continuation completed but its guard was not reset"
  );
  assert!(matches!(events.recv().await.unwrap(), WireMessage::TurnEnd));
  assert!(matches!(
    events.recv().await.unwrap(),
    WireMessage::TurnComplete { blocked: false }
  ));
}

#[tokio::test]
async fn a_busy_session_never_discards_a_new_user_message() {
  let bus = crate::wire::WireBus::new(16);
  let mut events = bus.subscriber();
  let (tx, rx) = mpsc::unbounded_channel();
  let mut actor = SessionActor::new(SessionActorConfig {
    id: "queue-test".into(),
    provider: Box::new(IdleProvider),
    messages: vec![],
    wire_publisher: bus.publisher(),
    cmd_rx: rx,
    session_store: None,
    meta: SessionMeta::new("queue-test", ""),
    runtime: Arc::new(Runtime::for_test(crate::config::Config::default())),
    tool_policy: None,
  });
  actor.state = ActorState::Streaming;
  let task = tokio::spawn(actor.run());
  tx.send(SessionCommand::SendMessage {
    content: "follow this up".into(),
  })
  .unwrap();
  let event = loop {
    let event = tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
      .await
      .unwrap()
      .unwrap();
    if matches!(&event, WireMessage::Error { .. }) {
      break event;
    }
    if let WireMessage::InputQueueChanged { queue } = &event
      && queue.entries.iter().any(|e| e.content == "follow this up")
    {
      break event;
    }
  };
  let _ = tx.send(SessionCommand::Shutdown);
  task.await.unwrap();
  assert!(
    !matches!(event, WireMessage::Error { .. }),
    "busy input was rejected instead of queued"
  );
}

struct ControlledProvider {
  requests: mpsc::UnboundedSender<(
    Vec<Message>,
    mpsc::UnboundedSender<Result<crate::llm::provider::LlmStreamEvent>>,
  )>,
}
#[async_trait::async_trait]
impl LLMProvider for ControlledProvider {
  async fn chat_stream(
    &self,
    messages: Vec<Message>,
  ) -> Result<crate::llm::provider::LlmResponseStream> {
    let (tx, rx) = mpsc::unbounded_channel();
    self.requests.send((messages, tx)).unwrap();
    Ok(Box::pin(
      tokio_stream::wrappers::UnboundedReceiverStream::new(rx),
    ))
  }
  fn name(&self) -> &str {
    "controlled"
  }
  fn max_context_size(&self) -> usize {
    100000
  }
}

#[tokio::test]
async fn steering_waits_for_a_safe_boundary_and_followups_wait_for_task_completion() {
  use super::super::input_queue::InputKind;
  use crate::llm::provider::LlmStreamEvent;
  let bus = crate::wire::WireBus::new(64);
  let mut events = bus.subscriber();
  let (tx, rx) = mpsc::unbounded_channel();
  let (requests, mut responses) = mpsc::unbounded_channel();
  let actor = SessionActor::new(SessionActorConfig {
    id: "delivery-test".into(),
    provider: Box::new(ControlledProvider { requests }),
    messages: vec![],
    wire_publisher: bus.publisher(),
    cmd_rx: rx,
    session_store: None,
    meta: SessionMeta::new("delivery-test", ""),
    runtime: Arc::new(Runtime::for_test(crate::config::Config::default())),
    tool_policy: None,
  });
  let task = tokio::spawn(actor.run());
  tx.send(SessionCommand::SendMessage {
    content: "original".into(),
  })
  .unwrap();
  let (first, stream1) = tokio::time::timeout(std::time::Duration::from_secs(3), responses.recv())
    .await
    .unwrap()
    .unwrap();
  assert_eq!(first.last().unwrap().content, "original");
  tx.send(SessionCommand::EnqueueInput {
    id: "s1".into(),
    kind: InputKind::Steer,
    content: "change direction".into(),
  })
  .unwrap();
  tx.send(SessionCommand::EnqueueInput {
    id: "f1".into(),
    kind: InputKind::FollowUp,
    content: "next task".into(),
  })
  .unwrap();
  loop {
    let event = tokio::time::timeout(std::time::Duration::from_secs(3), events.recv())
      .await
      .unwrap()
      .unwrap();
    if let WireMessage::InputQueueChanged { queue } = event
      && queue.entries.len() == 2
    {
      break;
    }
  }
  assert!(
    responses.try_recv().is_err(),
    "queued input modified an in-flight request"
  );
  stream1
    .send(Ok(LlmStreamEvent::Content("initial response".into())))
    .unwrap();
  drop(stream1);
  let (second, stream2) = tokio::time::timeout(std::time::Duration::from_secs(3), responses.recv())
    .await
    .unwrap()
    .unwrap();
  let users: Vec<_> = second
    .iter()
    .filter(|m| m.role == crate::llm::types::Role::User)
    .map(|m| m.content.as_str())
    .collect();
  assert_eq!(users, ["original", "change direction"]);
  stream2
    .send(Ok(LlmStreamEvent::Content("steered response".into())))
    .unwrap();
  drop(stream2);
  let (third, stream3) = tokio::time::timeout(std::time::Duration::from_secs(3), responses.recv())
    .await
    .unwrap()
    .unwrap();
  let users: Vec<_> = third
    .iter()
    .filter(|m| m.role == crate::llm::types::Role::User)
    .map(|m| m.content.as_str())
    .collect();
  assert_eq!(users, ["original", "change direction", "next task"]);
  stream3
    .send(Ok(LlmStreamEvent::Content("follow-up response".into())))
    .unwrap();
  drop(stream3);
  let mut completed = 0;
  while completed < 2 {
    let event = tokio::time::timeout(std::time::Duration::from_secs(3), events.recv())
      .await
      .unwrap()
      .unwrap();
    if matches!(event, WireMessage::TurnComplete { blocked: false }) {
      completed += 1;
    }
  }
  tx.send(SessionCommand::Shutdown).unwrap();
  task.await.unwrap();
}

#[tokio::test]
async fn pending_approvals_block_steering_and_cancel_preserves_tool_pairing() {
  use super::super::input_queue::InputKind;
  let bus = crate::wire::WireBus::new(32);
  let (_tx, rx) = mpsc::unbounded_channel();
  let call = ToolCall::new("pending-tool", "WriteFile", "{}");
  let mut actor = SessionActor::new(SessionActorConfig {
    id: "approval-queue".into(),
    provider: Box::new(IdleProvider),
    messages: vec![Message::assistant_with_tools("", vec![call.clone()])],
    wire_publisher: bus.publisher(),
    cmd_rx: rx,
    session_store: None,
    meta: SessionMeta::new("approval-queue", ""),
    runtime: Arc::new(Runtime::for_test(crate::config::Config::default())),
    tool_policy: None,
  });
  actor.state = ActorState::WaitingApproval {
    tool_calls: vec![call],
    current_index: 0,
    approved_indices: vec![],
    rejected_indices: vec![],
  };
  actor.enqueue_input("s".into(), InputKind::Steer, "pending direction".into());
  Box::pin(actor.drain_ready_inputs()).await;
  assert_eq!(actor.context.messages().len(), 1);
  actor.cancel_pending_input_work();
  assert!(actor.meta.input_queue.paused);
  assert_eq!(
    actor.meta.input_queue.entries[0].content,
    "pending direction"
  );
  assert_eq!(
    actor
      .context
      .messages()
      .last()
      .unwrap()
      .tool_call_id
      .as_deref(),
    Some("pending-tool")
  );
  assert!(matches!(actor.state, ActorState::Idle));
}

#[tokio::test]
async fn a_failed_delivery_is_not_mistaken_for_a_durable_user_message() {
  use super::super::input_queue::InputKind;
  let directory = tempfile::tempdir().unwrap();
  let store = Arc::new(SessionStore::new(directory.path()));
  let meta = SessionMeta::new("io-retry", "");
  store.create(&meta).unwrap();
  let path = directory.path().join("sessions/io-retry/context.jsonl");
  // A directory in place of the log deterministically fails message append.
  std::fs::remove_file(&path).unwrap();
  std::fs::create_dir(&path).unwrap();
  let bus = crate::wire::WireBus::new(32);
  let (_tx, rx) = mpsc::unbounded_channel();
  let mut actor = SessionActor::new(SessionActorConfig {
    id: "io-retry".into(),
    provider: Box::new(IdleProvider),
    messages: vec![],
    wire_publisher: bus.publisher(),
    cmd_rx: rx,
    session_store: Some(store.clone()),
    meta,
    runtime: Arc::new(Runtime::for_test(crate::config::Config::default())),
    tool_policy: None,
  });
  actor.enqueue_input(
    "retry".into(),
    InputKind::Steer,
    "one durable message".into(),
  );
  let input = actor.meta.input_queue.next(false).unwrap();
  assert!(!Box::pin(actor.accept_queued_input(input, true)).await);
  assert!(actor.turn_failed && actor.meta.input_queue.paused);
  assert_eq!(actor.context.messages().len(), 1);
  std::fs::remove_dir(&path).unwrap();
  std::fs::write(&path, "").unwrap();
  actor.meta.input_queue.paused = false;
  actor.turn_failed = false;
  let input = actor.meta.input_queue.next(false).unwrap();
  assert!(Box::pin(actor.accept_queued_input(input, true)).await);
  assert_eq!(actor.context.messages().len(), 1);
  assert_eq!(store.load("io-retry").unwrap().1.len(), 1);
  assert!(actor.meta.input_queue.entries.is_empty());
}

struct SteeringHook;
#[async_trait::async_trait]
impl crate::hooks::WireHookDispatcher for SteeringHook {
  async fn dispatch_wire_hook(&self, handle: crate::hooks::WireHookHandle) {
    if handle.target == "reject this steer" {
      handle.resolve("block", "steering rejected");
    } else {
      handle.resolve("allow", "");
    }
  }
}
#[tokio::test]
async fn blocked_steering_does_not_create_an_extra_model_step() {
  let mut runtime = Runtime::for_test(crate::config::Config::default());
  let mut engine = crate::hooks::HookEngine::new(Vec::new(), None);
  engine.add_wire_subscriptions(vec![crate::hooks::WireHookSubscription::new(
    "steering",
    HookEventType::UserPromptSubmit,
  )]);
  engine.set_dispatcher(Arc::new(SteeringHook));
  runtime.hook_engine = Arc::new(engine);
  let bus = crate::wire::WireBus::new(64);
  let mut events = bus.subscriber();
  let (tx, rx) = mpsc::unbounded_channel();
  let (requests, mut responses) = mpsc::unbounded_channel();
  let actor = SessionActor::new(SessionActorConfig {
    id: "hook-steer".into(),
    provider: Box::new(ControlledProvider { requests }),
    messages: vec![],
    wire_publisher: bus.publisher(),
    cmd_rx: rx,
    session_store: None,
    meta: SessionMeta::new("hook-steer", ""),
    runtime: Arc::new(runtime),
    tool_policy: None,
  });
  let task = tokio::spawn(actor.run());
  tx.send(SessionCommand::SendMessage {
    content: "original".into(),
  })
  .unwrap();
  let (_, stream) = tokio::time::timeout(std::time::Duration::from_secs(3), responses.recv())
    .await
    .unwrap()
    .unwrap();
  tx.send(SessionCommand::EnqueueInput {
    id: "blocked".into(),
    kind: super::super::input_queue::InputKind::Steer,
    content: "reject this steer".into(),
  })
  .unwrap();
  loop {
    let event = events.recv().await.unwrap();
    if let WireMessage::InputQueueChanged { queue } = event
      && queue.entries.len() == 1
    {
      break;
    }
  }
  stream
    .send(Ok(crate::llm::provider::LlmStreamEvent::Content(
      "done".into(),
    )))
    .unwrap();
  drop(stream);
  tokio::time::timeout(std::time::Duration::from_secs(3), async {
      loop {
        tokio::select! {
          Some(_) = responses.recv() => panic!("a rejected steer created another model request"),
          Ok(event) = events.recv() => if matches!(event, WireMessage::TurnComplete { blocked: false }) { break; },
        }
      }
    }).await.unwrap();
  assert!(responses.try_recv().is_err());
  tx.send(SessionCommand::Shutdown).unwrap();
  task.await.unwrap();
}

#[tokio::test]
async fn dropping_a_job_guard_cancels_its_inflight_child_provider() {
  let bus = crate::wire::WireBus::new(16);
  let (requests, mut responses) = mpsc::unbounded_channel();
  let handle = ChatSession::start_subagent(
    "owned-child".into(),
    Box::new(ControlledProvider { requests }),
    vec![Message::user("child work")],
    Arc::new(Runtime::for_test(crate::config::Config::default())),
    bus.publisher(),
    None,
    false,
  );
  let guard = handle.abort_on_drop();
  handle.start_existing_context();
  let (_, stream) = tokio::time::timeout(std::time::Duration::from_secs(3), responses.recv())
    .await
    .unwrap()
    .unwrap();
  drop(guard);
  tokio::time::timeout(std::time::Duration::from_secs(3), async {
    while !stream.is_closed() {
      tokio::task::yield_now().await;
    }
  })
  .await
  .unwrap();
}
