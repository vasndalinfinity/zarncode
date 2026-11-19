//! User-owned, versioned definitions. Project directories are not auto-trusted.
use std::fs;
use std::path::{Path, PathBuf};

use super::{AgentTypeDefinition, LaborMarket, ToolPolicy, ToolPolicyMode};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

fn default_tools() -> Vec<String> {
  vec!["ReadFile".into(), "Glob".into(), "Grep".into()]
}
fn default_steps() -> usize {
  5
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMode {
  Primary,
  Subagent,
  #[default]
  Both,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentFile {
  #[serde(default)]
  pub mode: AgentMode,
  pub schema_version: u32,
  pub name: String,
  pub description: String,
  pub prompt: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub model: Option<String>,
  #[serde(default = "default_tools")]
  pub tools: Vec<String>,
  #[serde(default = "default_steps")]
  pub max_steps: usize,
  #[serde(default)]
  pub supports_background: bool,
}

impl AgentFile {
  pub fn validate(&self) -> Result<()> {
    if self.schema_version != 1 {
      bail!("Unsupported agent schema_version {}", self.schema_version);
    }
    if self.name.is_empty()
      || self.name.len() > 48
      || !self
        .name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
      bail!("Agent name must be 1..48 ASCII letters, numbers, '-' or '_'");
    }
    let upper = self.name.to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
      || (upper.len() == 4
        && (upper.starts_with("COM") || upper.starts_with("LPT"))
        && upper.as_bytes()[3].is_ascii_digit())
    {
      bail!("Agent name is reserved on Windows");
    }
    if self.description.trim().is_empty() || self.description.len() > 1024 {
      bail!("Agent description must be nonempty and at most 1024 bytes");
    }
    if self.prompt.trim().is_empty() || self.prompt.len() > 65536 {
      bail!("Agent prompt must be nonempty and at most 64 KiB");
    }
    if !(1..=10000).contains(&self.max_steps) {
      bail!("Agent max_steps must be between 1 and 10000");
    }
    if self.tools.len() > 64 {
      bail!("An agent may allow at most 64 tools");
    }
    let mut seen = std::collections::HashSet::new();
    for tool in &self.tools {
      if tool.is_empty()
        || !tool.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        || !seen.insert(tool)
      {
        bail!("Invalid or duplicate agent tool: {tool}");
      }
      if self.mode != AgentMode::Primary && matches!(tool.as_str(), "Agent" | "Task") {
        bail!("Nested delegation is not supported; child agents cannot allow {tool}");
      }
    }
    if self.model.as_ref().is_some_and(|m| m.trim().is_empty()) {
      bail!("Agent model alias cannot be empty");
    }
    Ok(())
  }

  pub fn load(path: &Path) -> Result<Self> {
    if fs::metadata(path)?.len() > 131072 {
      bail!("Agent file exceeds 128 KiB: {}", path.display());
    }
    let spec: Self = serde_yaml::from_str(&fs::read_to_string(path)?)
      .with_context(|| format!("Invalid agent definition {}", path.display()))?;
    spec.validate()?;
    if path.file_stem().and_then(|s| s.to_str()) != Some(&spec.name) {
      bail!("Agent filename must match name '{}'", spec.name);
    }
    Ok(spec)
  }

  fn definition(self, path: PathBuf) -> AgentTypeDefinition {
    AgentTypeDefinition {
      mode: self.mode,
      name: self.name,
      description: self.description.clone(),
      agent_file: path,
      when_to_use: self.description,
      role_additional: self.prompt,
      default_model: self.model,
      tool_policy: ToolPolicy {
        mode: ToolPolicyMode::Allowlist,
        tools: self.tools,
      },
      supports_background: self.supports_background,
      max_steps: Some(self.max_steps),
    }
  }
}

impl LaborMarket {
  pub fn load_with_user(config_dir: &Path) -> Result<Self> {
    let mut market = Self::load_builtin()?;
    let directory = config_dir.join("agents");
    if !directory.exists() {
      return Ok(market);
    }
    let mut paths: Vec<_> = fs::read_dir(&directory)?
      .map(|e| e.map(|e| e.path()))
      .collect::<std::io::Result<_>>()?;
    paths.sort();
    for path in paths {
      if !path.is_file()
        || !matches!(
          path.extension().and_then(|x| x.to_str()),
          Some("yaml" | "yml")
        )
      {
        continue;
      }
      let spec = AgentFile::load(&path)?;
      if market.get(&spec.name).is_some() {
        bail!("Duplicate or reserved agent name '{}'", spec.name);
      }
      market.add_builtin_type(spec.definition(path));
    }
    Ok(market)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  pub fn sample() -> AgentFile {
    AgentFile {
      mode: AgentMode::Both,
      schema_version: 1,
      name: "reviewer".into(),
      description: "Review code".into(),
      prompt: "Review correctness and security.".into(),
      model: None,
      tools: default_tools(),
      max_steps: 3,
      supports_background: false,
    }
  }
  #[test]
  fn custom_definitions_load_and_preserve_their_policy_and_budget() {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("agents")).unwrap();
    fs::write(
      directory.path().join("agents/reviewer.yaml"),
      serde_yaml::to_string(&sample()).unwrap(),
    )
    .unwrap();
    let market = LaborMarket::load_with_user(directory.path()).unwrap();
    let agent = market.require("reviewer").unwrap();
    assert_eq!(agent.max_steps, Some(3));
    assert!(!agent.tool_policy.allows("WriteFile"));
    assert!(!agent.supports_background);
  }
  #[test]
  fn invalid_names_nested_delegation_and_unknown_fields_are_rejected() {
    let mut spec = sample();
    spec.name = "../escape".into();
    assert!(spec.validate().is_err());
    spec = sample();
    spec.tools.push("Agent".into());
    assert!(spec.validate().is_err());
    let yaml = format!(
      "{}\nunsafe_extra: true\n",
      serde_yaml::to_string(&sample()).unwrap()
    );
    assert!(serde_yaml::from_str::<AgentFile>(&yaml).is_err());
  }
  #[test]
  fn builtins_cannot_be_silently_overridden() {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("agents")).unwrap();
    let mut spec = sample();
    spec.name = "coder".into();
    fs::write(
      directory.path().join("agents/coder.yaml"),
      serde_yaml::to_string(&spec).unwrap(),
    )
    .unwrap();
    assert!(LaborMarket::load_with_user(directory.path()).is_err());
  }

  #[test]
  fn reserved_device_names_and_invalid_step_caps_are_rejected() {
    let mut spec = sample();
    spec.name = "CON".into();
    assert!(spec.validate().is_err());
    spec = sample();
    spec.max_steps = 0;
    assert!(spec.validate().is_err());
    spec.max_steps = 10001;
    assert!(spec.validate().is_err());
  }
}
