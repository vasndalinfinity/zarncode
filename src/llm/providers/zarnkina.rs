//! Zarnkina relay provider.
//!
//! OpenAI-compatible SSE against the relay gateway. Identity is a `zk_live_`
//! API key or a signed `zk_sess_` session token; the relay decides the upstream,
//! so nothing provider-specific lives here.

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream::{self, unfold};
use reqwest::Client;
use reqwest_eventsource::{Error as EventSourceError, Event, RequestBuilderExt};
use serde::{Deserialize, Serialize};

use crate::account::Credentials;
use crate::error::{Error, LlmError, Result, StreamErrorCategory};
use crate::llm::provider::{LLMProvider, LlmResponseStream, LlmStreamEvent};
use crate::llm::types::{ChatConfig, Message, Role};
use crate::tools::Tool;

/// Provider for `type = "zarnkina"` and generic OpenAI-compatible relays.
pub struct ZarnkinaProvider {
  http_client: Client,
  base_url: String,
  /// Base for OpenAI-compatible calls; always ends in /v1.
  chat_url: String,
  config: ChatConfig,
  credentials: Credentials,
  max_context_size: usize,
  tool_registry: std::sync::Arc<crate::tools::ToolRegistry>,
  /// Current user turn id; the relay bills one prompt per turn.
  turn_id: std::sync::Arc<std::sync::Mutex<String>>,
}

impl std::fmt::Debug for ZarnkinaProvider {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("ZarnkinaProvider")
      .field("base_url", &self.base_url)
      .field("config", &self.config)
      .field("max_context_size", &self.max_context_size)
      .finish()
  }
}

impl ZarnkinaProvider {
  pub fn new(
    base_url: impl Into<String>,
    config: ChatConfig,
    credentials: Credentials,
    max_context_size: usize,
    tool_registry: std::sync::Arc<crate::tools::ToolRegistry>,
    turn_id: std::sync::Arc<std::sync::Mutex<String>>,
  ) -> Result<Self> {
    let base_url = base_url.into();
    crate::account::validate_relay_url(&base_url)
      .map_err(|e| LlmError::InvalidConfig(format!("Zarnkina relay URL rejected: {e}")))?;
    if credentials.api_key.is_none() && credentials.session_token.is_none() {
      return Err(
        LlmError::InvalidConfig(
          "Zarnkina relay needs an API key or a session token; run `zarncode auth login`"
            .to_string(),
        )
        .into(),
      );
    }
    let http_client = Self::create_http_client(&credentials)?;
    let root = base_url.trim_end_matches('/');
    // The relay serves the OpenAI surface under /v1 and account endpoints
    // at the root, so the chat base is derived instead of assumed.
    let chat_url = if root.ends_with("/v1") {
      root.to_string()
    } else {
      format!("{root}/v1")
    };
    Ok(Self {
      http_client,
      base_url: root.to_string(),
      chat_url,
      config,
      credentials,
      max_context_size,
      tool_registry,
      turn_id,
    })
  }

  fn create_http_client(credentials: &Credentials) -> Result<Client> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
      "Content-Type",
      "application/json"
        .parse()
        .map_err(|_| LlmError::InvalidConfig("Invalid Content-Type header".to_string()))?,
    );
    if let Some(key) = credentials.api_key.as_ref() {
      headers.insert(
        "Authorization",
        format!("Bearer {key}")
          .parse()
          .map_err(|_| LlmError::InvalidConfig("Invalid Authorization header".to_string()))?,
      );
    } else if let Some(token) = credentials.session_token.as_ref() {
      headers.insert(
        "x-zarnkina-token",
        token
          .parse()
          .map_err(|_| LlmError::InvalidConfig("Invalid session token header".to_string()))?,
      );
    }
    if let Some(email) = credentials.email.as_ref()
      && let Ok(value) = email.parse()
    {
      headers.insert("x-zarnkina-email", value);
    }
    Client::builder()
      .default_headers(headers)
      .connect_timeout(std::time::Duration::from_secs(15))
      .build()
      .map_err(|e| LlmError::InvalidConfig(format!("Failed to build HTTP client: {e}")).into())
  }
}

#[derive(Debug, Clone, Serialize)]
struct RequestMessage {
  role: String,
  #[serde(skip_serializing_if = "Option::is_none")]
  content: Option<String>,
  #[serde(skip_serializing_if = "Option::is_none")]
  tool_calls: Option<Vec<RequestToolCall>>,
  #[serde(skip_serializing_if = "Option::is_none")]
  tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct RequestToolCall {
  id: String,
  #[serde(rename = "type")]
  call_type: String,
  function: RequestToolFunction,
}

#[derive(Debug, Clone, Serialize)]
struct RequestToolFunction {
  name: String,
  arguments: String,
}

#[derive(Debug, Clone, Serialize)]
struct ToolDefinition {
  #[serde(rename = "type")]
  tool_type: String,
  function: ToolFunction,
}

#[derive(Debug, Clone, Serialize)]
struct ToolFunction {
  name: String,
  description: String,
  parameters: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
struct StreamOptions {
  include_usage: bool,
}

#[derive(Debug, Clone, Serialize)]
struct ChatRequest {
  model: String,
  messages: Vec<RequestMessage>,
  stream: bool,
  #[serde(skip_serializing_if = "Option::is_none")]
  max_tokens: Option<u32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  temperature: Option<f32>,
  #[serde(skip_serializing_if = "Option::is_none")]
  tools: Option<Vec<ToolDefinition>>,
  #[serde(skip_serializing_if = "Option::is_none")]
  stream_options: Option<StreamOptions>,
  /// Reasoning effort. Every upstream we serve understands "none", and a
  /// model that skips its thinking answers in seconds instead of a minute.
  #[serde(skip_serializing_if = "Option::is_none")]
  reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct Delta {
  #[serde(default)]
  content: Option<String>,
  #[serde(default)]
  reasoning_content: Option<String>,
  #[serde(default)]
  reasoning: Option<String>,
  #[serde(default)]
  tool_calls: Option<Vec<DeltaToolCall>>,
}

#[derive(Debug, Clone, Deserialize)]
struct DeltaToolCall {
  #[serde(default)]
  index: Option<u32>,
  #[serde(default)]
  id: Option<String>,
  #[serde(default)]
  function: Option<DeltaToolFunction>,
}

#[derive(Debug, Clone, Deserialize)]
struct DeltaToolFunction {
  #[serde(default)]
  name: Option<String>,
  #[serde(default)]
  arguments: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct Choice {
  #[serde(default)]
  delta: Delta,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct Usage {
  #[serde(default)]
  prompt_tokens: u32,
  #[serde(default)]
  completion_tokens: u32,
  #[serde(default)]
  total_tokens: u32,
  #[serde(default)]
  cached_tokens: Option<u32>,
  #[serde(default)]
  prompt_tokens_details: Option<PromptTokensDetails>,
}

#[derive(Debug, Clone, Deserialize)]
struct PromptTokensDetails {
  #[serde(default)]
  cached_tokens: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
struct StreamChunk {
  #[serde(default)]
  choices: Vec<Choice>,
  #[serde(default)]
  usage: Option<Usage>,
}

fn convert_message(message: &Message) -> RequestMessage {
  let role = match message.role {
    Role::System => "system",
    Role::User => "user",
    Role::Assistant => "assistant",
    Role::Tool => "tool",
  };
  let tool_calls = message.tool_calls.as_ref().map(|calls| {
    calls
      .iter()
      .map(|call| RequestToolCall {
        id: call.id.clone(),
        call_type: "function".to_string(),
        function: RequestToolFunction {
          name: call.name.clone(),
          arguments: call.arguments.clone(),
        },
      })
      .collect()
  });
  RequestMessage {
    role: role.to_string(),
    content: if message.content.is_empty() && tool_calls.is_some() {
      None
    } else {
      Some(message.content.clone())
    },
    tool_calls,
    tool_call_id: message.tool_call_id.clone(),
  }
}

fn convert_tools(tools: &[&Tool]) -> Vec<ToolDefinition> {
  tools
    .iter()
    .map(|tool| ToolDefinition {
      tool_type: "function".to_string(),
      function: ToolFunction {
        name: tool.name.clone(),
        description: tool.description.clone(),
        parameters: tool.parameters.clone(),
      },
    })
    .collect()
}

/// Relay errors arrive as `{"error":{"message":...,"type":...}}`.
fn classify_stream_error(error: EventSourceError) -> LlmError {
  match error {
    EventSourceError::InvalidStatusCode(code, _) => LlmError::Stream {
      category: StreamErrorCategory::Http,
      status_code: Some(code.as_u16()),
      message: format!("relay returned HTTP {code}"),
    },
    EventSourceError::Transport(source) => {
      if source.is_timeout() {
        LlmError::Stream {
          category: StreamErrorCategory::Timeout,
          status_code: None,
          message: format!("stream timeout: {source}"),
        }
      } else if source.is_connect() {
        LlmError::Stream {
          category: StreamErrorCategory::Disconnected,
          status_code: None,
          message: format!("connection lost: {source}"),
        }
      } else {
        LlmError::Stream {
          category: StreamErrorCategory::Transport,
          status_code: None,
          message: format!("transport error: {source}"),
        }
      }
    }
    other => LlmError::Stream {
      category: StreamErrorCategory::Parse,
      status_code: None,
      message: format!("stream parse error: {other}"),
    },
  }
}

/// Splits inline ` thinking` style tags into thinking events so a model that
/// emits them as text still renders correctly.
#[derive(Debug, Default)]
struct ThinkSplitter {
  in_think: bool,
  buffer: String,
}
const OPEN_TAGS: [&str; 3] = ["<thinking", "<thought", "<think"];
const CLOSE_TAGS: [&str; 3] = ["</thinking", "</thought", "</think"];

impl ThinkSplitter {
  fn push(&mut self, chunk: &str) -> Vec<LlmStreamEvent> {
    self.buffer.push_str(chunk);
    let mut events = Vec::new();
    loop {
      let tags: &[&str] = if self.in_think {
        &CLOSE_TAGS
      } else {
        &OPEN_TAGS
      };
      match earliest_tag(&self.buffer, tags) {
        Some((index, tag_len)) => {
          let piece = self.buffer[..index].to_string();
          self.emit(&mut events, piece);
          self.buffer.drain(..index + tag_len);
          // The tag's closing bracket is not content.
          if self.buffer.starts_with('>') {
            self.buffer.drain(..1);
          }
          self.in_think = !self.in_think;
        }
        None => {
          let hold = holdback_len(&self.buffer, tags);
          let keep = self.buffer.len().saturating_sub(hold);
          if keep > 0 {
            let piece: String = self.buffer[..keep].to_string();
            self.buffer.drain(..keep);
            self.emit(&mut events, piece);
          }
          break;
        }
      }
    }
    events
  }

  fn finish(&mut self) -> Vec<LlmStreamEvent> {
    let mut events = Vec::new();
    let tail = std::mem::take(&mut self.buffer);
    self.emit(&mut events, tail);
    events
  }

  fn emit(&self, events: &mut Vec<LlmStreamEvent>, piece: String) {
    if piece.is_empty() {
      return;
    }
    if self.in_think {
      events.push(LlmStreamEvent::Thinking(piece));
    } else {
      events.push(LlmStreamEvent::Content(piece));
    }
  }
}

/// Earliest tag match; the longest tag wins when two start at the same byte.
fn earliest_tag(buffer: &str, tags: &[&str]) -> Option<(usize, usize)> {
  let mut best: Option<(usize, usize)> = None;
  for tag in tags {
    if let Some(index) = buffer.find(tag) {
      best = match best {
        Some((best_index, best_len))
          if best_index < index || (best_index == index && best_len >= tag.len()) =>
        {
          Some((best_index, best_len))
        }
        _ => Some((index, tag.len())),
      };
    }
  }
  best
}
/// Bytes at the end of the buffer that could still become a tag.
fn holdback_len(buffer: &str, tags: &[&str]) -> usize {
  let max = tags
    .iter()
    .map(|tag| tag.len())
    .max()
    .unwrap_or(0)
    .saturating_sub(1);
  for len in 1..=max.min(buffer.len()) {
    let start = buffer.len() - len;
    if !buffer.is_char_boundary(start) {
      continue;
    }
    let suffix = &buffer[start..];
    if tags.iter().any(|tag| tag.starts_with(suffix)) {
      return len;
    }
  }
  0
}

#[async_trait]
impl LLMProvider for ZarnkinaProvider {
  async fn chat_stream(&self, messages: Vec<Message>) -> Result<LlmResponseStream> {
    let mut request = ChatRequest {
      model: self.config.model.clone(),
      messages: messages.iter().map(convert_message).collect(),
      stream: true,
      max_tokens: self.config.max_tokens,
      temperature: self.config.temperature,
      tools: None,
      stream_options: Some(StreamOptions {
        include_usage: true,
      }),
      reasoning_effort: if self.config.enable_thinking {
        None
      } else {
        Some("none".to_string())
      },
    };
    if !self.tool_registry.is_empty() {
      let tools = self.tool_registry.all();
      request.tools = Some(convert_tools(&tools));
    }

    let url = format!("{}/chat/completions", self.chat_url);
    // One user turn is one prompt: every call inside the turn carries the same
    // id, so the relay charges once and the tool loop rides along for free.
    let turn = self
      .turn_id
      .lock()
      .map(|value| value.clone())
      .unwrap_or_default();
    let mut builder = self.http_client.post(&url);
    builder = builder.header(
      "x-zarnkina-client",
      format!("cli/{}", env!("CARGO_PKG_VERSION")),
    );
    if !turn.is_empty() {
      builder = builder.header("x-zarnkina-turn", turn);
    }
    let event_source = builder
      .json(&request)
      .eventsource()
      .map_err(|_| LlmError::InvalidConfig("Failed to create event source".to_string()))?;

    // The splitter lives inside the unfold state so tag detection survives
    // chunk boundaries across polls.
    let stream = unfold(
      (event_source, ThinkSplitter::default()),
      |(mut source, mut splitter)| async move {
        loop {
          match source.next().await {
            Some(Ok(Event::Open)) => continue,
            Some(Ok(Event::Message(message))) => {
              let data = message.data.trim().to_string();
              if data == "[DONE]" {
                let tail: Vec<Result<LlmStreamEvent>> =
                  splitter.finish().into_iter().map(Ok).collect();
                if tail.is_empty() {
                  return None;
                }
                return Some((stream::iter(tail), (source, splitter)));
              }
              if data.is_empty() {
                continue;
              }
              let chunk: StreamChunk = match serde_json::from_str(&data) {
                Ok(parsed) => parsed,
                Err(error) => {
                  let failure = vec![Err(Error::Llm(LlmError::Stream {
                    category: StreamErrorCategory::Parse,
                    status_code: None,
                    message: format!("failed to parse relay chunk: {error}"),
                  }))];
                  return Some((stream::iter(failure), (source, splitter)));
                }
              };
              let mut events: Vec<Result<LlmStreamEvent>> = Vec::new();
              for choice in &chunk.choices {
                let reasoning = choice
                  .delta
                  .reasoning_content
                  .as_ref()
                  .or(choice.delta.reasoning.as_ref());
                if let Some(text) = reasoning
                  && !text.is_empty()
                {
                  events.push(Ok(LlmStreamEvent::Thinking(text.clone())));
                }
                if let Some(text) = choice.delta.content.as_ref()
                  && !text.is_empty()
                {
                  events.extend(splitter.push(text).into_iter().map(Ok));
                }
                if let Some(calls) = choice.delta.tool_calls.as_ref() {
                  for call in calls {
                    events.push(Ok(LlmStreamEvent::ToolCallChunk {
                      index: call.index.unwrap_or(0),
                      id: call.id.clone(),
                      name: call.function.as_ref().and_then(|f| f.name.clone()),
                      arguments: call.function.as_ref().and_then(|f| f.arguments.clone()),
                    }));
                  }
                }
              }
              if let Some(usage) = chunk.usage {
                events.push(Ok(LlmStreamEvent::Usage {
                  total_tokens: usage.total_tokens,
                  prompt_tokens: usage.prompt_tokens,
                  completion_tokens: usage.completion_tokens,
                  cached_tokens: usage.cached_tokens.or_else(|| {
                    usage
                      .prompt_tokens_details
                      .and_then(|details| details.cached_tokens)
                  }),
                }));
              }
              if events.is_empty() {
                continue;
              }
              return Some((stream::iter(events), (source, splitter)));
            }
            Some(Err(error)) => {
              let failure = vec![Err(Error::Llm(classify_stream_error(error)))];
              return Some((stream::iter(failure), (source, splitter)));
            }
            None => {
              let tail: Vec<Result<LlmStreamEvent>> =
                splitter.finish().into_iter().map(Ok).collect();
              if tail.is_empty() {
                return None;
              }
              return Some((stream::iter(tail), (source, splitter)));
            }
          }
        }
      },
    )
    .flatten();

    Ok(Box::pin(stream))
  }

  fn name(&self) -> &str {
    "zarnkina"
  }

  fn max_context_size(&self) -> usize {
    self.max_context_size
  }

  async fn on_retryable_error(&mut self, error: &LlmError) {
    log::info!("ZarnkinaProvider: rebuilding HTTP client after {error}");
    if let Ok(client) = Self::create_http_client(&self.credentials) {
      self.http_client = client;
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn splitter_separates_thinking_from_content() {
    let mut splitter = ThinkSplitter::default();
    let mut events = splitter.push("hello <thi");
    events.extend(splitter.push("nk>inner</thinking> world"));
    events.extend(splitter.finish());
    let rendered: Vec<String> = events
      .into_iter()
      .map(|event| match event {
        LlmStreamEvent::Content(text) => format!("C:{text}"),
        LlmStreamEvent::Thinking(text) => format!("T:{text}"),
        other => format!("{other:?}"),
      })
      .collect();
    let joined = rendered.join("|");
    assert!(joined.contains("C:hello "), "{joined}");
    assert!(joined.contains("T:inner"), "{joined}");
    assert!(joined.contains("C: world"), "{joined}");
  }

  #[test]
  fn splitter_holds_partial_tag_prefixes() {
    let mut splitter = ThinkSplitter::default();
    let first = splitter.push("abc <think");
    assert!(first.is_empty() || matches!(first[0], LlmStreamEvent::Content(_)));
    let second = splitter.push(">hidden");
    assert!(
      second
        .iter()
        .any(|event| matches!(event, LlmStreamEvent::Thinking(_)))
    );
  }
}
