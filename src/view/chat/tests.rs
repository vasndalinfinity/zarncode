use crossterm::event::{KeyCode, KeyEvent};
use tokio::sync::mpsc;

use crate::cli::app::PendingQuestions;
use crate::cli::runtime::Runtime;
use crate::config::Config;
use crate::llm::Question;
use crate::llm::session::SessionCommand;

use super::*;

fn make_session_handle() -> (SessionHandle, mpsc::UnboundedReceiver<SessionCommand>) {
  let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
  (
    SessionHandle::test_new("test-session".to_string(), cmd_tx),
    cmd_rx,
  )
}

fn make_pending_questions() -> PendingQuestions {
  PendingQuestions {
    tool_call_id: "call-123".to_string(),
    questions: vec![
      Question {
        question: "Which color?".to_string(),
        header: "Style".to_string(),
        options: vec![
          crate::llm::session::QuestionOption {
            label: "Red".to_string(),
            description: "Bold".to_string(),
          },
          crate::llm::session::QuestionOption {
            label: "Blue".to_string(),
            description: "Calm".to_string(),
          },
        ],
        multi_select: false,
        confirmation: false,
        default: Vec::new(),
        required: false,
      },
      Question {
        question: "Pick sizes".to_string(),
        header: "".to_string(),
        options: vec![
          crate::llm::session::QuestionOption {
            label: "Small".to_string(),
            description: "".to_string(),
          },
          crate::llm::session::QuestionOption {
            label: "Large".to_string(),
            description: "".to_string(),
          },
        ],
        multi_select: true,
        confirmation: false,
        default: Vec::new(),
        required: false,
      },
    ],
    current_question_idx: 0,
    answers: Vec::new(),
    selected_option_idx: 0,
  }
}

#[test]
fn test_question_keyboard_down_navigation() {
  let (session_handle, mut cmd_rx) = make_session_handle();
  let mut data = AppData::new();
  let mut view = ChatView::new(
    &data,
    session_handle,
    std::sync::Arc::new(Runtime::for_test(Config::default())),
  );
  data.pending_questions = Some(make_pending_questions());

  // Press Down to move from option 0 to option 1
  view.handle_key(&mut data, KeyEvent::from(KeyCode::Down));

  let pq = data.pending_questions.as_ref().unwrap();
  assert_eq!(pq.selected_option_idx, 1);
  assert!(cmd_rx.try_recv().is_err()); // no command sent yet
}

#[test]
fn test_question_single_select_enter() {
  let (session_handle, _cmd_rx) = make_session_handle();
  let mut data = AppData::new();
  let mut view = ChatView::new(
    &data,
    session_handle,
    std::sync::Arc::new(Runtime::for_test(Config::default())),
  );
  data.pending_questions = Some(make_pending_questions());

  // Press Enter to confirm first question (single-select)
  view.handle_key(&mut data, KeyEvent::from(KeyCode::Enter));

  // Should move to next question
  let pq = data.pending_questions.as_ref().unwrap();
  assert_eq!(pq.current_question_idx, 1);
  assert_eq!(pq.answers.len(), 1);
  assert_eq!(pq.answers[0], vec![0]); // first option selected
}

#[test]
fn test_question_multi_select_toggle() {
  let (session_handle, _cmd_rx) = make_session_handle();
  let mut data = AppData::new();
  let mut view = ChatView::new(
    &data,
    session_handle,
    std::sync::Arc::new(Runtime::for_test(Config::default())),
  );
  data.pending_questions = Some(make_pending_questions());

  // Move to second question (multi-select)
  view.handle_key(&mut data, KeyEvent::from(KeyCode::Enter));
  assert_eq!(
    data
      .pending_questions
      .as_ref()
      .unwrap()
      .current_question_idx,
    1
  );

  // Toggle option 0 with Space
  view.handle_key(&mut data, KeyEvent::from(KeyCode::Char(' ')));
  let pq = data.pending_questions.as_ref().unwrap();
  assert_eq!(pq.answers[1], vec![0]);

  // Move down and toggle option 1
  view.handle_key(&mut data, KeyEvent::from(KeyCode::Down));
  view.handle_key(&mut data, KeyEvent::from(KeyCode::Char(' ')));
  let pq = data.pending_questions.as_ref().unwrap();
  assert_eq!(pq.answers[1], vec![0, 1]);

  // Toggle option 0 off
  view.handle_key(&mut data, KeyEvent::from(KeyCode::Up));
  view.handle_key(&mut data, KeyEvent::from(KeyCode::Char(' ')));
  let pq = data.pending_questions.as_ref().unwrap();
  assert_eq!(pq.answers[1], vec![1]);
}

#[test]
fn test_question_complete_all_and_submit() {
  let (session_handle, mut cmd_rx) = make_session_handle();
  let mut data = AppData::new();
  let mut view = ChatView::new(
    &data,
    session_handle,
    std::sync::Arc::new(Runtime::for_test(Config::default())),
  );
  data.pending_questions = Some(make_pending_questions());

  // Answer first question (single-select, option 1)
  view.handle_key(&mut data, KeyEvent::from(KeyCode::Down));
  view.handle_key(&mut data, KeyEvent::from(KeyCode::Enter));

  // Answer second question (multi-select, toggle option 0)
  view.handle_key(&mut data, KeyEvent::from(KeyCode::Char(' ')));
  view.handle_key(&mut data, KeyEvent::from(KeyCode::Enter));

  // pending_questions should be cleared and command sent
  assert!(data.pending_questions.is_none());
  let cmd = cmd_rx.try_recv().expect("Expected AnswerQuestions command");
  match cmd {
    SessionCommand::AnswerQuestions {
      tool_call_id,
      answers,
      dismissed,
    } => {
      assert_eq!(tool_call_id, "call-123");
      assert!(!dismissed);
      assert_eq!(answers.len(), 2);
      assert_eq!(answers[0], vec![1]); // Blue
      assert_eq!(answers[1], vec![0]); // Small
    }
    other => panic!("Expected AnswerQuestions, got {:?}", other),
  }
}

#[test]
fn test_question_dismiss_with_q() {
  let (session_handle, mut cmd_rx) = make_session_handle();
  let mut data = AppData::new();
  let mut view = ChatView::new(
    &data,
    session_handle,
    std::sync::Arc::new(Runtime::for_test(Config::default())),
  );
  data.pending_questions = Some(make_pending_questions());

  view.handle_key(&mut data, KeyEvent::from(KeyCode::Char('q')));

  assert!(data.pending_questions.is_none());
  let cmd = cmd_rx.try_recv().expect("Expected AnswerQuestions command");
  match cmd {
    SessionCommand::AnswerQuestions {
      tool_call_id,
      answers,
      dismissed,
    } => {
      assert_eq!(tool_call_id, "call-123");
      assert!(dismissed);
      assert!(answers.is_empty());
    }
    other => panic!("Expected AnswerQuestions, got {:?}", other),
  }
}

#[test]
fn test_question_dismiss_with_esc() {
  let (session_handle, mut cmd_rx) = make_session_handle();
  let mut data = AppData::new();
  let mut view = ChatView::new(
    &data,
    session_handle,
    std::sync::Arc::new(Runtime::for_test(Config::default())),
  );
  data.pending_questions = Some(make_pending_questions());

  view.handle_key(&mut data, KeyEvent::from(KeyCode::Esc));

  assert!(data.pending_questions.is_none());
  let cmd = cmd_rx.try_recv().expect("Expected AnswerQuestions command");
  match cmd {
    SessionCommand::AnswerQuestions {
      tool_call_id,
      answers,
      dismissed,
    } => {
      assert_eq!(tool_call_id, "call-123");
      assert!(dismissed);
      assert!(answers.is_empty());
    }
    other => panic!("Expected AnswerQuestions, got {:?}", other),
  }
}

#[test]
fn test_question_digit_quick_select() {
  let (session_handle, _cmd_rx) = make_session_handle();
  let mut data = AppData::new();
  let mut view = ChatView::new(
    &data,
    session_handle,
    std::sync::Arc::new(Runtime::for_test(Config::default())),
  );
  data.pending_questions = Some(make_pending_questions());

  // Press '2' to select option 1 (0-indexed) and auto-confirm single-select
  view.handle_key(&mut data, KeyEvent::from(KeyCode::Char('2')));

  let pq = data.pending_questions.as_ref().unwrap();
  assert_eq!(pq.current_question_idx, 1); // moved to next question
  assert_eq!(pq.answers[0], vec![1]); // Blue selected
}

fn make_confirmation_question() -> PendingQuestions {
  PendingQuestions {
    tool_call_id: "call-confirm".to_string(),
    questions: vec![Question {
      question: "Are you sure?".to_string(),
      header: "Confirm".to_string(),
      options: vec![
        crate::llm::session::QuestionOption {
          label: "Yes".to_string(),
          description: String::new(),
        },
        crate::llm::session::QuestionOption {
          label: "No".to_string(),
          description: String::new(),
        },
      ],
      multi_select: false,
      confirmation: true,
      default: Vec::new(),
      required: false,
    }],
    current_question_idx: 0,
    answers: Vec::new(),
    selected_option_idx: 0,
  }
}

#[test]
fn test_question_confirmation_yes() {
  let (session_handle, mut cmd_rx) = make_session_handle();
  let mut data = AppData::new();
  let mut view = ChatView::new(
    &data,
    session_handle,
    std::sync::Arc::new(Runtime::for_test(Config::default())),
  );
  data.pending_questions = Some(make_confirmation_question());

  view.handle_key(&mut data, KeyEvent::from(KeyCode::Char('y')));

  assert!(data.pending_questions.is_none());
  let cmd = cmd_rx.try_recv().expect("Expected AnswerQuestions command");
  match cmd {
    SessionCommand::AnswerQuestions {
      tool_call_id,
      answers,
      dismissed,
    } => {
      assert_eq!(tool_call_id, "call-confirm");
      assert!(!dismissed);
      assert_eq!(answers, vec![vec![0]]); // Yes = index 0
    }
    other => panic!("Expected AnswerQuestions, got {:?}", other),
  }
}

#[test]
fn test_question_confirmation_no() {
  let (session_handle, mut cmd_rx) = make_session_handle();
  let mut data = AppData::new();
  let mut view = ChatView::new(
    &data,
    session_handle,
    std::sync::Arc::new(Runtime::for_test(Config::default())),
  );
  data.pending_questions = Some(make_confirmation_question());

  view.handle_key(&mut data, KeyEvent::from(KeyCode::Char('n')));

  assert!(data.pending_questions.is_none());
  let cmd = cmd_rx.try_recv().expect("Expected AnswerQuestions command");
  match cmd {
    SessionCommand::AnswerQuestions {
      tool_call_id,
      answers,
      dismissed,
    } => {
      assert_eq!(tool_call_id, "call-confirm");
      assert!(!dismissed);
      assert_eq!(answers, vec![vec![1]]); // No = index 1
    }
    other => panic!("Expected AnswerQuestions, got {:?}", other),
  }
}

#[test]
fn test_question_default_value_preselected() {
  let (session_handle, _cmd_rx) = make_session_handle();
  let mut data = AppData::new();
  let mut view = ChatView::new(
    &data,
    session_handle,
    std::sync::Arc::new(Runtime::for_test(Config::default())),
  );
  data.pending_questions = Some(PendingQuestions {
    tool_call_id: "call-def".to_string(),
    questions: vec![Question {
      question: "Pick one".to_string(),
      header: "Choice".to_string(),
      options: vec![
        crate::llm::session::QuestionOption {
          label: "A".to_string(),
          description: String::new(),
        },
        crate::llm::session::QuestionOption {
          label: "B".to_string(),
          description: String::new(),
        },
      ],
      multi_select: false,
      confirmation: false,
      default: vec![1], // default is B
      required: false,
    }],
    current_question_idx: 0,
    answers: Vec::new(),
    selected_option_idx: 0,
  });

  // Default should auto-select option 1 and move to next/submit
  // Since it's single-select with default, the default is already applied
  // Check that pressing Enter submits the default
  view.handle_key(&mut data, KeyEvent::from(KeyCode::Enter));
  assert!(data.pending_questions.is_none());
}

#[test]
fn test_question_required_blocks_empty() {
  let (session_handle, _cmd_rx) = make_session_handle();
  let mut data = AppData::new();
  let mut view = ChatView::new(
    &data,
    session_handle,
    std::sync::Arc::new(Runtime::for_test(Config::default())),
  );
  data.pending_questions = Some(PendingQuestions {
    tool_call_id: "call-req".to_string(),
    questions: vec![Question {
      question: "Required?".to_string(),
      header: "".to_string(),
      options: vec![
        crate::llm::session::QuestionOption {
          label: "Opt1".to_string(),
          description: String::new(),
        },
        crate::llm::session::QuestionOption {
          label: "Opt2".to_string(),
          description: String::new(),
        },
      ],
      multi_select: false,
      confirmation: false,
      default: Vec::new(),
      required: true,
    }],
    current_question_idx: 0,
    answers: Vec::new(),
    selected_option_idx: 0,
  });

  view.handle_key(&mut data, KeyEvent::from(KeyCode::Enter));

  // Should still be pending because required and nothing selected
  assert!(data.pending_questions.is_some());
  let pq = data.pending_questions.as_ref().unwrap();
  assert_eq!(pq.current_question_idx, 0);

  // Now select an option
  view.handle_key(&mut data, KeyEvent::from(KeyCode::Char('1')));
  view.handle_key(&mut data, KeyEvent::from(KeyCode::Enter));
  assert!(data.pending_questions.is_none());
}

#[test]
fn test_slash_completion_basic() {
  let (session_handle, _cmd_rx) = make_session_handle();
  let data = AppData::new();
  let mut view = ChatView::new(
    &data,
    session_handle,
    std::sync::Arc::new(Runtime::for_test(Config::default())),
  );

  // Type '/'
  view.handle_key(&mut AppData::new(), KeyEvent::from(KeyCode::Char('/')));
  assert!(view.slash_completion.active);
  assert_eq!(view.slash_completion.filtered.len(), SLASH_COMMANDS.len());

  // Type 't' → "/t" matches "task" and "thinking"
  view.handle_key(&mut AppData::new(), KeyEvent::from(KeyCode::Char('t')));
  assert!(view.slash_completion.active);
  assert_eq!(view.slash_completion.filtered.len(), 2);
  let names: Vec<&str> = view
    .slash_completion
    .filtered
    .iter()
    .map(|index| SLASH_COMMANDS[*index].name)
    .collect();
  assert!(names.contains(&"task"), "missing task: {names:?}");
  assert!(names.contains(&"thinking"), "missing thinking: {names:?}");

  // Type 'a' â†’ "/ta"
  view.handle_key(&mut AppData::new(), KeyEvent::from(KeyCode::Char('a')));
  assert!(view.slash_completion.active);
  assert_eq!(view.slash_completion.filtered.len(), 1);

  // Type 's' â†’ "/tas"
  view.handle_key(&mut AppData::new(), KeyEvent::from(KeyCode::Char('s')));
  assert!(view.slash_completion.active);
  assert_eq!(view.slash_completion.filtered.len(), 1);

  // Type 'k' â†’ "/task"
  view.handle_key(&mut AppData::new(), KeyEvent::from(KeyCode::Char('k')));
  assert!(view.slash_completion.active);
  assert_eq!(view.slash_completion.filtered.len(), 1);

  // Type ' ' â†’ "/task "
  view.handle_key(&mut AppData::new(), KeyEvent::from(KeyCode::Char(' ')));
  assert!(!view.slash_completion.active);
}

#[test]
fn test_slash_completion_filtering() {
  let (session_handle, _cmd_rx) = make_session_handle();
  let data = AppData::new();
  let mut view = ChatView::new(
    &data,
    session_handle,
    std::sync::Arc::new(Runtime::for_test(Config::default())),
  );

  // "/c" should match "clear"
  view.handle_key(&mut AppData::new(), KeyEvent::from(KeyCode::Char('/')));
  view.handle_key(&mut AppData::new(), KeyEvent::from(KeyCode::Char('c')));
  assert!(view.slash_completion.active);
  assert_eq!(view.slash_completion.filtered.len(), 1);
  assert_eq!(
    SLASH_COMMANDS[view.slash_completion.filtered[0]].name,
    "clear"
  );

  // "/r" should offer every command that starts with r (reset, resume, rename)
  view.handle_key(&mut AppData::new(), KeyEvent::from(KeyCode::Backspace));
  view.handle_key(&mut AppData::new(), KeyEvent::from(KeyCode::Char('r')));
  assert!(view.slash_completion.active);
  let names: Vec<&str> = view
    .slash_completion
    .filtered
    .iter()
    .map(|index| SLASH_COMMANDS[*index].name)
    .collect();
  assert!(names.contains(&"reset"), "{names:?}");
  assert!(names.contains(&"resume"), "{names:?}");
  assert!(names.contains(&"rename"), "{names:?}");

  // "/x" should match nothing
  view.handle_key(&mut AppData::new(), KeyEvent::from(KeyCode::Backspace));
  view.handle_key(&mut AppData::new(), KeyEvent::from(KeyCode::Char('x')));
  assert!(!view.slash_completion.active);
}

#[test]
fn test_slash_completion_navigation_and_accept() {
  let (session_handle, _cmd_rx) = make_session_handle();
  let data = AppData::new();
  let mut view = ChatView::new(
    &data,
    session_handle,
    std::sync::Arc::new(Runtime::for_test(Config::default())),
  );

  // Type "/" to show all commands
  view.handle_key(&mut AppData::new(), KeyEvent::from(KeyCode::Char('/')));
  assert!(view.slash_completion.active);
  assert_eq!(view.slash_completion.filtered.len(), SLASH_COMMANDS.len());
  assert_eq!(view.slash_completion.selected, 0);

  // Press Down to select next item
  view.handle_key(&mut AppData::new(), KeyEvent::from(KeyCode::Down));
  assert_eq!(view.slash_completion.selected, 1);

  // Press Up to go back
  view.handle_key(&mut AppData::new(), KeyEvent::from(KeyCode::Up));
  assert_eq!(view.slash_completion.selected, 0);

  // Press Enter to accept the first command (task)
  view.handle_key(&mut AppData::new(), KeyEvent::from(KeyCode::Enter));
  assert!(!view.slash_completion.active);
  assert_eq!(view.input.text(), "/task ");

  // Press Esc to cancel completion
  view.input.replace_text("/".to_string());
  view.update_slash_completion();
  assert!(view.slash_completion.active);
  view.handle_key(&mut AppData::new(), KeyEvent::from(KeyCode::Esc));
  assert!(!view.slash_completion.active);
}

fn render_design_screen(data: &AppData, width: u16, height: u16, input: &str) -> String {
  use ratatui::{Terminal, backend::TestBackend};
  let (handle, _rx) = make_session_handle();
  let mut view = ChatView::new(data, handle, Arc::new(Runtime::for_test(Config::default())));
  for c in input.chars() {
    view.input.insert_char(c);
  }
  let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
  terminal.draw(|f| view.draw(f, data)).unwrap();
  let cursor = terminal.get_cursor_position().unwrap();
  assert!(cursor.x < width && cursor.y < height);
  let buffer = terminal.backend().buffer();
  (0..height)
    .map(|y| {
      (0..width)
        .map(|x| buffer[(x, y)].symbol())
        .collect::<String>()
    })
    .collect::<Vec<_>>()
    .join("\n")
}

#[test]
fn design_welcome_uses_shared_shell() {
  let screen = render_design_screen(&AppData::new(), 100, 30, "");
  assert!(screen.contains("What will you build today?"));
  assert!(screen.contains("Ask anything about your code"));
  assert!(screen.contains("Zarncode"));
}

#[test]
fn design_long_history_keeps_composer_and_latest_message_visible() {
  let mut data = AppData::new();
  data.chat_history = (0..50)
    .map(|i| ChatMessage::User {
      content: format!("message-{i}"),
    })
    .collect();
  let screen = render_design_screen(&data, 100, 24, "next request");
  assert!(screen.contains("message-49"));
  assert_eq!(screen.matches("message-49").count(), 1);
  assert!(screen.contains("next request"));
  assert!(!screen.contains("message-0 "));
}

#[test]
fn design_handles_narrow_screens_and_long_unicode_input() {
  for (w, h) in [(1, 1), (20, 8), (40, 12), (80, 24)] {
    render_design_screen(&AppData::new(), w, h, &"abä¸–ç•Œ\n".repeat(80));
  }
}

#[test]
fn design_pending_panels_are_not_displaced_by_history() {
  let mut data = AppData::new();
  data.chat_history = (0..40)
    .map(|_| ChatMessage::Assistant {
      content: "history".to_string(),
      thinking_content: Some("reasoning".to_string()),
    })
    .collect();
  data.pending_questions = Some(make_pending_questions());
  data.pending_approval = Some(crate::cli::app::PendingApproval {
    approval_id: "a".into(),
    source_id: "s".into(),
    source_kind: None,
    tool_call_id: "t".into(),
    name: "WriteFile".into(),
    diff_preview: None,
    position: 1,
    total: 1,
  });
  let screen = render_design_screen(&data, 100, 40, "");
  assert!(screen.contains("Tool approval"));
  assert!(screen.contains("Which color?"));
  assert!(screen.contains("Ask anything about your code"));
}

#[test]
fn design_mixed_streams_preserve_pending_question_space() {
  use ratatui::{Terminal, backend::TestBackend};
  let mut data = AppData::new();
  data.pending_questions = Some(make_pending_questions());
  data.streaming_response = Arc::new(vec![
    StreamingChunk::Normal("first fragment".into()),
    StreamingChunk::Thinking("reasoning".into()),
    StreamingChunk::Normal("last fragment".into()),
  ]);
  let (handle, _rx) = make_session_handle();
  let mut view = ChatView::new(
    &data,
    handle,
    Arc::new(Runtime::for_test(Config::default())),
  );
  view.enter_streaming();
  let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
  terminal.draw(|f| view.draw(f, &data)).unwrap();
  let buffer = terminal.backend().buffer();
  let screen = (0..40)
    .map(|y| {
      (0..100)
        .map(|x| buffer[(x, y)].symbol())
        .collect::<String>()
    })
    .collect::<Vec<_>>()
    .join("\n");
  assert!(screen.contains("last fragment"));
  assert!(screen.contains("Which color?"));
  assert!(screen.contains("Ask anything about your code"));
}

#[test]
fn usage_command_shows_exact_local_totals_without_model_request() {
  let mut data = AppData::new();
  let (handle, mut rx) = make_session_handle();
  let mut view = ChatView::new(
    &data,
    handle,
    Arc::new(Runtime::for_test(Config::default())),
  );
  let command = crate::cli::slash::parse_slash_command("/usage").unwrap();
  assert!(view.handle_slash_command(&mut data, command).is_none());
  let ChatMessage::System { content, .. } = data.chat_history.last().unwrap() else {
    panic!("usage report expected");
  };
  assert!(content.contains("Input 0 | Output 0 | Cache 0 | Total 0"));
  assert!(content.contains("5h rolling"));
  assert!(content.contains("Week (7d rolling)"));
  assert!(content.contains("not account quota"));
  assert!(rx.try_recv().is_err());
}

#[test]
fn busy_input_queues_steering_without_claiming_delivery() {
  let mut data = AppData::new();
  let (handle, mut rx) = make_session_handle();
  let mut view = ChatView::new(
    &data,
    handle,
    Arc::new(Runtime::for_test(Config::default())),
  );
  view.enter_streaming();
  view.input.replace_text("please change direction".into());
  view.submit_message(&mut data);
  assert!(view.input.is_empty());
  assert!(data.chat_history.is_empty());
  assert!(
    matches!(rx.try_recv().unwrap(), SessionCommand::EnqueueInput { kind: crate::llm::session::input_queue::InputKind::Steer, content, .. } if content == "please change direction")
  );
}

#[test]
fn alt_enter_never_approves_a_pending_tool_and_composer_can_queue_followups() {
  let mut data = AppData::new();
  data.pending_approval = Some(crate::cli::app::PendingApproval {
    approval_id: "a".into(),
    source_id: "s".into(),
    source_kind: None,
    tool_call_id: "t".into(),
    name: "WriteFile".into(),
    diff_preview: None,
    position: 1,
    total: 1,
  });
  let (handle, mut rx) = make_session_handle();
  let mut view = ChatView::new(
    &data,
    handle,
    Arc::new(Runtime::for_test(Config::default())),
  );
  view.input.replace_text("later work".into());
  view.handle_key(&mut data, KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
  assert!(data.pending_approval.is_some());
  assert!(rx.try_recv().is_err());
  view.handle_key(
    &mut data,
    KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
  );
  view.handle_key(&mut data, KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
  assert!(data.pending_approval.is_some());
  assert!(matches!(
    rx.try_recv().unwrap(),
    SessionCommand::EnqueueInput {
      kind: crate::llm::session::input_queue::InputKind::FollowUp,
      ..
    }
  ));
}

#[test]
fn a_closed_session_keeps_unsent_draft_text() {
  let mut data = AppData::new();
  let (handle, rx) = make_session_handle();
  drop(rx);
  let mut view = ChatView::new(
    &data,
    handle,
    Arc::new(Runtime::for_test(Config::default())),
  );
  view.input.replace_text("do not lose this".into());
  view.submit_message(&mut data);
  assert_eq!(view.input.text(), "do not lose this");
}

#[test]
fn a_full_queue_keeps_the_new_draft_in_the_editor() {
  let mut data = AppData::new();
  for index in 0..64 {
    data
      .input_queue
      .enqueue(
        format!("{index}"),
        crate::llm::session::input_queue::InputKind::FollowUp,
        "pending".into(),
      )
      .unwrap();
  }
  let (handle, mut rx) = make_session_handle();
  let mut view = ChatView::new(
    &data,
    handle,
    Arc::new(Runtime::for_test(Config::default())),
  );
  view.enter_streaming();
  view.input.replace_text("keep this draft".into());
  view.submit_message(&mut data);
  assert_eq!(view.input.text(), "keep this draft");
  assert!(rx.try_recv().is_err());
}

#[test]
#[ignore]
fn export_priority_queue_scene() {
  let mut data = AppData::new();
  data.chat_history.push(ChatMessage::User {
    content: "Review this module without changing files.".into(),
  });
  data.chat_history.push(ChatMessage::Assistant { content: "This is a seeded UI demonstration of the real renderer.\nQueued directions stay separate until a safe boundary.".into(), thinking_content: None });
  data.streaming_response = Arc::new(vec![StreamingChunk::Normal(
    "Demo work is in progress. No live inference is running.".into(),
  )]);
  data
    .input_queue
    .enqueue(
      "82a09e6b-demo-steer".into(),
      crate::llm::session::input_queue::InputKind::Steer,
      "Focus on security and edge cases.".into(),
    )
    .unwrap();
  data
    .input_queue
    .enqueue(
      "a6b091e2-demo-follow".into(),
      crate::llm::session::input_queue::InputKind::FollowUp,
      "Summarize the findings after this task.".into(),
    )
    .unwrap();
  data.input_queue.paused = true;
  let (handle, _rx) = make_session_handle();
  let config = Config {
    default_model: "demo / no provider".into(),
    ..Default::default()
  };
  let mut runtime = Runtime::for_test(config);
  runtime.active_agent = Some("reviewer".into());
  runtime.max_steps = 3;
  let mut view = ChatView::new(&data, handle, Arc::new(runtime));
  view.prompt = "demo workspace".into();
  view.enter_streaming();
  view
    .input
    .replace_text("Inspect the queue before continuing.".into());
  crate::view::chrome::export_snapshot_svg("artifacts/zarncode-priorities.svg", |frame| {
    view.draw(frame, &data)
  });
}
