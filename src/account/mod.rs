//! Zarnkina account identity, relay verification, and model catalogue.
//!
//! Credential precedence: environment, then CLI config, then the CLI auth file,
//! then the desktop auth file written by the Zarnkina desktop client. Chat
//! requests authenticate with a `zk_live_` API key when one is present,
//! otherwise with the signed session token issued by the relay at login.

pub mod quota;

use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::config::Config;

/// Public relay. Overridable per provider via `base_url`.
pub const DEFAULT_RELAY_BASE: &str = "https://zarnkina-relay.hostless.app";

/// Google OAuth client used by the desktop client as well; the secret stays on
/// the relay, so only the client id ships here.
const GOOGLE_CLIENT_ID: &str =
  "818723996932-mpabto4q6tbndd7q8uj65mov8aiumjrn.apps.googleusercontent.com";
const OAUTH_REDIRECT_PORT: u16 = 8765;
const OAUTH_REDIRECT: &str = "http://127.0.0.1:8765";

/// How long a verification result is reused before the relay is asked again.
const VERIFY_TTL: Duration = Duration::from_secs(3);
/// Network failure may keep the last-good answer this long before denying.
const DEGRADED_GRACE: Duration = Duration::from_secs(600);

/// Identity used for relay requests.
#[derive(Debug, Clone, Default)]
pub struct Credentials {
  pub email: Option<String>,
  pub api_key: Option<String>,
  pub session_token: Option<String>,
}

impl Credentials {
  /// A label for quota bookkeeping; the email when known, otherwise a key prefix.
  pub fn quota_label(&self) -> String {
    if let Some(email) = self.email.as_ref().filter(|e| !e.is_empty()) {
      return email.clone();
    }
    match self.api_key.as_deref() {
      Some(key) if key.len() >= 12 => format!(
        "{}ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã¢â‚¬Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢ÃƒÆ’Ã†â€™ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬ÃƒÂ¢Ã¢â‚¬Å¾Ã‚Â¢ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬Ãƒâ€šÃ‚Â ÃƒÆ’Ã†â€™Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¡Ãƒâ€šÃ‚Â¬ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¾Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã¢â‚¬Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢ÃƒÆ’Ã†â€™Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¡Ãƒâ€šÃ‚Â¬ÃƒÆ’Ã¢â‚¬Â¦Ãƒâ€šÃ‚Â¡ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬Ãƒâ€¦Ã‚Â¡ÃƒÆ’Ã†â€™ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¡ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã¢â‚¬Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢ÃƒÆ’Ã†â€™ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬ÃƒÂ¢Ã¢â‚¬Å¾Ã‚Â¢ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬Ãƒâ€¦Ã‚Â¡ÃƒÆ’Ã†â€™ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¡ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã¢â‚¬Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢ÃƒÆ’Ã†â€™ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¡ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã†â€™Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¡Ãƒâ€šÃ‚Â¬ÃƒÆ’Ã¢â‚¬Â¦Ãƒâ€šÃ‚Â¡ÃƒÆ’Ã†â€™ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¡ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â¬ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬Ãƒâ€šÃ‚Â¦ÃƒÆ’Ã†â€™ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¡ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â¡ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã¢â‚¬Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢ÃƒÆ’Ã†â€™Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¡Ãƒâ€šÃ‚Â¬ÃƒÆ’Ã¢â‚¬Â¦Ãƒâ€šÃ‚Â¡ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬Ãƒâ€¦Ã‚Â¡ÃƒÆ’Ã†â€™ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¡ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â¬ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã¢â‚¬Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢ÃƒÆ’Ã†â€™ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬ÃƒÂ¢Ã¢â‚¬Å¾Ã‚Â¢ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã†â€™Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬Ãƒâ€¦Ã‚Â¡ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â¬ÃƒÆ’Ã†â€™ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â¦ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â¡ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã¢â‚¬Â ÃƒÂ¢Ã¢â€šÂ¬Ã¢â€žÂ¢ÃƒÆ’Ã†â€™Ãƒâ€šÃ‚Â¢ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¡Ãƒâ€šÃ‚Â¬ÃƒÆ’Ã¢â‚¬Â¦Ãƒâ€šÃ‚Â¡ÃƒÆ’Ã†â€™Ãƒâ€ Ã¢â‚¬â„¢ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬Ãƒâ€¦Ã‚Â¡ÃƒÆ’Ã†â€™ÃƒÂ¢Ã¢â€šÂ¬Ã…Â¡ÃƒÆ’Ã¢â‚¬Å¡Ãƒâ€šÃ‚Â¦",
        &key[..12]
      ),
      _ => "local".to_string(),
    }
  }
}

/// On-disk auth record shared with the desktop client (`Zarnkina/auth.json`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuthFile {
  #[serde(default)]
  pub connected: bool,
  #[serde(default)]
  pub email: Option<String>,
  #[serde(default)]
  pub api_key: Option<String>,
  #[serde(default)]
  pub session_token: Option<String>,
  #[serde(default)]
  pub saved_at: Option<String>,
}

impl AuthFile {
  fn from_user(user: serde_json::Value) -> Self {
    Self {
      connected: true,
      email: user
        .get("email")
        .and_then(|v| v.as_str())
        .map(|s| s.to_ascii_lowercase()),
      api_key: None,
      session_token: user
        .get("sessionToken")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string()),
      saved_at: Some(chrono::Local::now().to_rfc3339()),
    }
  }
}

/// Errors raised by account operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
  #[error("not signed in: {0}")]
  NotAuthenticated(String),
  #[error("relay unreachable: {0}")]
  Network(String),
  #[error("access denied: {0}")]
  Denied(String),
  #[error("invalid relay URL: {0}")]
  InvalidRelay(String),
  #[error("io error: {0}")]
  Io(#[from] std::io::Error),
  #[error("unexpected relay response: {0}")]
  Protocol(String),
}

type Result<T> = std::result::Result<T, Error>;

/// One entry of the relay model catalogue.
#[derive(Debug, Clone, Default, Deserialize)]
#[allow(dead_code)]
pub struct ModelInfo {
  #[serde(default)]
  pub id: String,
  #[serde(default)]
  pub name: String,
  #[serde(default)]
  pub tier: String,
  #[serde(default)]
  pub status: String,
  #[serde(default)]
  pub disabled: bool,
  #[serde(default, rename = "contextWindow")]
  pub context_window: Option<usize>,
  #[serde(default)]
  pub max_output_tokens: Option<u32>,
  #[serde(default, rename = "usageTier")]
  pub usage_tier: Option<String>,
  #[serde(default)]
  pub reasoning: Option<bool>,
  #[serde(default)]
  pub inputs: Option<String>,
  #[serde(default)]
  pub tag: Option<String>,
}

impl ModelInfo {
  pub fn is_pro(&self) -> bool {
    self.tier.eq_ignore_ascii_case("pro")
  }
  pub fn is_active(&self) -> bool {
    !self.disabled && (self.status.is_empty() || self.status == "active")
  }
}

/// Verification snapshot for the signed-in account.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AccountState {
  #[serde(default)]
  pub email: String,
  #[serde(default)]
  pub allowed: bool,
  #[serde(default)]
  pub pro: bool,
  #[serde(default)]
  pub status: String,
  #[serde(default)]
  pub reason: Option<String>,
  #[serde(default)]
  pub message: Option<String>,
  #[serde(default)]
  #[allow(dead_code)]
  pub quota_reset_at: u64,
  #[serde(default)]
  pub models: Vec<ModelInfo>,
}

impl AccountState {
  fn denied(reason: impl Into<String>) -> Self {
    Self {
      allowed: false,
      status: "unverified".to_string(),
      reason: Some(reason.into()),
      ..Self::default()
    }
  }

  /// Human-readable explanation when this account or model cannot be used.
  pub fn denial_for(&self, model: &str) -> Option<String> {
    if !self.allowed {
      return Some(
        self
          .message
          .clone()
          .or_else(|| self.reason.clone())
          .unwrap_or_else(|| "account is not allowed to use the relay".to_string()),
      );
    }
    let wanted = model.trim().to_ascii_lowercase();
    if wanted.is_empty() || self.models.is_empty() {
      return None;
    }
    let entry = self
      .models
      .iter()
      .find(|m| m.id.eq_ignore_ascii_case(&wanted))?;
    if !entry.is_active() {
      return Some(if entry.status == "working_on_it" {
        format!("model '{model}' is under maintenance")
      } else {
        format!("model '{model}' is disabled")
      });
    }
    if entry.is_pro() && !self.pro {
      return Some(format!("model '{model}' requires Pro access"));
    }
    None
  }
}

struct Cached {
  state: AccountState,
  fetched: Option<Instant>,
}

/// Verifies the account against the relay and caches the answer briefly.
pub struct AccountClient {
  client: Client,
  base_url: String,
  credentials: Credentials,
  cache: RwLock<Cached>,
}

impl std::fmt::Debug for AccountClient {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("AccountClient")
      .field("base_url", &self.base_url)
      .field("email", &self.credentials.email)
      .finish()
  }
}

impl AccountClient {
  pub fn new(base_url: impl Into<String>, credentials: Credentials) -> Arc<Self> {
    let base_url = base_url.into().trim_end_matches('/').to_string();
    let client = Client::builder()
      .connect_timeout(Duration::from_secs(10))
      .timeout(Duration::from_secs(20))
      .build()
      .unwrap_or_else(|_| Client::new());
    Arc::new(Self {
      client,
      base_url,
      credentials,
      cache: RwLock::new(Cached {
        state: AccountState::default(),
        fetched: None,
      }),
    })
  }

  pub fn base_url(&self) -> &str {
    &self.base_url
  }

  /// Current state, refreshing at most once per [`VERIFY_TTL`].
  pub async fn state(&self) -> AccountState {
    {
      let guard = self.cache.read().await;
      if let Some(fetched) = guard.fetched
        && fetched.elapsed() < VERIFY_TTL
      {
        return guard.state.clone();
      }
    }
    let mut guard = self.cache.write().await;
    if let Some(fetched) = guard.fetched
      && fetched.elapsed() < VERIFY_TTL
    {
      return guard.state.clone();
    }
    match self.fetch_state().await {
      Ok(state) => {
        guard.state = state.clone();
        guard.fetched = Some(Instant::now());
        state
      }
      Err(error) => {
        // A transient relay failure must not look like a ban: keep the last
        // good answer for a bounded window, then fail closed.
        let keep_last = guard.state.allowed
          && guard
            .fetched
            .map(|f| f.elapsed() < DEGRADED_GRACE)
            .unwrap_or(false);
        if keep_last {
          log::warn!("Account verification failed, keeping last known state: {error}");
          return guard.state.clone();
        }
        let denied = AccountState::denied(format!("cannot verify account: {error}"));
        guard.state = denied.clone();
        guard.fetched = Some(Instant::now());
        denied
      }
    }
  }

  async fn fetch_state(&self) -> Result<AccountState> {
    let url = format!("{}/api/client/verify", self.base_url);
    let mut request = self.client.post(&url);
    if let Some(email) = self.credentials.email.as_ref() {
      request = request.json(&serde_json::json!({ "email": email }));
    } else {
      request = request.json(&serde_json::json!({}));
    }
    if let Some(key) = self.credentials.api_key.as_ref() {
      request = request.header("authorization", format!("Bearer {key}"));
    }
    let response = request
      .send()
      .await
      .map_err(|e| Error::Network(e.to_string()))?;
    let status = response.status();
    let payload: serde_json::Value = response
      .json()
      .await
      .map_err(|e| Error::Protocol(format!("verify response: {e}")))?;
    if status.as_u16() == 401 || status.as_u16() == 403 {
      return Ok(AccountState::denied(
        payload
          .get("error")
          .and_then(|v| v.as_str())
          .unwrap_or("relay rejected the session"),
      ));
    }
    if !status.is_success() {
      return Err(Error::Protocol(format!("verify returned HTTP {status}")));
    }
    Ok(parse_account(&payload, self.credentials.email.as_deref()))
  }

  /// Full catalogue, even when the account is not allowed to chat.
  pub async fn catalog(&self) -> Vec<ModelInfo> {
    let state = self.state().await;
    if !state.models.is_empty() {
      return state.models;
    }
    self.fetch_catalog().await.unwrap_or_default()
  }

  pub async fn fetch_catalog(&self) -> Result<Vec<ModelInfo>> {
    let url = format!("{}/api/public/models", self.base_url);
    let response = self
      .client
      .get(&url)
      .send()
      .await
      .map_err(|e| Error::Network(e.to_string()))?;
    if !response.status().is_success() {
      return Err(Error::Protocol(format!(
        "models returned HTTP {}",
        response.status()
      )));
    }
    let payload: serde_json::Value = response
      .json()
      .await
      .map_err(|e| Error::Protocol(format!("models response: {e}")))?;
    let models = payload
      .get("models")
      .or_else(|| payload.get("data"))
      .and_then(|v| v.as_array())
      .cloned()
      .unwrap_or_default();
    Ok(
      models
        .into_iter()
        .filter_map(|value| serde_json::from_value::<ModelInfo>(value).ok())
        .collect(),
    )
  }
}

fn parse_account(payload: &serde_json::Value, fallback_email: Option<&str>) -> AccountState {
  let user = payload.get("user").cloned().unwrap_or_default();
  let models: Vec<ModelInfo> = payload
    .get("models")
    .and_then(|v| v.as_array())
    .map(|items| {
      items
        .iter()
        .filter_map(|item| serde_json::from_value::<ModelInfo>(item.clone()).ok())
        .collect()
    })
    .unwrap_or_default();
  AccountState {
    email: user
      .get("email")
      .and_then(|v| v.as_str())
      .map(|s| s.to_ascii_lowercase())
      .or_else(|| fallback_email.map(|s| s.to_ascii_lowercase()))
      .unwrap_or_default(),
    allowed: payload
      .get("allowed")
      .and_then(|v| v.as_bool())
      .unwrap_or(false),
    pro: user.get("pro").and_then(|v| v.as_bool()).unwrap_or(false),
    status: user
      .get("status")
      .and_then(|v| v.as_str())
      .unwrap_or("unknown")
      .to_string(),
    reason: payload
      .get("reason")
      .and_then(|v| v.as_str())
      .map(|s| s.to_string()),
    message: payload
      .get("error")
      .and_then(|v| v.as_str())
      .map(|s| s.to_string()),
    quota_reset_at: user
      .get("quotaResetAt")
      .and_then(|v| v.as_u64())
      .unwrap_or(0),
    models,
  }
}

/// Relay URL must be https, or plain http only for a loopback proxy.
pub fn validate_relay_url(url: &str) -> Result<()> {
  let trimmed = url.trim();
  if trimmed.starts_with("https://") {
    return Ok(());
  }
  if trimmed.starts_with("http://") {
    let host = trimmed
      .trim_start_matches("http://")
      .split(['/', ':'])
      .next()
      .unwrap_or_default();
    if matches!(host, "127.0.0.1" | "localhost" | "[::1]") {
      return Ok(());
    }
  }
  Err(Error::InvalidRelay(format!(
    "'{trimmed}' is not https and not a loopback address"
  )))
}

/// CLI auth file lives next to the config so `--config` keeps profiles apart.
pub fn cli_auth_path(config_dir: &Path) -> PathBuf {
  config_dir.join("auth.json")
}

/// Auth file written by the desktop client, read-only fallback.
pub fn desktop_auth_path() -> Option<PathBuf> {
  dirs::config_dir().map(|dir| dir.join("Zarnkina").join("auth.json"))
}

pub fn read_auth_file(path: &Path) -> Option<AuthFile> {
  let raw = std::fs::read_to_string(path).ok()?;
  let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
  if value.get("connected").and_then(|v| v.as_bool()) != Some(true) {
    return None;
  }
  if let Some(user) = value.get("user") {
    let mut file = AuthFile::from_user(user.clone());
    if let Some(key) = value.get("apiKey").and_then(|v| v.as_str()) {
      file.api_key = Some(key.to_string());
    }
    return Some(file);
  }
  serde_json::from_value(value).ok()
}

pub fn write_auth_file(path: &Path, file: &AuthFile) -> Result<()> {
  if let Some(parent) = path.parent() {
    std::fs::create_dir_all(parent)?;
  }
  let payload = serde_json::json!({
    "connected": true,
    "email": file.email,
    "apiKey": file.api_key,
    "savedAt": file.saved_at,
    "user": {
      "email": file.email,
      "sessionToken": file.session_token,
    },
  });
  let body = serde_json::to_string_pretty(&payload)
    .map_err(|e| Error::Protocol(format!("auth file: {e}")))?;
  let tmp = path.with_extension("json.tmp");
  std::fs::write(&tmp, body)?;
  std::fs::rename(&tmp, path)?;
  restrict_permissions(path);
  Ok(())
}

pub fn remove_auth_file(path: &Path) -> Result<()> {
  match std::fs::remove_file(path) {
    Ok(()) => Ok(()),
    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
    Err(error) => Err(error.into()),
  }
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) {
  use std::os::unix::fs::PermissionsExt;
  if let Ok(meta) = std::fs::metadata(path) {
    let mut perms = meta.permissions();
    perms.set_mode(0o600);
    let _ = std::fs::set_permissions(path, perms);
  }
}

#[cfg(not(unix))]
fn restrict_permissions(path: &Path) {
  // Windows: drop inherited ACEs and grant the current user alone. The token on
  // this file is a bearer credential, so `%APPDATA%` defaults are not enough.
  #[cfg(target_os = "windows")]
  {
    use std::process::{Command, Stdio};
    let Ok(user) = std::env::var("USERNAME") else {
      return;
    };
    if user.trim().is_empty() {
      return;
    }
    let _ = Command::new("icacls")
      .arg(path)
      .arg("/inheritance:r")
      .arg("/grant:r")
      .arg(format!("{}:F", user.trim()))
      .stdout(Stdio::null())
      .stderr(Stdio::null())
      .status();
  }
}

/// First credential found wins: env, config, CLI auth file, desktop auth file.
pub fn resolve_credentials(config: &Config, config_dir: &Path) -> Credentials {
  let mut creds = Credentials::default();

  if let Ok(email) = std::env::var("ZARNKINA_EMAIL")
    && !email.trim().is_empty()
  {
    creds.email = Some(email.trim().to_ascii_lowercase());
  }
  if let Ok(key) = std::env::var("ZARNKINA_API_KEY")
    && !key.trim().is_empty()
  {
    creds.api_key = Some(key.trim().to_string());
  }
  if let Ok(token) = std::env::var("ZARNKINA_SESSION_TOKEN")
    && !token.trim().is_empty()
  {
    creds.session_token = Some(token.trim().to_string());
  }

  if creds.api_key.is_none() && creds.email.is_none() {
    for provider in config.providers.values() {
      if provider.provider_type != "zarnkina" {
        continue;
      }
      if creds.api_key.is_none()
        && let Some(key) = provider.api_key.as_ref()
      {
        let resolved = config.resolve_api_key(key);
        if !resolved.trim().is_empty() {
          creds.api_key = Some(resolved.trim().to_string());
        }
      }
      if creds.email.is_none()
        && let Some(email) = provider.email.as_ref()
        && !email.trim().is_empty()
      {
        creds.email = Some(email.trim().to_ascii_lowercase());
      }
    }
  }

  if creds.api_key.is_none()
    && creds.session_token.is_none()
    && let Some(file) = read_auth_file(&cli_auth_path(config_dir))
  {
    creds.email = creds.email.or(file.email);
    creds.api_key = creds.api_key.or(file.api_key);
    creds.session_token = creds.session_token.or(file.session_token);
  }

  if creds.api_key.is_none()
    && creds.session_token.is_none()
    && creds.email.is_none()
    && let Some(path) = desktop_auth_path()
    && let Some(file) = read_auth_file(&path)
  {
    creds.email = file.email;
    creds.session_token = file.session_token;
    creds.api_key = file.api_key;
  }

  creds
}

/// Relay base URL for the default Zarnkina provider, or the public default.
pub fn relay_base(config: &Config) -> String {
  config
    .providers
    .values()
    .find(|p| p.provider_type == "zarnkina")
    .map(|p| p.base_url.clone())
    .unwrap_or_else(|| DEFAULT_RELAY_BASE.to_string())
}

/// Browser OAuth against Google, exchanged by the relay for a session token.
///
/// The client secret never reaches the CLI: only the authorization code is sent
/// to `/api/client/google-token`, which holds the secret server-side.
pub async fn google_login(base_url: &str) -> Result<AuthFile> {
  validate_relay_url(base_url)?;
  let listener = TcpListener::bind(("127.0.0.1", OAUTH_REDIRECT_PORT))
    .map_err(|e| Error::Network(format!("cannot bind loopback listener: {e}")))?;
  let state = uuid::Uuid::new_v4().to_string();
  let url = format!(
    "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope=openid%20email%20profile&access_type=offline&prompt=consent&state={}",
    percent_encode(GOOGLE_CLIENT_ID),
    percent_encode(OAUTH_REDIRECT),
    state
  );
  println!("Open this URL in a browser to sign in:\n\n{url}\n");
  let _ = open_browser(&url);

  let (mut stream, _) = listener
    .accept()
    .map_err(|e| Error::Network(format!("loopback accept failed: {e}")))?;
  let mut reader = BufReader::new(stream.try_clone()?);
  let mut request_line = String::new();
  reader.read_line(&mut request_line)?;
  let path = request_line.split_whitespace().nth(1).unwrap_or_default();
  let code = query_param(path, "code")
    .ok_or_else(|| Error::NotAuthenticated("browser returned no authorization code".into()))?;
  let reply = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 44\r\nConnection: close\r\n\r\nSign-in complete. You can close this window.";
  use std::io::Write;
  let _ = stream.write_all(reply.as_bytes());

  let client = Client::builder()
    .timeout(Duration::from_secs(25))
    .build()
    .map_err(|e| Error::Network(e.to_string()))?;
  let response = client
    .post(format!(
      "{}/api/client/google-token",
      base_url.trim_end_matches('/')
    ))
    .json(&serde_json::json!({
      "code": code,
      "clientId": GOOGLE_CLIENT_ID,
      "redirectUri": OAUTH_REDIRECT,
    }))
    .send()
    .await
    .map_err(|e| Error::Network(e.to_string()))?;
  let status = response.status();
  let payload: serde_json::Value = response
    .json()
    .await
    .map_err(|e| Error::Protocol(format!("token response: {e}")))?;
  if !status.is_success() || payload.get("ok").and_then(|v| v.as_bool()) != Some(true) {
    return Err(Error::Denied(
      payload
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("sign-in rejected")
        .to_string(),
    ));
  }
  let email = payload
    .get("email")
    .and_then(|v| v.as_str())
    .ok_or_else(|| Error::Protocol("no email in token response".into()))?;
  let mut file = AuthFile {
    connected: true,
    email: Some(email.to_ascii_lowercase()),
    api_key: None,
    session_token: payload
      .get("sessionToken")
      .and_then(|v| v.as_str())
      .map(|s| s.to_string()),
    saved_at: Some(chrono::Local::now().to_rfc3339()),
  };
  if !payload
    .get("whitelisted")
    .and_then(|v| v.as_bool())
    .unwrap_or(false)
  {
    file.session_token = None;
    return Err(Error::Denied(format!(
      "'{email}' is not on the whitelist yet; access has to be approved first"
    )));
  }
  Ok(file)
}

/// Verify an API key against the relay before it is written to disk.
pub async fn verify_api_key(base_url: &str, api_key: &str) -> Result<AccountState> {
  validate_relay_url(base_url)?;
  if !api_key.starts_with("zk_live_") {
    return Err(Error::NotAuthenticated(
      "API key must start with zk_live_".to_string(),
    ));
  }
  let client = Client::builder()
    .timeout(Duration::from_secs(20))
    .build()
    .map_err(|e| Error::Network(e.to_string()))?;
  let response = client
    .post(format!(
      "{}/api/client/verify",
      base_url.trim_end_matches('/')
    ))
    .header("authorization", format!("Bearer {api_key}"))
    .json(&serde_json::json!({}))
    .send()
    .await
    .map_err(|e| Error::Network(e.to_string()))?;
  let status = response.status();
  let payload: serde_json::Value = response
    .json()
    .await
    .map_err(|e| Error::Protocol(format!("verify response: {e}")))?;
  let state = parse_account(&payload, None);
  if status.as_u16() == 401 || status.as_u16() == 403 || !state.allowed {
    return Err(Error::Denied(
      state
        .message
        .or(state.reason)
        .unwrap_or_else(|| "relay rejected the key".to_string()),
    ));
  }
  Ok(state)
}

fn percent_encode(value: &str) -> String {
  let mut out = String::with_capacity(value.len());
  for byte in value.bytes() {
    match byte {
      b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
      _ => out.push_str(&format!("%{byte:02X}")),
    }
  }
  out
}

fn query_param(path: &str, key: &str) -> Option<String> {
  let query = path.split_once('?')?.1;
  for pair in query.split('&') {
    let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
    if name == key {
      return Some(decode_component(value));
    }
  }
  None
}

fn decode_component(value: &str) -> String {
  let bytes = value.as_bytes();
  let mut out = Vec::with_capacity(bytes.len());
  let mut index = 0;
  while index < bytes.len() {
    match bytes[index] {
      b'%' if index + 2 < bytes.len() => {
        let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or("");
        if let Ok(byte) = u8::from_str_radix(hex, 16) {
          out.push(byte);
          index += 3;
          continue;
        }
        out.push(bytes[index]);
        index += 1;
      }
      b'+' => {
        out.push(b' ');
        index += 1;
      }
      byte => {
        out.push(byte);
        index += 1;
      }
    }
  }
  String::from_utf8_lossy(&out).into_owned()
}

fn open_browser(url: &str) -> std::io::Result<()> {
  // No shell involved: `rundll32 url.dll,FileProtocolHandler` hands the whole
  // URL to the default browser, so `&` and `%` are never re-parsed.
  #[cfg(target_os = "windows")]
  {
    Command::new("rundll32")
      .args(["url.dll,FileProtocolHandler", url])
      .spawn()?;
  }
  #[cfg(target_os = "macos")]
  {
    Command::new("open").arg(url).spawn()?;
  }
  #[cfg(all(unix, not(target_os = "macos")))]
  {
    Command::new("xdg-open").arg(url).spawn()?;
  }
  Ok(())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn loopback_http_is_allowed_but_public_http_is_not() {
    assert!(validate_relay_url("https://zarnkina-relay.hostless.app").is_ok());
    assert!(validate_relay_url("http://127.0.0.1:8456/v1").is_ok());
    assert!(validate_relay_url("http://localhost:8456/v1").is_ok());
    assert!(validate_relay_url("http://relay.example.com/v1").is_err());
  }

  #[test]
  fn denial_names_the_exact_reason() {
    let state = AccountState {
      allowed: true,
      pro: false,
      models: vec![
        ModelInfo {
          id: "zarnkina-glm-5.3-flash".into(),
          status: "active".into(),
          tier: "free".into(),
          ..ModelInfo::default()
        },
        ModelInfo {
          id: "zarnkina-beam-1.0".into(),
          status: "active".into(),
          tier: "pro".into(),
          ..ModelInfo::default()
        },
        ModelInfo {
          id: "zarnkina-deepseek-v4-pro".into(),
          status: "working_on_it".into(),
          tier: "free".into(),
          ..ModelInfo::default()
        },
      ],
      ..AccountState::default()
    };
    assert!(state.denial_for("zarnkina-glm-5.3-flash").is_none());
    assert!(state.denial_for("unknown-model").is_none());
    assert!(
      state
        .denial_for("zarnkina-beam-1.0")
        .unwrap()
        .contains("Pro")
    );
    assert!(
      state
        .denial_for("zarnkina-deepseek-v4-pro")
        .unwrap()
        .contains("maintenance")
    );
  }
}
