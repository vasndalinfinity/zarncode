//! Account and quota commands; never starts a session or a model turn.

use anyhow::{Context, Result, bail};

use super::args::{Args, AuthCommand, CliCommand};
use crate::account::{
  self, AccountClient, AuthFile, Credentials,
  quota::{QuotaLedger, QuotaSnapshot},
};
use crate::config::Config;
use crate::config::loader::data_dir;

/// Entry point for `auth`, `quota`, and `models`.
pub async fn run(args: &Args, command: &CliCommand) -> Result<()> {
  if args.print || args.preview || args.session.is_some() || args.r#continue {
    bail!("Account commands cannot be combined with execution/session flags");
  }
  match command {
    CliCommand::Auth { command } => auth(args, command).await,
    CliCommand::Quota { reset } => quota(args, *reset),
    CliCommand::Models => models(args).await,
    CliCommand::Agent { .. } => unreachable!("agent commands are routed separately"),
  }
}

fn config_for(args: &Args) -> Config {
  args.effective_config().unwrap_or_default()
}

fn quota_ledger(args: &Args) -> (QuotaLedger, String) {
  let config = config_for(args);
  let credentials = account::resolve_credentials(&config, &args.config_dir());
  let config_dir = args.config_dir();
  let data_dir = data_dir(&config);
  let label = credentials.quota_label();
  let ledger = QuotaLedger::new(&data_dir, label.clone(), config.quota.enforce);
  let _ = config_dir;
  (ledger, label)
}

async fn auth(args: &Args, command: &AuthCommand) -> Result<()> {
  let config_dir = args.config_dir();
  let auth_path = account::cli_auth_path(&config_dir);

  match command {
    AuthCommand::Login { api_key, relay } => {
      let config = config_for(args);
      let base = relay
        .clone()
        .or_else(|| args.relay.clone())
        .unwrap_or_else(|| account::relay_base(&config));
      account::validate_relay_url(&base).map_err(|e| anyhow::anyhow!(e))?;

      let file = if let Some(key) = api_key.as_ref() {
        let state = account::verify_api_key(&base, key)
          .await
          .map_err(|e| anyhow::anyhow!(e))?;
        println!(
          "API key valid Ãƒâ€šÃ‚Â· {} Ãƒâ€šÃ‚Â· {} model(s)",
          state.email,
          state.models.len()
        );
        AuthFile {
          connected: true,
          email: Some(state.email.clone()),
          api_key: Some(key.clone()),
          session_token: None,
          saved_at: Some(chrono::Local::now().to_rfc3339()),
        }
      } else {
        println!("Signing in through Google. The browser window will hand the code to the CLI.");
        account::google_login(&base)
          .await
          .map_err(|e| anyhow::anyhow!(e))?
      };

      account::write_auth_file(&auth_path, &file).map_err(|e| anyhow::anyhow!(e))?;
      println!(
        "signed in as {} Ãƒâ€šÃ‚Â· session saved to {}",
        file.email.as_deref().unwrap_or("unknown"),
        auth_path.display()
      );
      Ok(())
    }
    AuthCommand::Status => {
      let config = config_for(args);
      let credentials = account::resolve_credentials(&config, &config_dir);
      if credentials.email.is_none()
        && credentials.api_key.is_none()
        && credentials.session_token.is_none()
      {
        println!(
          "Not signed in. Run `zarncode auth login` or `zarncode auth login --api-key zk_live_...`."
        );
        return Ok(());
      }
      let base = account::relay_base(&config);
      let client = AccountClient::new(base.clone(), credentials.clone());
      let state = client.state().await;
      println!("relay        {base}");
      println!(
        "email        {}",
        if state.email.is_empty() {
          "-"
        } else {
          &state.email
        }
      );
      println!("allowed      {}", state.allowed);
      println!("tier         {}", if state.pro { "pro" } else { "free" });
      println!("status       {}", state.status);
      if let Some(reason) = state.reason.as_ref().or(state.message.as_ref()) {
        println!("reason       {reason}");
      }
      println!("models       {}", state.models.len());
      println!(
        "credential   {}",
        if credentials.api_key.is_some() {
          "api key"
        } else if credentials.session_token.is_some() {
          "session token"
        } else {
          "email only (chat will be refused)"
        }
      );
      let (ledger, _) = quota_ledger(args);
      print!("{}", ledger.snapshot().report());
      Ok(())
    }
    AuthCommand::Logout => {
      account::remove_auth_file(&auth_path).map_err(|e| anyhow::anyhow!(e))?;
      println!("signed out ({})", auth_path.display());
      Ok(())
    }
    AuthCommand::Request => {
      let config = config_for(args);
      let base = account::relay_base(&config);
      println!(
        "Zarnkina is invite-only. Ask an administrator to whitelist your email, then run:\n  zarncode auth login\n\nRelay: {base}\nWeb:   https://zarnkina-web.zarnkina.deno.net"
      );
      Ok(())
    }
  }
}

fn quota(args: &Args, reset: bool) -> Result<()> {
  let (ledger, label) = quota_ledger(args);
  if reset {
    ledger
      .repair()
      .context("could not remove the quota ledger")?;
    ledger
      .reset()
      .map_err(|e| anyhow::anyhow!(e))
      .context("could not reset the quota ledger")?;
    println!("quota ledger rebuilt for {label}");
  }
  let snapshot: QuotaSnapshot = ledger.snapshot();
  print!("{}", snapshot.report());
  Ok(())
}

async fn models(args: &Args) -> Result<()> {
  let config = config_for(args);
  let credentials: Credentials = account::resolve_credentials(&config, &args.config_dir());
  let base = account::relay_base(&config);
  let client = AccountClient::new(base, credentials);
  let catalog = client.catalog().await;
  if catalog.is_empty() {
    bail!("relay returned no models");
  }
  let default_model = config.default_model.clone();
  println!(
    "{:<32} {:<6} {:<8} {:<10} alias",
    "model", "tier", "status", "usage"
  );
  for model in catalog {
    let alias = config
      .models
      .iter()
      .find(|(_, entry)| entry.model == model.id)
      .map(|(name, _)| name.clone())
      .unwrap_or_else(|| "-".to_string());
    println!(
      "{:<32} {:<6} {:<8} {:<10} {}{}",
      model.id,
      if model.is_pro() { "pro" } else { "free" },
      if model.is_active() {
        "active"
      } else {
        model.status.as_str()
      },
      model.usage_tier.unwrap_or_else(|| "-".into()),
      alias,
      if alias == default_model {
        "  <- default"
      } else {
        ""
      }
    );
  }
  Ok(())
}
