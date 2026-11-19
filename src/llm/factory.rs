//! Provider construction in one place.
//!
//! `SessionActor` used to grow one match arm per provider. Everything about how
//! a provider is built (credentials, decorators, context size) now lives here so
//! the actor only asks for "the provider for this model".

use crate::cli::runtime::Runtime;
use crate::config::Error as ConfigError;
use crate::error::Result;
use crate::llm::gate::GatedProvider;
use crate::llm::provider::LLMProvider;
use crate::llm::providers::{ChatCompatProvider, ZarnkinaProvider};
use crate::llm::types::ChatConfig;
use crate::usage::TrackedProvider;

/// Build the provider for `model_alias`, or for the configured default model.
pub(crate) fn build_provider(
  runtime: &Runtime,
  model_alias: Option<&str>,
) -> Result<Box<dyn LLMProvider>> {
  let tool_registry = runtime.tool_registry.clone();

  let (model_alias, model_config) = match model_alias {
    Some(alias) => {
      // The relay catalogue is authoritative: a model the operator published
      // after the config file was written is usable even without a local entry.
      let resolved = runtime
        .config
        .get_model(alias)
        .cloned()
        .or_else(|| runtime.live_model_config(alias));
      (
        alias.to_string(),
        resolved.ok_or(ConfigError::ModelNotFound {
          model: alias.to_string(),
        })?,
      )
    }
    None => (
      runtime.default_model(),
      runtime
        .default_model_config()
        .cloned()
        .ok_or(ConfigError::MissingDefaultModel)?,
    ),
  };

  let provider = runtime
    .get_provider(&model_config.provider)
    .ok_or_else(|| ConfigError::ProviderNotFound {
      provider: model_config.provider.clone(),
      model: model_alias.clone(),
    })?;

  let api_key = provider
    .api_key
    .as_ref()
    .map(|key| runtime.resolve_api_key(key))
    .unwrap_or_default();

  let mut chat_config = ChatConfig::new(&model_config.model);
  if let Some(max_tokens) = model_config.max_tokens {
    chat_config = chat_config.with_max_tokens(max_tokens);
  }
  if let Some(temperature) = model_config.temperature {
    chat_config = chat_config.with_temperature(temperature);
  }
  chat_config = chat_config.with_thinking(runtime.default_thinking());

  let max_context_size = model_config
    .max_context_size
    .unwrap_or(crate::config::DEFAULT_MAX_CONTEXT_SIZE);

  let provider_type = provider.provider_type.as_str();
  let inner: Box<dyn LLMProvider> = match provider_type {
    "zarnkina" => {
      let mut credentials = runtime.credentials.clone();
      if credentials.api_key.is_none() && !api_key.trim().is_empty() {
        credentials.api_key = Some(api_key);
      }
      Box::new(ZarnkinaProvider::new(
        &provider.base_url,
        chat_config,
        credentials,
        max_context_size,
        tool_registry,
        runtime.turn_id.clone(),
      )?)
    }
    // Anything else that speaks the OpenAI chat-completions wire, including a
    // user's own endpoint: the plain provider, no relay headers, no session.
    "openai-compatible" => Box::new(ChatCompatProvider::new(
      &provider.base_url,
      api_key,
      chat_config,
      false,
      max_context_size,
      tool_registry,
    )?),
    other => {
      return Err(
        ConfigError::ProviderNotFound {
          provider: other.to_string(),
          model: model_alias,
        }
        .into(),
      );
    }
  };

  // Account and quota gates belong to the relay. A user's own endpoint
  // ("openai-compatible") is their bill, so it is never gated here.
  let gated: Box<dyn LLMProvider> = if provider_type == "zarnkina" {
    let account = runtime.account_for(&provider.base_url);
    let enforce_account = runtime.config.quota.verify_account;
    let enforce_quota = runtime.config.quota.enforce && runtime.quota.enforced();
    if enforce_account || enforce_quota {
      Box::new(GatedProvider::new(
        inner,
        account,
        runtime.quota.clone(),
        model_config.model.clone(),
        enforce_account,
        enforce_quota,
      ))
    } else {
      inner
    }
  } else {
    inner
  };

  Ok(Box::new(TrackedProvider::new(
    gated,
    runtime.usage.clone(),
    model_config.model.clone(),
  )))
}

#[allow(dead_code)]
fn _assert_error_type(_: crate::error::LlmError) {}
