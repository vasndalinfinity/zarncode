use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::Local;
use log::{debug, info, warn};

use crate::approval::ApprovalRuntime;
use crate::background::BackgroundTaskManager;
use crate::config::loader::{data_dir, system_prompt_path};
use crate::config::{
  BackgroundConfig, CompactionConfig, Config, DEFAULT_MAX_CONTEXT_SIZE, HistoryConfig, ModelConfig,
  ProviderConfig, RetryConfig,
};
use crate::error::Result;
use crate::hooks::HookEngine;
use crate::notification::NotificationManager;
use crate::subagents::{LaborMarket, ToolPolicy};
use crate::tools::handlers::{
  AgentHandler, AskUserQuestionHandler, EnterPlanModeHandler, ExitPlanModeHandler, FetchURLHandler,
  GlobHandler, GrepHandler, ReadFileHandler, ReplaceFileHandler, SearchWebHandler,
  SetTodoListHandler, TaskListHandler, TaskOutputHandler, TaskStopHandler, WriteFileHandler,
};
use crate::tools::{ExecutableToolRegistry, ToolRegistry};

// Import platform-specific shell handlers
#[cfg(not(target_os = "windows"))]
use crate::tools::handlers::BashHandler;
#[cfg(target_os = "windows")]
use crate::tools::handlers::PowerShellHandler;

/// Runtime environment errors
#[derive(thiserror::Error, Debug)]
pub enum Error {
  #[error("Failed to load tool definitions from {path:?}: {source}")]
  LoadTools {
    path: PathBuf,
    source: anyhow::Error,
  },

  #[error("Failed to get current directory")]
  GetCurrentDir {
    #[source]
    source: std::io::Error,
  },

  #[error("Failed to read directory: {path}")]
  ReadDir {
    path: PathBuf,
    #[source]
    source: std::io::Error,
  },

  #[error("Failed to read file metadata: {path}")]
  ReadMetadata {
    path: PathBuf,
    #[source]
    source: std::io::Error,
  },

  #[error("Failed to read system prompt from: {path}")]
  ReadSystemPrompt {
    path: PathBuf,
    #[source]
    source: std::io::Error,
  },

  #[error("Tool '{tool_name}' is defined in prompts but no handler is implemented")]
  MissingToolHandler { tool_name: String },

  #[error("Failed to load built-in subagent types: {0}")]
  LaborMarket(#[source] anyhow::Error),
}

impl Error {
  /// Create a read directory error with path
  pub fn read_dir(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
    Error::ReadDir {
      path: path.into(),
      source,
    }
  }

  /// Create a read metadata error with path
  pub fn read_metadata(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
    Error::ReadMetadata {
      path: path.into(),
      source,
    }
  }

  /// Create a read system prompt error with path
  pub fn read_system_prompt(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
    Error::ReadSystemPrompt {
      path: path.into(),
      source,
    }
  }
}

/// Runtime environment arguments for template substitution
///
/// These variables are loaded at startup and used to replace
/// placeholders in the system prompt template.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeArgs {
  /// Current date and time in ISO format (${ZARNCODE_NOW})
  pub now: String,
  /// Working directory absolute path (${ZARNCODE_WORK_DIR})
  pub work_dir: String,
  /// Working directory listing (${ZARNCODE_WORK_DIR_LS})
  pub work_dir_ls: String,
  /// Additional directories info (${ZARNCODE_ADDITIONAL_DIRS_INFO})
  pub additional_dirs_info: String,
  /// AGENTS.md file content (${ZARNCODE_AGENTS_MD})
  pub agents_md: String,
  /// Available skills list (${ZARNCODE_SKILLS})
  pub skills: String,
  /// Role additional info (${ROLE_ADDITIONAL})
  pub role_additional: String,
}

impl RuntimeArgs {
  /// Create a new RuntimeArgs instance by loading all environment data
  pub(crate) fn new() -> Result<Self> {
    Ok(Self {
      now: Self::load_now(),
      work_dir: Self::load_work_dir()?,
      work_dir_ls: Self::load_work_dir_ls()?,
      additional_dirs_info: String::new(), // TODO: Load from config or env
      agents_md: Self::load_agents_md(),
      skills: Self::load_skills()?,
      role_additional: String::new(), // TODO: Load from config or env
    })
  }

  /// Get current timestamp in ISO format
  fn load_now() -> String {
    Local::now().to_rfc3339()
  }

  /// Get current working directory
  fn load_work_dir() -> Result<String> {
    env::current_dir()
      .map(|p| p.to_string_lossy().to_string())
      .map_err(|e| Error::GetCurrentDir { source: e }.into())
  }

  /// Get directory listing of working directory
  fn load_work_dir_ls() -> Result<String> {
    let work_dir = env::current_dir().map_err(|e| Error::GetCurrentDir { source: e })?;
    let mut entries = Vec::new();

    for entry in fs::read_dir(&work_dir).map_err(|e| Error::read_dir(&work_dir, e))? {
      let entry = entry.map_err(|e| Error::read_dir(&work_dir, e))?;
      let name = entry.file_name().to_string_lossy().to_string();
      let metadata = entry
        .metadata()
        .map_err(|e| Error::read_metadata(&work_dir, e))?;
      let size = metadata.len();
      let is_dir = metadata.is_dir();
      let permissions = metadata.permissions();
      let mode = if permissions.readonly() { "r--" } else { "rw-" };

      let perms = if is_dir {
        "drwxr-xr-x".to_string()
      } else {
        format!("-{}r--r--", mode)
      };
      let size_str = if is_dir {
        String::new()
      } else {
        format!("{}", size)
      };
      entries.push(format!("{}{:>10} {}", perms, size_str, name));
    }

    entries.sort();
    Ok(entries.join("\n"))
  }

  /// Load AGENTS.md content if exists
  fn load_agents_md() -> String {
    let agents_path = PathBuf::from("AGENTS.md");
    fs::read_to_string(&agents_path).unwrap_or_default()
  }

  /// Load available skills (placeholder for now)
  fn load_skills() -> Result<String> {
    // TODO: Implement skills discovery
    // For now, return empty or load from a skills directory
    Ok(String::new())
  }
}

/// Role of the runtime in the agent hierarchy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum RuntimeRole {
  /// Root agent runtime.
  #[default]
  Root,
  /// Subagent runtime.
  Subagent {
    /// Stable subagent instance id.
    agent_id: String,
    /// Subagent type name.
    subagent_type: String,
  },
}

impl RuntimeRole {
  /// Returns true if this is a root runtime.
  pub fn is_root(&self) -> bool {
    matches!(self, RuntimeRole::Root)
  }

  /// Returns the subagent id if this is a subagent runtime.
  #[allow(dead_code)]
  pub fn agent_id(&self) -> Option<&str> {
    match self {
      RuntimeRole::Root => None,
      RuntimeRole::Subagent { agent_id, .. } => Some(agent_id),
    }
  }
}

/// Runtime settings applied before handlers capture the root runtime.
#[derive(Debug, Clone)]
pub(crate) struct RuntimeOptions {
  pub max_concurrent_agents: usize,
  pub active_agent: Option<String>,
  pub non_interactive: bool,
  pub max_steps: usize,
}

#[cfg(test)]
mod priority_tests {
  use super::*;
  #[test]
  fn child_limits_and_tools_are_intersections_not_new_grants() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("agents")).unwrap();
    let definition = crate::subagents::definitions::AgentFile {
      schema_version: 1,
      name: "reviewer".into(),
      description: "Review".into(),
      prompt: "RESTRICTED ROLE".into(),
      model: None,
      mode: crate::subagents::definitions::AgentMode::Both,
      tools: vec!["ReadFile".into()],
      max_steps: 3,
      supports_background: false,
    };
    std::fs::write(
      directory.path().join("agents/reviewer.yaml"),
      serde_yaml::to_string(&definition).unwrap(),
    )
    .unwrap();
    let mut parent = Runtime::for_test(Config::default());
    parent.max_steps = 2;
    parent.non_interactive = true;
    parent.tool_registry = Arc::new(ToolRegistry::load_builtin().unwrap());
    parent.labor_market = Arc::new(LaborMarket::load_with_user(directory.path()).unwrap());
    let child = parent.copy_for_subagent(
      "child".into(),
      "reviewer".into(),
      None,
      &crate::subagents::ToolPolicy::default(),
    );
    assert_eq!(child.max_steps, 2);
    assert!(!child.role.is_root());
    assert_eq!(child.role.agent_id(), Some("child"));
    assert!(child.non_interactive);
    assert!(child.tool_registry.get("ReadFile").is_some());
    assert!(child.tool_registry.get("WriteFile").is_none());
    assert!(child.tool_registry.get("Agent").is_none());
    assert!(child.render_system_prompt().contains("RESTRICTED ROLE"));
  }
}
impl Default for RuntimeOptions {
  fn default() -> Self {
    Self {
      max_concurrent_agents: 3,
      active_agent: None,
      non_interactive: false,
      max_steps: crate::llm::session::actor::DEFAULT_MAX_STEPS_PER_TURN,
    }
  }
}

/// Runtime holds the effective configuration, system prompt template, arguments,
/// and tool registries. All fields are loaded at startup and are read-only during
/// the session.
#[derive(Debug, Clone)]
pub(crate) struct Runtime {
  pub agent_slots: Arc<tokio::sync::Semaphore>,
  pub active_agent: Option<String>,
  /// Non-interactive clients must not inherit persisted session YOLO grants.
  pub non_interactive: bool,
  pub max_steps: usize,
  /// Shared provider-reported usage across the main agent and its children.
  pub usage: Arc<crate::usage::UsageStore>,
  /// Relay identity resolved at startup (env, config, auth files).
  pub credentials: crate::account::Credentials,
  /// Verification cache for the relay account.
  pub account: Arc<crate::account::AccountClient>,
  /// Local quota ledger shared with subagents.
  pub quota: Arc<crate::account::quota::QuotaLedger>,
  /// Identity of the current user turn. The relay charges one prompt per turn,
  /// so every provider call inside a turn carries this id.
  pub turn_id: Arc<std::sync::Mutex<String>>,
  pub turn_seq: Arc<std::sync::atomic::AtomicU64>,
  /// Model alias selected for this session (/model); None means the config default.
  pub active_model: Arc<std::sync::Mutex<Option<String>>>,
  /// Whether the model should show its reasoning (/thinking). Off means the
  /// upstream skips the thinking and answers in seconds.
  pub thinking: Arc<std::sync::Mutex<bool>>,
  /// Last known relay account state, refreshed in the background so the UI can
  /// show account details without awaiting the network from a key handler.
  pub account_state: Arc<std::sync::Mutex<Option<crate::account::AccountState>>>,
  /// When the cached account state was captured.
  pub account_state_at: Arc<std::sync::Mutex<Option<std::time::Instant>>>,
  /// Models the relay publishes right now, keyed by alias. The relay catalogue
  /// changes when the operator publishes or retires a model, so the picker and
  /// the provider factory read this instead of trusting a stale config file.
  pub live_models: Arc<std::sync::Mutex<std::collections::HashMap<String, crate::config::ModelConfig>>>,
  /// Effective configuration (user config + CLI overrides applied)
  pub(crate) config: Arc<Config>,
  /// Template arguments for substitution
  pub args: RuntimeArgs,
  /// The raw system prompt template (before substitution)
  pub system_prompt_template: String,
  /// Tool definitions registry (loaded from Markdown files)
  pub tool_registry: Arc<ToolRegistry>,
  /// Executable tool registry for dispatching and previewing tool calls
  pub executable_registry: Arc<ExecutableToolRegistry>,
  /// Background task manager (session is bound after session creation)
  pub background_manager: Arc<BackgroundTaskManager>,
  /// Notification manager (session is bound after session creation)
  pub notification_manager: Arc<NotificationManager>,
  /// Lifecycle hook engine
  pub hook_engine: Arc<HookEngine>,
  /// Cross-session approval runtime
  pub approval_runtime: Arc<ApprovalRuntime>,
  /// Built-in subagent type registry
  pub labor_market: Arc<LaborMarket>,
  /// Role of this runtime.
  pub role: RuntimeRole,
  /// Optional model alias override for subagents.
  pub model_override: Option<String>,
  /// Configuration directory used to load `config.toml`.
  pub(crate) config_dir: PathBuf,
}

impl Runtime {
  /// Name of the configured provider that points at the relay.
  ///
  /// The relay publishes its own catalogue; the provider entry in the config
  /// file is only a name plus a base URL, so the name is resolved by matching
  /// the URL.
  pub fn relay_provider_name(&self) -> Option<String> {
    let base = self.account.base_url().trim_end_matches('/');
    self
      .config
      .providers
      .iter()
      .find(|(_, provider)| {
        provider
          .base_url
          .trim_end_matches('/')
          .eq_ignore_ascii_case(base)
      })
      .map(|(name, _)| name.clone())
  }

  /// Publish the relay's current catalogue so the picker and the provider
  /// factory stop trusting a stale config file.
  pub fn apply_live_catalogue(&self, models: &[crate::account::ModelInfo]) {
    let Some(provider) = self.relay_provider_name() else {
      return;
    };
    let mut map = std::collections::HashMap::new();
    for model in models.iter().filter(|m| m.is_active() && !m.id.is_empty()) {
      let short = model
        .id
        .strip_prefix(&format!("{provider}-"))
        .unwrap_or(model.id.as_str())
        .to_string();
      let alias = format!("{provider}/{short}");
      map.insert(
        alias,
        crate::config::ModelConfig {
          provider: provider.clone(),
          model: model.id.clone(),
          max_context_size: model.context_window,
          temperature: None,
          max_tokens: model.max_output_tokens,
          supports_streaming: true,
          supports_vision: model
            .inputs
            .as_deref()
            .map(|inputs| inputs.contains("image"))
            .unwrap_or(false),
        },
      );
    }
    if let Ok(mut guard) = self.live_models.lock() {
      *guard = map;
    }
  }

  /// Live catalogue entry for an alias, if the relay publishes it.
  pub fn live_model_config(&self, alias: &str) -> Option<crate::config::ModelConfig> {
    self
      .live_models
      .lock()
      .ok()
      .and_then(|map| map.get(alias).cloned())
  }
}

#[cfg(test)]
mod branding_tests {
  use super::*;

  #[test]
  fn legacy_prompt_variables_remain_supported() {
    let directory = tempfile::tempdir().unwrap();
    let prompts = directory.path().join("prompts");
    fs::create_dir_all(&prompts).unwrap();
    fs::write(
      prompts.join("system.md"),
      "${IRONCODE_NOW} ${ZARNCODE_WORK_DIR}",
    )
    .unwrap();
    let mut runtime = Runtime::for_test(Config::default());
    runtime.system_prompt_template = Runtime::load_system_prompt_template(directory.path());
    runtime.args.now = "today".to_string();
    runtime.args.work_dir = "workspace".to_string();
    assert_eq!(runtime.render_system_prompt(), "today workspace");
  }
}

impl Runtime {
  /// Create a new Runtime instance by loading all environment data
  ///
  /// Loads system prompt from data_dir/prompts/system.md
  /// Loads tools from data_dir/prompts/tools/
  /// Returns empty string if prompt file doesn't exist
  pub(crate) fn new_with_options(
    data_dir: &Path,
    config: Arc<Config>,
    config_dir: impl Into<PathBuf>,
    options: RuntimeOptions,
  ) -> Result<Self> {
    let config_dir = config_dir.into();
    let system_prompt_template = Self::load_system_prompt_template(data_dir);
    let mut args = RuntimeArgs::new()?;

    let credentials = crate::account::resolve_credentials(&config, &config_dir);
    let relay_base = crate::account::relay_base(&config);

    let background_manager = Arc::new(BackgroundTaskManager::new(
      data_dir.to_path_buf(),
      config.background.clone(),
    ));
    let notification_manager = Arc::new(NotificationManager::new(
      data_dir.to_path_buf(),
      config.notifications.clone(),
    ));
    let hook_engine = Arc::new(HookEngine::new(
      config.hooks.clone(),
      std::env::current_dir().ok(),
    ));
    let approval_runtime = Arc::new(ApprovalRuntime::new());
    let labor_market =
      Arc::new(LaborMarket::load_with_user(&config_dir).map_err(Error::LaborMarket)?);

    // Create the Agent handler first; it is bound to Runtime after construction
    // to break the cyclic dependency (Runtime owns the registry, Agent needs Runtime).
    let agent_handler = Arc::new(AgentHandler::new());

    // Load executable tool registry first (handlers must be registered before checking)
    let executable_registry = Arc::new(Self::load_executable_tools(
      &background_manager,
      agent_handler.clone(),
    ));

    // Load tool definitions from Markdown files
    let mut tool_registry = Arc::new(Self::load_tools(data_dir)?);

    for agent in labor_market.iter() {
      if let Some(model) = &agent.default_model
        && config.get_model(model).is_none()
      {
        return Err(
          crate::config::Error::ModelNotFound {
            model: model.clone(),
          }
          .into(),
        );
      }
      if agent.max_steps.is_some() {
        for name in &agent.tool_policy.tools {
          if !executable_registry.has(name) {
            return Err(
              Error::MissingToolHandler {
                tool_name: name.clone(),
              }
              .into(),
            );
          }
        }
      }
    }
    if let Some(registry) = Arc::get_mut(&mut tool_registry)
      && let Some(tool) = registry.get_mut("Agent")
    {
      let mut agents: Vec<_> = labor_market
        .iter()
        .filter(|a| a.mode != crate::subagents::definitions::AgentMode::Primary)
        .collect();
      agents.sort_by(|a, b| a.name.cmp(&b.name));
      tool.description.push_str("\nAvailable subagents:\n");
      for agent in &agents {
        tool
          .description
          .push_str(&format!("- {}: {}\n", agent.name, agent.description));
      }
      if let Some(parameter) = tool
        .parameters
        .get_mut("properties")
        .and_then(|p| p.get_mut("subagent_type"))
      {
        parameter["enum"] = serde_json::json!(agents.iter().map(|a| &a.name).collect::<Vec<_>>());
      }
    }
    // Check that all defined tools have corresponding handlers
    Self::validate_tool_handlers(&tool_registry, &executable_registry)?;

    let mut max_steps = options.max_steps;
    if let Some(name) = &options.active_agent {
      let profile = labor_market.require(name).map_err(Error::LaborMarket)?;
      if profile.mode == crate::subagents::definitions::AgentMode::Subagent {
        return Err(Error::LaborMarket(anyhow::anyhow!("Agent '{name}' is subagent-only")).into());
      }
      args.role_additional = profile.role_additional.clone();
      max_steps = max_steps.min(profile.max_steps.unwrap_or(max_steps));
      let allowed: Vec<String> = tool_registry
        .all()
        .iter()
        .filter(|t| profile.tool_policy.allows(&t.name))
        .map(|t| t.name.clone())
        .collect();
      tool_registry = Arc::new(tool_registry.filter(&allowed));
    }
    let ping_relay = relay_base.clone();
    let runtime = Self {
      agent_slots: Arc::new(tokio::sync::Semaphore::new(options.max_concurrent_agents)),
      active_agent: options.active_agent,
      non_interactive: options.non_interactive,
      max_steps,
      usage: Arc::new(crate::usage::UsageStore::new(data_dir)),
      credentials: credentials.clone(),
      account: crate::account::AccountClient::new(relay_base, credentials.clone()),
      quota: Arc::new(crate::account::quota::QuotaLedger::new(
        data_dir,
        credentials.quota_label(),
        config.quota.enforce,
      )),
      turn_id: Arc::new(std::sync::Mutex::new(String::new())),
      turn_seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
      active_model: Arc::new(std::sync::Mutex::new(None)),
      thinking: Arc::new(std::sync::Mutex::new(config.default_thinking)),
      account_state: Arc::new(std::sync::Mutex::new(None)),
      account_state_at: Arc::new(std::sync::Mutex::new(None)),
      live_models: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
      config,
      args,
      system_prompt_template,
      tool_registry,
      executable_registry,
      background_manager,
      notification_manager,
      hook_engine,
      approval_runtime,
      labor_market,
      role: RuntimeRole::Root,
      model_override: None,
      config_dir,
    };

    // Bind the runtime to the Agent handler now that it is fully constructed.
    let _ = agent_handler.bind_runtime(Arc::new(runtime.clone()));

    // Presence: the relay refuses chat without a beat in the last 30 seconds.
    {
      let relay = ping_relay.clone();
      let creds = credentials.clone();
      tokio::spawn(async move {
        let client = reqwest::Client::new();
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
          tick.tick().await;
          let url = format!("{}/api/client/ping", relay.trim_end_matches('/'));
          let mut req = client
            .post(&url)
            .header("x-zarnkina-client", format!("cli/{}", env!("CARGO_PKG_VERSION")));
          if let Some(token) = creds.session_token.as_deref().filter(|s| !s.is_empty()) {
            req = req.header("x-zarnkina-token", token);
          } else if let Some(key) = creds.api_key.as_deref().filter(|s| !s.is_empty()) {
            req = req.header("authorization", format!("Bearer {key}"));
          } else {
            continue;
          }
          let _ = req.send().await;
        }
      });
    }

    Ok(runtime)
  }

  #[cfg(test)]
  pub(crate) fn for_test(config: Config) -> Self {
    Self {
      agent_slots: Arc::new(tokio::sync::Semaphore::new(3)),
      active_agent: None,
      non_interactive: false,
      max_steps: RuntimeOptions::default().max_steps,
      usage: Arc::new(crate::usage::UsageStore::in_memory()),
      credentials: crate::account::Credentials::default(),
      account: crate::account::AccountClient::new(
        "https://127.0.0.1:1",
        crate::account::Credentials::default(),
      ),
      quota: Arc::new(crate::account::quota::QuotaLedger::in_memory(false)),
      turn_id: Arc::new(std::sync::Mutex::new(String::new())),
      turn_seq: Arc::new(std::sync::atomic::AtomicU64::new(0)),
      active_model: Arc::new(std::sync::Mutex::new(None)),
      thinking: Arc::new(std::sync::Mutex::new(config.default_thinking)),
      account_state: Arc::new(std::sync::Mutex::new(None)),
      account_state_at: Arc::new(std::sync::Mutex::new(None)),
      live_models: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
      config_dir: std::path::PathBuf::from("."),
      config: Arc::new(config),
      args: RuntimeArgs {
        now: String::new(),
        work_dir: String::new(),
        work_dir_ls: String::new(),
        additional_dirs_info: String::new(),
        agents_md: String::new(),
        skills: String::new(),
        role_additional: String::new(),
      },
      system_prompt_template: String::new(),
      tool_registry: Arc::new(ToolRegistry::default()),
      executable_registry: Arc::new(ExecutableToolRegistry::new()),
      background_manager: Arc::new(BackgroundTaskManager::new(
        std::path::PathBuf::from("."),
        crate::config::BackgroundConfig::default(),
      )),
      notification_manager: Arc::new(NotificationManager::new(
        std::path::PathBuf::from("."),
        crate::config::NotificationConfig::default(),
      )),
      hook_engine: Arc::new(HookEngine::empty()),
      approval_runtime: Arc::new(ApprovalRuntime::new()),
      labor_market: Arc::new(LaborMarket::new()),
      role: RuntimeRole::Root,
      model_override: None,
    }
  }

  /// Load and initialize the executable tool registry with all handlers
  fn load_executable_tools(
    manager: &Arc<BackgroundTaskManager>,
    agent_handler: Arc<AgentHandler>,
  ) -> ExecutableToolRegistry {
    let mut registry = ExecutableToolRegistry::new();
    registry.register("Agent", Box::new(agent_handler));
    registry.register("ReadFile", Box::new(ReadFileHandler::new()));
    registry.register("WriteFile", Box::new(WriteFileHandler::new()));
    registry.register("ReplaceFile", Box::new(ReplaceFileHandler::new()));
    registry.register("Grep", Box::new(GrepHandler::new()));
    registry.register("Glob", Box::new(GlobHandler::new()));
    registry.register("AskUserQuestion", Box::new(AskUserQuestionHandler::new()));
    registry.register("SetTodoList", Box::new(SetTodoListHandler::new()));
    registry.register("FetchURL", Box::new(FetchURLHandler::new()));
    registry.register("SearchWeb", Box::new(SearchWebHandler::new()));
    registry.register("EnterPlanMode", Box::new(EnterPlanModeHandler::new()));
    registry.register("ExitPlanMode", Box::new(ExitPlanModeHandler::new()));
    registry.register("TaskList", Box::new(TaskListHandler::new(manager.clone())));
    registry.register(
      "TaskOutput",
      Box::new(TaskOutputHandler::new(manager.clone())),
    );
    registry.register("TaskStop", Box::new(TaskStopHandler::new(manager.clone())));

    // Register platform-specific shell handler
    #[cfg(target_os = "windows")]
    registry.register("PowerShell", Box::new(PowerShellHandler::new()));
    #[cfg(not(target_os = "windows"))]
    registry.register("Bash", Box::new(BashHandler::new(manager.clone())));

    registry
  }

  /// Missing assets use embedded core tools; explicit user folders remain overrides.
  fn load_tools(data_dir: &Path) -> Result<ToolRegistry> {
    let directory = data_dir.join("prompts").join("tools");
    let registry = match fs::metadata(&directory) {
      Ok(_) => ToolRegistry::load_from_dir(&directory).map_err(|source| Error::LoadTools {
        path: directory.clone(),
        source,
      })?,
      Err(error) if error.kind() == std::io::ErrorKind::NotFound => ToolRegistry::load_builtin()
        .map_err(|source| Error::LoadTools {
          path: PathBuf::from("bundled core tools"),
          source,
        })?,
      Err(error) => return Err(error.into()),
    };
    let names: Vec<String> = registry
      .all()
      .iter()
      .filter(|tool| {
        if cfg!(target_os = "windows") {
          tool.name != "Bash"
        } else {
          tool.name != "PowerShell"
        }
      })
      .map(|tool| tool.name.clone())
      .collect();
    let mut registry = registry.filter(&names);
    let shell = if cfg!(target_os = "windows") {
      "PowerShell (powershell.exe)"
    } else {
      "bash (/bin/bash)"
    };
    for name in ["Bash", "PowerShell"] {
      if let Some(tool) = registry.get_mut(name) {
        tool.description = tool.description.replace("${SHELL}", shell);
      }
    }
    info!("Loaded {} tool definitions", registry.len());
    Ok(registry)
  }

  /// Validate that all tools defined in registry have corresponding handlers
  /// Tools marked with `no_handler: true` are skipped from validation
  fn validate_tool_handlers(
    tool_registry: &ToolRegistry,
    executable_registry: &ExecutableToolRegistry,
  ) -> Result<()> {
    for tool in tool_registry.all() {
      // Skip tools that are marked as not having a handler
      if tool.no_handler {
        log::debug!(
          "Skipping handler check for tool '{}' (no_handler: true)",
          tool.name
        );
        continue;
      }
      // Skip platform-specific shell tools based on OS
      #[cfg(target_os = "windows")]
      if tool.name == "Bash" {
        log::debug!("Skipping handler check for 'Bash' tool on Windows system");
        continue;
      }
      #[cfg(not(target_os = "windows"))]
      if tool.name == "PowerShell" {
        log::debug!("Skipping handler check for 'PowerShell' tool on non-Windows system");
        continue;
      }
      if !executable_registry.has(&tool.name) {
        return Err(
          Error::MissingToolHandler {
            tool_name: tool.name.clone(),
          }
          .into(),
        );
      }
    }
    Ok(())
  }

  /// Load the system prompt template from config directory
  ///
  /// Reads from config_dir/prompts/system.md
  /// Returns empty string if file doesn't exist
  fn load_system_prompt_template(config_dir: &Path) -> String {
    let prompt_path = system_prompt_path(config_dir);
    debug!("Loading system prompt from: {:?}", prompt_path);

    fs::read_to_string(&prompt_path)
      .map(|template| template.replace("IRONCODE_", "ZARNCODE_"))
      .unwrap_or_else(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
          return include_str!("../../prompts/system.md").to_string();
        }
        warn!("Failed to load system prompt from {:?}: {}", prompt_path, e);
        String::new()
      })
  }

  /// Whether automatic compaction is enabled.
  #[allow(dead_code)]
  pub fn enable_compaction(&self) -> bool {
    self.config.compaction.enabled
  }

  /// Get the compaction configuration.
  pub fn compaction_config(&self) -> &CompactionConfig {
    &self.config.compaction
  }

  /// Mark the start of a user turn. Every relay call made while this id is
  /// current belongs to the same prompt, so the relay charges once.
  pub fn begin_turn(&self) {
    let seq = self
      .turn_seq
      .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
      + 1;
    let id = format!("cli-{}-{}", std::process::id(), seq);
    if let Ok(mut slot) = self.turn_id.lock() {
      *slot = id;
    }
  }

  /// Get the default model name.
  pub fn default_model(&self) -> String {
    self.config.default_model.clone()
  }

  /// Get the default model configuration.
  pub fn default_model_config(&self) -> Option<&ModelConfig> {
    self.config.default_model_config()
  }

  /// Get the max context size of the default model.
  pub fn default_model_max_context_size(&self) -> usize {
    self
      .config
      .default_model_config()
      .and_then(|m| m.max_context_size)
      .unwrap_or(DEFAULT_MAX_CONTEXT_SIZE)
  }

  /// Get the data directory.
  pub fn data_dir(&self) -> PathBuf {
    data_dir(&self.config)
  }

  /// Get the history configuration.
  pub fn history_config(&self) -> HistoryConfig {
    self.config.history.clone()
  }

  /// Whether YOLO mode is enabled.
  pub fn yolo(&self) -> bool {
    self.config.yolo
  }

  /// Get the list of tools to auto-approve.
  pub fn auto_approve(&self) -> Vec<String> {
    self.config.auto_approve.clone()
  }

  /// Whether default thinking mode is enabled.
  pub fn default_thinking(&self) -> bool {
    self
      .thinking
      .lock()
      .map(|guard| *guard)
      .unwrap_or(self.config.default_thinking)
  }

  /// Turn the model's reasoning on or off for this session.
  ///
  /// With reasoning off the upstream answers in seconds instead of thinking for
  /// a minute before the first token.
  pub fn set_thinking(&self, enabled: bool) {
    if let Ok(mut guard) = self.thinking.lock() {
      *guard = enabled;
    }
  }

  /// Get the background task configuration.
  #[allow(dead_code)]
  pub fn background_config(&self) -> BackgroundConfig {
    self.config.background.clone()
  }

  /// Get the configuration directory.
  pub fn config_dir(&self) -> &Path {
    &self.config_dir
  }

  /// Get the approval runtime.
  pub fn approval_runtime(&self) -> Arc<ApprovalRuntime> {
    self.approval_runtime.clone()
  }

  /// Verification client for a relay base URL. Reuses the shared client when the
  /// URL matches so subagents do not each open their own cache.
  pub fn account_for(&self, base_url: &str) -> Arc<crate::account::AccountClient> {
    if base_url.trim_end_matches('/') == self.account.base_url() {
      return self.account.clone();
    }
    crate::account::AccountClient::new(base_url, self.credentials.clone())
  }

  /// Get the background task manager.
  pub fn background_manager(&self) -> Arc<BackgroundTaskManager> {
    self.background_manager.clone()
  }

  /// Get the notification manager.
  pub fn notification_manager(&self) -> Arc<NotificationManager> {
    self.notification_manager.clone()
  }

  /// Get the hook engine.
  pub fn hook_engine(&self) -> Arc<HookEngine> {
    self.hook_engine.clone()
  }

  /// Publish notifications for all terminal background tasks.
  pub fn publish_task_notifications(&self) {
    self
      .background_manager
      .reconcile(&self.notification_manager);
  }

  /// Get a provider by name.
  pub fn get_provider(&self, name: &str) -> Option<&ProviderConfig> {    self.config.get_provider(name)
  }

  /// Resolve an API key (handles env var substitution like "${OPENAI_API_KEY}").
  pub fn resolve_api_key(&self, key: &str) -> String {
    self.config.resolve_api_key(key)
  }

  /// Get the retry configuration.
  pub fn retry_config(&self) -> RetryConfig {
    self.config.retry.clone()
  }

  /// Assign role metadata. Use copy_for_subagent to also inherit tool and budget restrictions.
  pub fn set_role(&mut self, role: RuntimeRole) {
    self.role = role;
  }

  /// Create a child runtime for a subagent instance.
  pub fn copy_for_subagent(
    &self,
    agent_id: String,
    subagent_type: String,
    model_override: Option<String>,
    tool_policy: &ToolPolicy,
  ) -> Self {
    let allowed_tools: Vec<String> = match tool_policy.mode {
      crate::subagents::ToolPolicyMode::Inherit => self
        .tool_registry
        .all()
        .iter()
        .map(|t| t.name.clone())
        .collect(),
      crate::subagents::ToolPolicyMode::Allowlist => tool_policy.tools.clone(),
    };

    let allowed_tools: Vec<String> = allowed_tools
      .into_iter()
      .filter(|name| {
        name != "Agent"
          && name != "Task"
          && self.tool_registry.get(name).is_some()
          && self
            .labor_market
            .get(&subagent_type)
            .is_none_or(|definition| definition.tool_policy.allows(name))
      })
      .collect();
    let tool_registry = Arc::new(self.tool_registry.filter(&allowed_tools));

    let mut args = self.args.clone();
    let mut max_steps = self.max_steps;
    if let Some(definition) = self.labor_market.get(&subagent_type) {
      args.role_additional = definition.role_additional.clone();
      max_steps = max_steps.min(definition.max_steps.unwrap_or(max_steps));
    }
    let mut child = Self {
      agent_slots: self.agent_slots.clone(),
      active_agent: None,
      non_interactive: self.non_interactive,
      max_steps,
      usage: self.usage.clone(),
      credentials: self.credentials.clone(),
      account: self.account.clone(),
      quota: self.quota.clone(),
      turn_id: self.turn_id.clone(),
      turn_seq: self.turn_seq.clone(),
      active_model: self.active_model.clone(),
      thinking: self.thinking.clone(),
      account_state: self.account_state.clone(),
      account_state_at: self.account_state_at.clone(),
      live_models: self.live_models.clone(),
      args,
      system_prompt_template: self.system_prompt_template.clone(),
      tool_registry,
      executable_registry: self.executable_registry.clone(),
      background_manager: self.background_manager.clone(),
      notification_manager: self.notification_manager.clone(),
      hook_engine: self.hook_engine.clone(),
      approval_runtime: self.approval_runtime.clone(),
      config: self.config.clone(),
      labor_market: self.labor_market.clone(),
      role: self.role.clone(),
      model_override,
      config_dir: self.config_dir.clone(),
    };
    child.set_role(RuntimeRole::Subagent {
      agent_id,
      subagent_type,
    });
    child
  }

  /// Render the system prompt with all template variables substituted
  pub fn render_system_prompt(&self) -> String {
    let rendered = self
      .system_prompt_template
      .replace("${ZARNCODE_NOW}", &self.args.now)
      .replace("${ZARNCODE_WORK_DIR}", &self.args.work_dir)
      .replace("${ZARNCODE_WORK_DIR_LS}", &self.args.work_dir_ls)
      .replace(
        "${ZARNCODE_ADDITIONAL_DIRS_INFO}",
        &self.args.additional_dirs_info,
      )
      .replace("${ZARNCODE_AGENTS_MD}", &self.args.agents_md)
      .replace("${ZARNCODE_SKILLS}", &self.args.skills)
      .replace("${ROLE_ADDITIONAL}", &self.args.role_additional);
    if !self.system_prompt_template.contains("${ROLE_ADDITIONAL}")
      && !self.args.role_additional.trim().is_empty()
    {
      format!("{rendered}\n\n{}", self.args.role_additional)
    } else {
      rendered
    }
  }
}
