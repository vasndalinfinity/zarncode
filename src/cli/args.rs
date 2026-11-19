//! Command line argument parsing

use clap::{Parser, Subcommand, ValueEnum};

use crate::config::loader::load_config_from_dir;
use crate::config::{AppConfig, CliOverrides, Config};
use crate::error::Result;
use std::path::PathBuf;

/// Machine-readable output is newline-delimited JSON, schema version 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
  Text,
  Jsonl,
}

#[derive(Debug, Subcommand)]
pub enum CliCommand {
  /// Create, inspect, and validate user-owned agent definitions
  Agent {
    #[command(subcommand)]
    command: AgentCommand,
  },
  /// Sign in to the Zarnkina relay, inspect the session, or sign out
  Auth {
    #[command(subcommand)]
    command: AuthCommand,
  },
  /// Show local quota usage, or reset the ledger
  Quota {
    /// Rebuild the quota ledger (use after a corrupt-file error)
    #[arg(long)]
    reset: bool,
  },
  /// List models served by the relay with their tier and status
  Models,
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
  /// Sign in with a browser (Google) or with an API key
  Login {
    /// Zarnkina API key (zk_live_...); skips the browser flow
    #[arg(long, value_name = "KEY")]
    api_key: Option<String>,
    /// Relay base URL to sign in against
    #[arg(long, value_name = "URL")]
    relay: Option<String>,
  },
  /// Print the current session, account state, and catalogue count
  Status,
  /// Remove the stored CLI session
  Logout,
  /// Explain how to get access when the account is not whitelisted yet
  Request,
}

#[derive(Debug, Subcommand)]
pub enum AgentCommand {
  List,
  Validate,
  Create {
    name: String,
    #[arg(long, default_value = "Custom coding agent")]
    description: String,
    #[arg(
      long,
      default_value = "Review the task carefully and use only your allowed tools."
    )]
    prompt: String,
    #[arg(long)]
    model: Option<String>,
    #[arg(long = "tool")]
    tools: Vec<String>,
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=10000))]
    max_steps: Option<u32>,
    #[arg(long, default_value = "both", value_parser = ["primary", "subagent", "both"])]
    mode: String,
  },
}

/// Zarncode - AI-powered terminal code assistant
#[derive(Debug, Parser)]
#[command(name = "zarncode", bin_name = "zarncode")]
#[command(about = "AI-powered terminal code assistant")]
#[command(version)]
pub struct Args {
  /// Maximum simultaneously admitted foreground/background agent workers (1..32; default 3)
  #[arg(long, value_parser = clap::value_parser!(u32).range(1..=32))]
  pub max_concurrent_agents: Option<u32>,
  #[command(subcommand)]
  pub command: Option<CliCommand>,

  /// Select a built-in or user-defined primary agent profile
  #[arg(long, value_name = "NAME")]
  pub agent: Option<String>,
  /// Maximum logical model steps per task and per delegated agent (1..10000)
  #[arg(long, value_parser = clap::value_parser!(u32).range(1..=10000))]
  pub max_steps: Option<u32>,
  /// Run one task without the terminal UI; accept a prompt, stdin, or both
  #[arg(short = 'p', long, conflicts_with_all = ["preview", "background_task_worker"])]
  pub print: bool,

  /// Task prompt for --print; piped stdin is appended as context
  #[arg(value_name = "PROMPT", requires = "print")]
  pub prompt: Option<String>,

  /// Print plain text or schema-versioned JSON events (requires --print)
  #[arg(long, value_enum, requires = "print")]
  pub output_format: Option<OutputFormat>,

  /// Override the configured default model using a configured model alias
  #[arg(long, value_name = "ALIAS")]
  pub model: Option<String>,

  /// Open an offline UI preview without configuration, credentials, or network access
  #[arg(long, conflicts_with = "background_task_worker")]
  pub preview: bool,

  /// Path to configuration directory
  ///
  /// If not specified, defaults to ~/.zarncode/
  /// The directory should contain config.toml and optionally prompts/system.md
  ///
  /// Note: This specifies where to find the config file. The actual data directory
  /// (for logs, prompts, etc.) can be configured via the `dir` option in config.toml.
  #[arg(short = 'c', long, global = true, value_name = "DIR")]
  pub config: Option<PathBuf>,

  /// Start a new session instead of continuing the last one
  #[arg(long, conflicts_with_all = ["session", "continue"])]
  pub new_session: bool,

  /// Load a specific session by ID
  #[arg(long, value_name = "ID", conflicts_with = "continue")]
  pub session: Option<String>,

  /// Continue the most recent session
  #[arg(long)]
  pub r#continue: bool,

  /// Enable YOLO mode: auto-approve all tool calls without confirmation
  #[arg(long)]
  pub yolo: bool,

  /// Zarnkina relay API key (overrides config and auth file).
  /// Prefer the ZARNKINA_API_KEY environment variable: an argv key is visible to
  /// other local users through the process list.
  #[arg(long, value_name = "KEY")]
  pub api_key: Option<String>,

  /// Zarnkina relay account email (overrides config and auth file)
  #[arg(long, value_name = "EMAIL")]
  pub email: Option<String>,

  /// Zarnkina relay base URL override
  #[arg(long, value_name = "URL")]
  pub relay: Option<String>,

  /// Skip account verification and quota enforcement (owner/local testing)
  #[arg(long)]
  pub no_gate: bool,

  /// Path to an MCP server configuration JSON file
  ///
  /// The file should follow the standard MCP client format:
  /// { "mcpServers": { "name": { "command": "...", "args": [...] } } }
  #[arg(long, value_name = "PATH")]
  pub mcp_config_file: Option<PathBuf>,

  /// Internal: run as a background task worker process.
  #[arg(long, hide = true, value_name = "DIR")]
  pub background_task_worker: Option<PathBuf>,

  /// Internal: heartbeat interval for background task worker (ms).
  #[arg(long, hide = true)]
  pub worker_heartbeat_interval_ms: Option<u64>,

  /// Internal: control poll interval for background task worker (ms).
  #[arg(long, hide = true)]
  pub worker_control_poll_interval_ms: Option<u64>,

  /// Internal: kill grace period for background task worker (ms).
  #[arg(long, hide = true)]
  pub worker_kill_grace_period_ms: Option<u64>,
}

impl Args {
  /// Get the configuration directory path
  ///
  /// Returns the user-specified directory or the default location (~/.zarncode/)
  /// This is where the config.toml file is loaded from.
  pub fn config_dir(&self) -> PathBuf {
    self.config.clone().unwrap_or_else(|| {
      dirs::home_dir()
        .expect("Could not determine home directory")
        .join(".zarncode")
    })
  }

  fn resolved_agent_name(&self, config: &Config) -> Result<Option<String>> {
    if self.agent.is_some() {
      return Ok(self.agent.clone());
    }
    let store = crate::session::SessionStore::new(&crate::config::loader::data_dir(config));
    let id = if let Some(id) = &self.session {
      Some(id.clone())
    } else if self.r#continue {
      store.latest_id()?
    } else {
      None
    };
    match id {
      Some(id) => Ok(store.load(&id)?.0.agent_profile),
      None => Ok(None),
    }
  }
  /// Apply CLI overrides once, consistently for interactive and automation clients.
  pub fn effective_config(&self) -> Result<Config> {
    let config = load_config_from_dir(&self.config_dir())?;
    let mut config = AppConfig {
      user: config,
      overrides: CliOverrides {
        yolo: if self.yolo { Some(true) } else { None },
        mcp_config_file: self.mcp_config_file.clone(),
        session_id: self.session.clone(),
        r#continue: self.r#continue,
        api_key: self.api_key.clone(),
        email: self.email.clone(),
        relay: self.relay.clone(),
        no_gate: self.no_gate,
      },
    }
    .effective();
    let resolved_agent = self.resolved_agent_name(&config)?;
    let profile_model = if self.model.is_none() {
      resolved_agent
        .as_ref()
        .map(|name| {
          crate::subagents::LaborMarket::load_with_user(&self.config_dir()).and_then(|market| {
            market
              .require(name)
              .map(|agent| agent.default_model.clone())
          })
        })
        .transpose()
        .map_err(crate::cli::runtime::Error::LaborMarket)?
        .flatten()
    } else {
      None
    };
    if let Some(model) = self.model.as_ref().or(profile_model.as_ref()) {
      if config.get_model(model).is_none() {
        return Err(
          crate::config::Error::ModelNotFound {
            model: model.clone(),
          }
          .into(),
        );
      }
      config.default_model = model.clone();
    }
    Ok(config)
  }

  pub(crate) fn runtime_options(
    &self,
    config: &Config,
  ) -> Result<crate::cli::runtime::RuntimeOptions> {
    Ok(crate::cli::runtime::RuntimeOptions {
      max_concurrent_agents: self.max_concurrent_agents.map(|n| n as usize).unwrap_or(3),
      non_interactive: self.print,
      active_agent: self.resolved_agent_name(config)?,
      max_steps: self
        .max_steps
        .map(|steps| steps as usize)
        .unwrap_or(crate::llm::session::actor::DEFAULT_MAX_STEPS_PER_TURN),
    })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn default_directory_matches_zarncode_data_directory() {
    let args = Args::parse_from(["zarncode"]);
    assert_eq!(
      args.config_dir(),
      crate::config::loader::default_data_dir().unwrap()
    );
    assert!(args.config_dir().ends_with(".zarncode"));
  }

  #[test]
  fn explicit_legacy_configuration_is_preserved() {
    let args = Args::parse_from(["zarncode", "--config", ".ironcode"]);
    assert_eq!(args.config_dir(), PathBuf::from(".ironcode"));
  }
}
