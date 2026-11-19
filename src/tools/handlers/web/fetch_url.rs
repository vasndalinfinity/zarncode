//! FetchURL tool handler.
//!
//! Fetches a web page from a URL and extracts main text content from it.

use std::io::Cursor;

use async_trait::async_trait;
use readability::extractor::extract;
use reqwest::Client;
use reqwest::header::CONTENT_TYPE;
use serde::Deserialize;
use url::Url;

use crate::tools::{
  ToolError, ToolHandler, ToolInvocation, ToolKind, ToolOutput, ToolPayload, parse_arguments,
};

/// Handler for the FetchURL tool
pub struct FetchURLHandler;

/// Hosts and ranges a fetch must never reach: loopback, link-local, private
/// networks, and cloud metadata. The model supplies the URL, so this is the only
/// thing standing between the tool and an SSRF probe.
fn blocked_fetch_target(url: &Url) -> Option<String> {
  let scheme = url.scheme();
  if scheme != "http" && scheme != "https" {
    return Some(format!("scheme `{scheme}` is not allowed; use http or https"));
  }
  let host = url.host_str()?.to_ascii_lowercase();
  let host = host.trim_matches(|c| c == '[' || c == ']');
  if host == "localhost"
    || host.ends_with(".localhost")
    || host.ends_with(".local")
    || host.ends_with(".internal")
    || host == "metadata.google.internal"
  {
    return Some(format!("fetch refused: `{host}` is a local host"));
  }
  if let Ok(ip) = host.parse::<std::net::IpAddr>() {
    let blocked = match ip {
      std::net::IpAddr::V4(v4) => {
        v4.is_loopback()
          || v4.is_private()
          || v4.is_link_local()
          || v4.is_broadcast()
          || v4.is_unspecified()
          || v4.is_documentation()
          || v4.octets()[0] == 0
      }
      std::net::IpAddr::V6(v6) => {
        v6.is_loopback()
          || v6.is_unspecified()
          || v6.is_unique_local()
          || v6.is_unicast_link_local()
          // IPv4-mapped addresses reach the v4 checks above only when written
          // as v4; block the mapped form too.
          || v6.to_ipv4_mapped().map(|v4| {
            v4.is_loopback() || v4.is_private() || v4.is_link_local()
          }).unwrap_or(false)
      }
    };
    if blocked {
      return Some(format!("fetch refused: `{host}` is a private address"));
    }
  }
  None
}

/// Resolve the hostname and refuse when any answer is a private address. A
/// literal was the only thing checked before, so `localtest.me` and any
/// attacker-controlled A record walked straight into 127.0.0.1.
async fn resolved_target_blocked(url: &Url) -> Option<String> {
  let host = url.host_str()?.to_ascii_lowercase();
  let host = host.trim_matches(|c| c == '[' || c == ']').to_string();
  if host.parse::<std::net::IpAddr>().is_ok() {
    return None;
  }
  let port = url.port_or_known_default().unwrap_or(443);
  let addrs = match tokio::net::lookup_host((host.as_str(), port)).await {
    Ok(a) => a,
    Err(_) => return Some(format!("fetch refused: cannot resolve `{host}`")),
  };
  for addr in addrs {
    let ip = addr.ip();
    let blocked = match ip {
      std::net::IpAddr::V4(v4) => {
        v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
      }
      std::net::IpAddr::V6(v6) => {
        v6.is_loopback() || v6.is_unspecified() || v6.is_unique_local() || v6.is_unicast_link_local()
      }
    };
    if blocked {
      return Some(format!(
        "fetch refused: `{host}` resolves to the private address {ip}"
      ));
    }
  }
  None
}

/// Arguments for the FetchURL tool
#[derive(Debug, Deserialize)]
struct FetchURLArgs {
  /// The URL to fetch content from
  url: String,
}

#[async_trait]
impl ToolHandler for FetchURLHandler {
  fn kind(&self) -> ToolKind {
    ToolKind::Function
  }

  async fn is_mutating(&self, _invocation: &ToolInvocation) -> bool {
    false
  }

  async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, ToolError> {
    let ToolInvocation { payload, .. } = invocation;

    // Extract arguments from payload
    let arguments = match payload {
      ToolPayload::Function { arguments } => arguments,
    };

    // Parse arguments
    let args: FetchURLArgs = parse_arguments(&arguments)?;

    // Validate URL
    let url = match Url::parse(&args.url) {
      Ok(u) => u,
      Err(e) => {
        return Err(ToolError::RespondToModel(format!(
          "Invalid URL '{}': {}",
          args.url, e
        )));
      }
    };
    // Only the public web is fetchable. Loopback, link-local, and private
    // ranges were reachable before, which made the tool an SSRF probe of the
    // local network and of cloud metadata endpoints.
    if let Some(reason) = blocked_fetch_target(&url) {
      return Err(ToolError::RespondToModel(reason));
    }
    if let Some(reason) = resolved_target_blocked(&url).await {
      return Err(ToolError::RespondToModel(reason));
    }

    // Fetch the URL
    let client = match Client::builder()
      .user_agent(
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
         (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36",
      )
      // A public URL must not be able to bounce the request onto localhost, so
      // every hop is re-checked and the chain is short.
      .redirect(reqwest::redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 5 {
          attempt.stop()
        } else if blocked_fetch_target(attempt.url()).is_some() {
          attempt.stop()
        } else {
          attempt.follow()
        }
      }))
      .timeout(std::time::Duration::from_secs(45))
      .build()
    {
      Ok(c) => c,
      Err(e) => {
        return Err(ToolError::Fatal(format!(
          "Failed to build HTTP client: {}",
          e
        )));
      }
    };

    let response = match client.get(url.clone()).send().await {
      Ok(r) => r,
      Err(e) => {
        return Err(ToolError::RespondToModel(format!(
          "Failed to fetch URL due to network error: {}. \
           This may indicate the URL is invalid or the server is unreachable.",
          e
        )));
      }
    };

    let status = response.status();
    if status.as_u16() >= 400 {
      return Err(ToolError::RespondToModel(format!(
        "Failed to fetch URL. Status: {}. \
         This may indicate the page is not accessible or the server is down.",
        status
      )));
    }

    // Check content type
    let content_type = response
      .headers()
      .get(CONTENT_TYPE)
      .and_then(|v| v.to_str().ok())
      .unwrap_or("")
      .to_lowercase();

    // Cap the body: an unbounded read let one page exhaust the context window.
    const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
    if let Some(len) = response.content_length() {
      if len as usize > MAX_BODY_BYTES {
        return Err(ToolError::RespondToModel(format!(
          "Page is {len} bytes; the fetch cap is {MAX_BODY_BYTES} bytes."
        )));
      }
    }
    let mut body_bytes = match response.bytes().await {
      Ok(b) => b.to_vec(),
      Err(e) => {
        return Err(ToolError::RespondToModel(format!(
          "Failed to read response body: {}",
          e
        )));
      }
    };
    if body_bytes.len() > MAX_BODY_BYTES {
      body_bytes.truncate(MAX_BODY_BYTES);
    }

    if body_bytes.is_empty() {
      return Ok(ToolOutput::success("The response body is empty."));
    }

    // For plain text or markdown, return directly
    if content_type.starts_with("text/plain") || content_type.starts_with("text/markdown") {
      let text = String::from_utf8_lossy(&body_bytes);
      return Ok(ToolOutput::success(text.to_string()));
    }

    // For HTML, extract main content using readability
    let mut cursor = Cursor::new(&body_bytes);
    match extract(&mut cursor, &url) {
      Ok(product) => {
        if product.text.trim().is_empty() {
          Err(ToolError::RespondToModel(
            "Failed to extract meaningful content from the page. \
             This may indicate the page content is not suitable for text extraction, \
             or the page requires JavaScript to render its content."
              .to_string(),
          ))
        } else {
          Ok(ToolOutput::success(product.text))
        }
      }
      Err(e) => Err(ToolError::RespondToModel(format!(
        "Failed to extract content from the page: {}",
        e
      ))),
    }
  }
}

impl FetchURLHandler {
  /// Create a new FetchURLHandler
  pub fn new() -> Self {
    Self
  }
}

impl Default for FetchURLHandler {
  fn default() -> Self {
    Self::new()
  }
}

#[cfg(test)]
mod tests {
  use std::env;

  use super::*;

  #[test]
  fn test_parse_arguments() {
    let json = r#"{"url": "https://example.com"}"#;
    let args: FetchURLArgs = parse_arguments(json).unwrap();
    assert_eq!(args.url, "https://example.com");
  }

  #[tokio::test]
  async fn test_fetch_url_invalid_url() {
    let temp_dir = env::temp_dir();
    let handler = FetchURLHandler::new();

    let invocation = ToolInvocation::new(
      "test-call-id",
      "call-test",
      ToolPayload::Function {
        arguments: r#"{"url": "not-a-valid-url"}"#.to_string(),
      },
      &temp_dir,
    );

    let result = handler.handle(invocation).await;
    assert!(result.is_err());
    let err_msg = format!("{}", result.unwrap_err());
    assert!(err_msg.contains("Invalid URL"));
  }
}
