use std::process::{Command, Stdio};

fn binary() -> &'static str {
  env!("CARGO_BIN_EXE_zarncode")
}

#[test]
fn help_advertises_automation_mode() {
  let output = Command::new(binary()).arg("--help").output().unwrap();
  let help = String::from_utf8(output.stdout).unwrap();
  for flag in ["--print", "--output-format", "jsonl", "--model"] {
    assert!(help.contains(flag), "missing {flag}: {help}");
  }
}

#[test]
fn empty_headless_input_is_a_structured_usage_error() {
  let output = Command::new(binary())
    .args(["--print", "--output-format", "jsonl"])
    .stdin(Stdio::null())
    .output()
    .unwrap();
  assert_eq!(output.status.code(), Some(2));
  let lines: Vec<serde_json::Value> = String::from_utf8(output.stdout)
    .unwrap()
    .lines()
    .map(|s| serde_json::from_str(s).unwrap())
    .collect();
  let result = lines.last().expect("terminal result event");
  assert_eq!(result["type"], "result");
  assert_eq!(result["status"], "invalid_input");
  assert_eq!(result["schema_version"], 1);
}

use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{
  Arc, Mutex,
  atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::Duration;

#[derive(Clone)]
enum Reply {
  DelegateThenText,
  ParallelDelegate,
  DelegateThenNested,
  Text,
  WriteThenText(String),
  Error,
}

struct Server {
  url: String,
  requests: Arc<Mutex<Vec<Value>>>,
  stopped: Arc<AtomicBool>,
  task: Option<thread::JoinHandle<()>>,
}

impl Server {
  fn new(reply: Reply) -> Self {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let stopped = Arc::new(AtomicBool::new(false));
    let captured = requests.clone();
    let stop = stopped.clone();
    let task = thread::spawn(move || {
      while !stop.load(Ordering::SeqCst) {
        let (mut stream, _) = match listener.accept() {
          Ok(value) => value,
          Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
            thread::sleep(Duration::from_millis(5));
            continue;
          }
          Err(_) => break,
        };
        stream
          .set_read_timeout(Some(Duration::from_secs(3)))
          .unwrap();
        let Some(request) = read_request(&mut stream) else {
          continue;
        };
        let count = {
          let mut requests = captured.lock().unwrap();
          requests.push(request);
          requests.len()
        };
        let (status, content_type, body) = match &reply {
          Reply::Error => (
            "401 Unauthorized",
            "application/json",
            json!({"error":{"message":"mock authentication failure"}}).to_string(),
          ),
          _ => {
            let delta = match &reply {
              Reply::ParallelDelegate if count == 1 => {
                json!({"tool_calls":(0..3).map(|i| json!({"index":i,"id":format!("parallel-{i}"),"type":"function","function":{"name":"Agent","arguments":json!({"subagent_type":"reviewer","description":"Parallel attempt","prompt":"Review safely"}).to_string()}})).collect::<Vec<_>>()})
              }
              Reply::DelegateThenText | Reply::DelegateThenNested if count == 1 => {
                json!({"tool_calls":[{"index":0,"id":"agent-1","type":"function","function":{"name":"Agent","arguments":json!({"subagent_type":"reviewer","description":"Review safely","prompt":"Review the supplied task"}).to_string()}}]})
              }
              Reply::DelegateThenNested if count == 2 => {
                json!({"tool_calls":[{"index":0,"id":"nested-1","type":"function","function":{"name":"Agent","arguments":json!({"subagent_type":"reviewer","description":"Nested attempt","prompt":"Do not permit nested delegation"}).to_string()}}]})
              }
              Reply::WriteThenText(path) if count == 1 => {
                json!({"tool_calls":[{"index":0,"id":"write-1","type":"function","function":{"name":"WriteFile","arguments":json!({"path":path,"content":"approved fixture"}).to_string()}}]})
              }
              _ => json!({"content":"mock answer"}),
            };
            let chunk = json!({"id":"mock-response","object":"chat.completion.chunk","created":0,"model":"mock-model","choices":[{"index":0,"delta":delta}],"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12,"cached_tokens":3}});
            (
              "200 OK",
              "text/event-stream",
              format!("data: {chunk}\n\ndata: [DONE]\n\n"),
            )
          }
        };
        if matches!(&reply, Reply::ParallelDelegate) && count == 2 {
          thread::sleep(Duration::from_millis(150));
        }
        let response = format!(
          "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
          body.len()
        );
        let _ = stream.write_all(response.as_bytes());
      }
    });
    Self {
      url,
      requests,
      stopped,
      task: Some(task),
    }
  }
}

impl Drop for Server {
  fn drop(&mut self) {
    self.stopped.store(true, Ordering::SeqCst);
    if let Some(task) = self.task.take() {
      task.join().unwrap();
    }
  }
}

fn read_request(stream: &mut TcpStream) -> Option<Value> {
  let mut data = Vec::new();
  loop {
    let mut buffer = [0; 4096];
    let count = stream.read(&mut buffer).ok()?;
    if count == 0 {
      return None;
    }
    data.extend_from_slice(&buffer[..count]);
    let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") else {
      continue;
    };
    let headers = std::str::from_utf8(&data[..end]).ok()?;
    let length: usize = headers.lines().find_map(|line| {
      let (name, value) = line.split_once(':')?;
      name
        .eq_ignore_ascii_case("content-length")
        .then(|| value.trim().parse().ok())
        .flatten()
    })?;
    if data.len() >= end + 4 + length {
      return serde_json::from_slice(&data[end + 4..end + 4 + length]).ok();
    }
  }
}

fn configuration(server: &Server) -> tempfile::TempDir {
  let directory = tempfile::tempdir().unwrap();
  let data = directory.path().join("data");
  std::fs::create_dir_all(data.join("prompts/tools")).unwrap();
  std::fs::write(
    data.join("prompts/tools/write.md"),
    include_str!("../prompts/tools/file/write.md"),
  )
  .unwrap();
  std::fs::write(
    data.join("prompts/tools/read.md"),
    include_str!("../prompts/tools/file/read.md"),
  )
  .unwrap();
  std::fs::write(
    data.join("prompts/tools/agent.md"),
    include_str!("../prompts/tools/agent/Agent.md"),
  )
  .unwrap();
  let config = format!(
    "dir = {}\ndefault_model = \"base\"\nyolo = true\nauto_approve = [\"WriteFile\"]\n[providers.mock]\ntype = \"openai-compatible\"\nbase_url = {}\napi_key = \"test-fixture-key\"\n[models.base]\nprovider = \"mock\"\nmodel = \"base-model\"\n[models.alternate]\nprovider = \"mock\"\nmodel = \"alternate-model\"\n[retry]\nmax_attempts = 1\n",
    json!(data.to_str().unwrap()),
    json!(server.url)
  );
  std::fs::write(directory.path().join("config.toml"), config).unwrap();
  directory
}

fn invoke(config: &tempfile::TempDir, arguments: &[&str], input: &str) -> std::process::Output {
  let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
  invoke_in(config, arguments, input, &cwd)
}

/// Same helper, but with an explicit working directory. The file tools refuse
/// writes outside the workspace, so a test that expects a real file has to run
/// the agent inside the directory it writes to.
fn invoke_in(
  config: &tempfile::TempDir,
  arguments: &[&str],
  input: &str,
  cwd: &std::path::Path,
) -> std::process::Output {
  let mut child = Command::new(binary())
    .arg("--config")
    .arg(config.path())
    .args(arguments)
    .current_dir(cwd)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
  child
    .stdin
    .take()
    .unwrap()
    .write_all(input.as_bytes())
    .unwrap();
  child.wait_with_output().unwrap()
}

fn events(output: &std::process::Output) -> Vec<Value> {
  let text = String::from_utf8(output.stdout.clone()).unwrap();
  assert!(!text.contains('\u{1b}'));
  text
    .lines()
    .map(|line| serde_json::from_str(line).unwrap())
    .collect()
}

#[test]
fn print_mode_uses_real_actor_with_piped_input_and_plain_output() {
  let server = Server::new(Reply::Text);
  let config = configuration(&server);
  let output = invoke(&config, &["--print"], "explain this code\n");
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert_eq!(String::from_utf8(output.stdout).unwrap(), "mock answer\n");
  let requests = server.requests.lock().unwrap();
  let messages = requests[0]["messages"].as_array().unwrap();
  assert_eq!(messages.last().unwrap()["content"], "explain this code\n");
}

#[test]
fn jsonl_stream_has_terminal_result_cache_usage_and_model_override() {
  let server = Server::new(Reply::Text);
  let config = configuration(&server);
  let output = invoke(
    &config,
    &[
      "--print",
      "review",
      "--output-format",
      "jsonl",
      "--model",
      "alternate",
    ],
    "piped context",
  );
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  let events = events(&output);
  assert!(events.iter().all(|event| event["schema_version"] == 1));
  let result = events.last().unwrap();
  assert_eq!(result["type"], "result");
  assert_eq!(result["status"], "success");
  assert_eq!(result["text"], "mock answer");
  assert_eq!(result["last_request_usage"]["cache"], 3);
  assert_eq!(result["last_request_usage"]["total"], 12);
  let requests = server.requests.lock().unwrap();
  assert_eq!(requests[0]["model"], "alternate-model");
  let messages = requests[0]["messages"].as_array().unwrap();
  assert_eq!(
    messages.last().unwrap()["content"],
    "review\n\n--- stdin ---\npiped context"
  );
}

#[test]
fn unattended_write_is_denied_despite_persisted_config_grants() {
  let directory = tempfile::tempdir().unwrap();
  let marker = directory.path().join("must-not-exist.txt");
  let server = Server::new(Reply::WriteThenText(marker.to_string_lossy().to_string()));
  let config = configuration(&server);
  let output = invoke(
    &config,
    &["--print", "write", "--output-format", "jsonl"],
    "",
  );
  assert_eq!(
    output.status.code(),
    Some(3),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(!marker.exists());
  assert_eq!(
    events(&output).last().unwrap()["status"],
    "approval_required"
  );
}

#[test]
fn explicit_yolo_allows_the_normal_tool_loop_then_prints_only_final_answer() {
  let directory = tempfile::tempdir().unwrap();
  let marker = directory.path().join("approved.txt");
  let server = Server::new(Reply::WriteThenText(marker.to_string_lossy().to_string()));
  let config = configuration(&server);
  let output = invoke_in(&config, &["--print", "write", "--yolo"], "", directory.path());
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert_eq!(std::fs::read_to_string(marker).unwrap(), "approved fixture");
  assert_eq!(String::from_utf8(output.stdout).unwrap(), "mock answer\n");
  assert_eq!(server.requests.lock().unwrap().len(), 2);
}

#[test]
fn authentication_errors_return_nonzero_structured_results() {
  let server = Server::new(Reply::Error);
  let config = configuration(&server);
  let output = invoke(
    &config,
    &["--print", "hello", "--output-format", "jsonl"],
    "",
  );
  assert_eq!(output.status.code(), Some(1));
  assert_eq!(events(&output).last().unwrap()["status"], "error");
}

#[test]
fn resume_keeps_session_identity_and_does_not_restore_yolo_permission() {
  let server = Server::new(Reply::Text);
  let config = configuration(&server);
  let first = invoke(
    &config,
    &["--print", "first", "--output-format", "jsonl"],
    "",
  );
  assert!(first.status.success());
  let id = events(&first)[0]["session_id"]
    .as_str()
    .unwrap()
    .to_string();
  let meta = config
    .path()
    .join("data/sessions")
    .join(&id)
    .join("meta.json");
  let mut value: Value = serde_json::from_slice(&std::fs::read(&meta).unwrap()).unwrap();
  value["yolo"] = json!(true);
  std::fs::write(meta, serde_json::to_vec(&value).unwrap()).unwrap();
  let marker = config.path().join("forbidden-write.txt");
  let next = Server::new(Reply::WriteThenText(marker.to_string_lossy().to_string()));
  let path = config.path().join("config.toml");
  let text = std::fs::read_to_string(&path)
    .unwrap()
    .replace(&server.url, &next.url);
  std::fs::write(path, text).unwrap();
  let second = invoke(
    &config,
    &[
      "--print",
      "second",
      "--continue",
      "--output-format",
      "jsonl",
    ],
    "",
  );
  assert_eq!(
    second.status.code(),
    Some(3),
    "{}",
    String::from_utf8_lossy(&second.stderr)
  );
  assert_eq!(events(&second)[0]["session_id"], id);
  assert_eq!(
    events(&second).last().unwrap()["status"],
    "approval_required"
  );
  assert!(!marker.exists());
  assert_eq!(
    next.requests.lock().unwrap()[0]["messages"]
      .as_array()
      .unwrap()
      .iter()
      .filter(|m| m["role"] == "user")
      .count(),
    2
  );
}

#[test]
fn unknown_model_and_ambiguous_session_options_fail_before_inference() {
  let server = Server::new(Reply::Text);
  let config = configuration(&server);
  let output = invoke(
    &config,
    &[
      "--print",
      "hello",
      "--model",
      "not-configured",
      "--output-format",
      "jsonl",
    ],
    "",
  );
  assert_eq!(output.status.code(), Some(1));
  assert_eq!(events(&output).last().unwrap()["status"], "error");
  assert!(server.requests.lock().unwrap().is_empty());
  let output = invoke(
    &config,
    &["--print", "hello", "--session", "id", "--continue"],
    "",
  );
  assert_eq!(output.status.code(), Some(2));
}

#[test]
fn piped_interactive_mode_is_rejected_without_touching_terminal_state() {
  let output = Command::new(binary())
    .stdin(Stdio::null())
    .output()
    .unwrap();
  assert_eq!(output.status.code(), Some(2));
  assert!(output.stdout.is_empty());
  assert!(
    String::from_utf8(output.stderr)
      .unwrap()
      .contains("--print")
  );
}

#[test]
fn invalid_utf8_input_is_rejected_before_loading_configuration() {
  let mut child = Command::new(binary())
    .args(["--print", "--output-format", "jsonl"])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
  child.stdin.take().unwrap().write_all(&[255]).unwrap();
  let output = child.wait_with_output().unwrap();
  assert_eq!(output.status.code(), Some(2));
  assert_eq!(events(&output).last().unwrap()["status"], "invalid_input");
}

#[test]
fn a_closed_output_pipe_is_a_clean_exit_not_a_panic() {
  let server = Server::new(Reply::Text);
  let config = configuration(&server);
  let mut child = Command::new(binary())
    .arg("--config")
    .arg(config.path())
    .args(["--print", "hello", "--output-format", "jsonl"])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .unwrap();
  drop(child.stdout.take());
  let output = child.wait_with_output().unwrap();
  assert_eq!(
    output.status.code(),
    Some(0),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
}

#[test]
#[ignore]
fn export_headless_example_trace() {
  let server = Server::new(Reply::Text);
  let config = configuration(&server);
  let output = invoke(
    &config,
    &[
      "--print",
      "Summarize the supplied text",
      "--output-format",
      "jsonl",
    ],
    "Local test context",
  );
  assert!(output.status.success());
  assert_eq!(events(&output).last().unwrap()["status"], "success");
  std::fs::create_dir_all("artifacts").unwrap();
  std::fs::write("artifacts/zarncode-harness.jsonl", output.stdout).unwrap();
}

fn write_agent(config: &tempfile::TempDir, mode: &str) {
  std::fs::create_dir_all(config.path().join("agents")).unwrap();
  let spec = json!({"schema_version":1,"name":"reviewer","description":"Review safely","prompt":"SPECIAL REVIEWER INSTRUCTIONS","mode":mode,"model":"alternate","tools":["ReadFile"],"max_steps":3,"supports_background":false});
  std::fs::write(
    config.path().join("agents/reviewer.yaml"),
    serde_yaml::to_string(&spec).unwrap(),
  )
  .unwrap();
}

#[test]
fn agent_management_is_offline_and_never_overwrites_existing_definitions() {
  let directory = tempfile::tempdir().unwrap();
  let output = Command::new(binary())
    .arg("--config")
    .arg(directory.path())
    .args([
      "agent",
      "create",
      "reviewer",
      "--prompt",
      "Review safely",
      "--tool",
      "ReadFile",
      "--max-steps",
      "3",
    ])
    .output()
    .unwrap();
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  let path = directory.path().join("agents/reviewer.yaml");
  let before = std::fs::read(&path).unwrap();
  let value: Value = serde_yaml::from_slice(&before).unwrap();
  assert_eq!(value["max_steps"], 3);
  assert_eq!(value["mode"], "both");
  for action in ["list", "validate"] {
    assert!(
      Command::new(binary())
        .arg("--config")
        .arg(directory.path())
        .args(["agent", action])
        .output()
        .unwrap()
        .status
        .success()
    );
  }
  assert!(
    !Command::new(binary())
      .arg("--config")
      .arg(directory.path())
      .args(["agent", "create", "reviewer"])
      .output()
      .unwrap()
      .status
      .success()
  );
  assert_eq!(std::fs::read(path).unwrap(), before);
  assert!(!directory.path().join("config.toml").exists());
}

#[test]
fn agent_profile_applies_prompt_model_and_tool_allowlist() {
  let server = Server::new(Reply::Text);
  let config = configuration(&server);
  write_agent(&config, "both");
  let output = invoke(
    &config,
    &[
      "--print",
      "review",
      "--agent",
      "reviewer",
      "--output-format",
      "jsonl",
    ],
    "",
  );
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  let requests = server.requests.lock().unwrap();
  assert_eq!(requests[0]["model"], "alternate-model");
  assert!(
    requests[0]["messages"][0]["content"]
      .as_str()
      .unwrap()
      .contains("SPECIAL REVIEWER INSTRUCTIONS")
  );
  let names: Vec<_> = requests[0]["tools"]
    .as_array()
    .unwrap()
    .iter()
    .map(|t| t["function"]["name"].as_str().unwrap())
    .collect();
  assert_eq!(names, ["ReadFile"]);
}

#[test]
fn agent_tool_allowlist_is_enforced_even_when_yolo_is_explicit() {
  let directory = tempfile::tempdir().unwrap();
  let marker = directory.path().join("forbidden.txt");
  let server = Server::new(Reply::WriteThenText(marker.to_string_lossy().into()));
  let config = configuration(&server);
  write_agent(&config, "both");
  let output = invoke(
    &config,
    &[
      "--print",
      "try",
      "--agent",
      "reviewer",
      "--yolo",
      "--output-format",
      "jsonl",
    ],
    "",
  );
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert!(!marker.exists());
  assert!(events(&output).iter().any(|e| {
    e["type"] == "tool_end"
      && e["output"]
        .as_str()
        .is_some_and(|s| s.contains("not available"))
  }));
}

#[test]
fn configured_step_budget_stops_before_an_extra_provider_request() {
  let directory = tempfile::tempdir().unwrap();
  let marker = directory.path().join("authorized.txt");
  let server = Server::new(Reply::WriteThenText(marker.to_string_lossy().into()));
  let config = configuration(&server);
  let output = invoke_in(
    &config,
    &[
      "--print",
      "write",
      "--yolo",
      "--max-steps",
      "1",
      "--output-format",
      "jsonl",
    ],
    "",
    directory.path(),
  );
  assert_eq!(
    output.status.code(),
    Some(1),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert_eq!(server.requests.lock().unwrap().len(), 1);
  assert!(marker.exists());
  let record = events(&output).pop().unwrap();
  assert!(
    record["message"]
      .as_str()
      .unwrap()
      .contains("maximum steps")
  );
  for invalid in ["0", "10001"] {
    assert_eq!(
      invoke(&config, &["--print", "no", "--max-steps", invalid], "")
        .status
        .code(),
      Some(2)
    );
  }
}

#[test]
fn custom_subagent_is_advertised_and_runs_through_existing_delegation() {
  let server = Server::new(Reply::DelegateThenText);
  let config = configuration(&server);
  write_agent(&config, "both");
  let output = invoke(
    &config,
    &["--print", "delegate", "--yolo", "--output-format", "jsonl"],
    "",
  );
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  let requests = server.requests.lock().unwrap();
  assert_eq!(requests.len(), 3);
  assert!(
    requests[1]["messages"][0]["content"]
      .as_str()
      .unwrap()
      .contains("SPECIAL REVIEWER INSTRUCTIONS")
  );
  assert_eq!(requests[1]["model"], "alternate-model");
  let tool = requests[0]["tools"]
    .as_array()
    .unwrap()
    .iter()
    .find(|t| t["function"]["name"] == "Agent")
    .unwrap();
  assert!(
    tool["function"]["parameters"]["properties"]["subagent_type"]["enum"]
      .as_array()
      .unwrap()
      .contains(&json!("reviewer"))
  );
}

#[test]
fn a_child_cannot_reenter_the_root_bound_agent_handler() {
  let server = Server::new(Reply::DelegateThenNested);
  let config = configuration(&server);
  write_agent(&config, "both");
  let output = invoke(
    &config,
    &[
      "--print",
      "delegate safely",
      "--yolo",
      "--output-format",
      "jsonl",
    ],
    "",
  );
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  let requests = server.requests.lock().unwrap();
  assert_eq!(
    requests.len(),
    4,
    "nested delegation escaped its leaf tool policy"
  );
  assert!(
    requests[1]["tools"]
      .as_array()
      .unwrap()
      .iter()
      .all(|t| t["function"]["name"] != "Agent")
  );
}

#[test]
fn installed_binary_bundles_core_tools_and_system_prompt() {
  let server = Server::new(Reply::Text);
  let config = configuration(&server);
  std::fs::remove_dir_all(config.path().join("data/prompts")).unwrap();
  let output = invoke(
    &config,
    &["--print", "hello", "--output-format", "jsonl"],
    "",
  );
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  let requests = server.requests.lock().unwrap();
  assert!(
    !requests[0]["messages"][0]["content"]
      .as_str()
      .unwrap()
      .trim()
      .is_empty()
  );
  let names: Vec<_> = requests[0]["tools"]
    .as_array()
    .unwrap()
    .iter()
    .map(|t| t["function"]["name"].as_str().unwrap())
    .collect();
  assert!(names.contains(&"ReadFile") && names.contains(&"Agent"));
  assert!(!names.contains(&if cfg!(windows) { "Bash" } else { "PowerShell" }));
}

#[test]
fn explicit_model_choice_wins_over_an_agent_default_and_agent_budget_is_capped() {
  let server = Server::new(Reply::Text);
  let config = configuration(&server);
  write_agent(&config, "both");
  let output = invoke(
    &config,
    &[
      "--print",
      "review",
      "--agent",
      "reviewer",
      "--model",
      "base",
      "--max-steps",
      "2",
      "--output-format",
      "jsonl",
    ],
    "",
  );
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  let values = events(&output);
  assert_eq!(values[0]["max_steps"], 2);
  assert_eq!(values[0]["agent"], "reviewer");
  assert_eq!(server.requests.lock().unwrap()[0]["model"], "base-model");
}

#[test]
fn resuming_a_profiled_session_restores_its_model_prompt_and_permissions() {
  let server = Server::new(Reply::Text);
  let config = configuration(&server);
  write_agent(&config, "both");
  let first = invoke(
    &config,
    &[
      "--print",
      "review first",
      "--agent",
      "reviewer",
      "--output-format",
      "jsonl",
    ],
    "",
  );
  assert!(
    first.status.success(),
    "{}",
    String::from_utf8_lossy(&first.stderr)
  );
  let second = invoke(
    &config,
    &[
      "--print",
      "continue review",
      "--continue",
      "--output-format",
      "jsonl",
    ],
    "",
  );
  assert!(
    second.status.success(),
    "{}",
    String::from_utf8_lossy(&second.stderr)
  );
  assert_eq!(events(&second)[0]["agent"], "reviewer");
  assert_eq!(
    events(&second)[0]["session_id"],
    events(&first)[0]["session_id"]
  );
  let requests = server.requests.lock().unwrap();
  assert_eq!(requests[1]["model"], "alternate-model");
  assert!(
    requests[1]["messages"][0]["content"]
      .as_str()
      .unwrap()
      .contains("SPECIAL REVIEWER INSTRUCTIONS")
  );
  assert!(
    requests[1]["tools"]
      .as_array()
      .unwrap()
      .iter()
      .all(|t| t["function"]["name"] == "ReadFile")
  );
}

#[test]
fn parallel_delegation_cannot_exceed_the_admission_limit() {
  let server = Server::new(Reply::ParallelDelegate);
  let config = configuration(&server);
  write_agent(&config, "both");
  let output = invoke(
    &config,
    &[
      "--print",
      "delegate",
      "--yolo",
      "--max-concurrent-agents",
      "1",
      "--output-format",
      "jsonl",
    ],
    "",
  );
  assert!(
    output.status.success(),
    "{}",
    String::from_utf8_lossy(&output.stderr)
  );
  assert_eq!(server.requests.lock().unwrap().len(), 3);
  assert_eq!(
    events(&output)
      .iter()
      .filter(|e| e["type"] == "tool_end"
        && e["output"]
          .as_str()
          .is_some_and(|s| s.contains("Concurrent agent limit")))
      .count(),
    2
  );
  for invalid in ["0", "33"] {
    assert_eq!(
      invoke(
        &config,
        &["--print", "no", "--max-concurrent-agents", invalid],
        ""
      )
      .status
      .code(),
      Some(2)
    );
  }
}
