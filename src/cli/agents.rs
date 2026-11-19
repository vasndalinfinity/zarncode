//! Offline agent configuration management; never starts inference or tools.
use super::args::{AgentCommand, Args};
use crate::subagents::{
  LaborMarket,
  definitions::{AgentFile, AgentMode},
};
use anyhow::{Result, bail};
use std::fs::{self, OpenOptions};
use std::io::Write;

pub fn run(args: &Args, command: &AgentCommand) -> Result<()> {
  if args.print || args.preview || args.session.is_some() || args.r#continue {
    bail!("Agent management cannot be combined with execution/session flags");
  }
  let directory = args.config_dir();
  let market = LaborMarket::load_with_user(&directory)?;
  match command {
    AgentCommand::List => {
      let mut agents: Vec<_> = market.iter().collect();
      agents.sort_by(|a, b| a.name.cmp(&b.name));
      for agent in agents {
        println!(
          "{}\t{:?}\tsteps={}",
          agent.name,
          agent.mode,
          agent
            .max_steps
            .map(|n| n.to_string())
            .unwrap_or_else(|| "inherited".into())
        );
      }
    }
    AgentCommand::Validate => println!(
      "{} agent definitions valid (schema). Model aliases and handler availability are checked when a runtime starts.",
      market.iter().count()
    ),
    AgentCommand::Create {
      name,
      description,
      prompt,
      model,
      tools,
      max_steps,
      mode,
    } => {
      if market.get(name).is_some() {
        bail!("Agent '{name}' already exists or is reserved");
      }
      let definition = AgentFile {
        schema_version: 1,
        name: name.clone(),
        description: description.clone(),
        prompt: prompt.clone(),
        model: model.clone(),
        tools: if tools.is_empty() {
          vec!["ReadFile".into(), "Glob".into(), "Grep".into()]
        } else {
          tools.clone()
        },
        max_steps: max_steps.map(|n| n as usize).unwrap_or(5),
        supports_background: false,
        mode: match mode.as_str() {
          "primary" => AgentMode::Primary,
          "subagent" => AgentMode::Subagent,
          _ => AgentMode::Both,
        },
      };
      definition.validate()?;
      fs::create_dir_all(directory.join("agents"))?;
      let path = directory.join("agents").join(format!("{name}.yaml"));
      let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
      file.write_all(serde_yaml::to_string(&definition)?.as_bytes())?;
      file.sync_all()?;
      println!("Created {}", path.display());
    }
  }
  Ok(())
}
