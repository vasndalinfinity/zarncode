//! Request gate: account, model availability, Pro tier, and local quota.
//!
//! Sits between the usage tracker and the relay adapter, so a refused turn
//! never reaches the network and never spends quota.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use async_trait::async_trait;
use futures::Stream;

use crate::account::{AccountClient, quota::QuotaLedger, quota::Reservation};
use crate::error::{Error, LlmError, Result};
use crate::llm::provider::{LLMProvider, LlmResponseStream, LlmStreamEvent};
use crate::llm::types::Message;

/// Wraps a provider with the account and quota checks.
pub struct GatedProvider {
  inner: Box<dyn LLMProvider>,
  account: Arc<AccountClient>,
  quota: Arc<QuotaLedger>,
  model_id: String,
  enforce_account: bool,
  enforce_quota: bool,
}

impl GatedProvider {
  pub fn new(
    inner: Box<dyn LLMProvider>,
    account: Arc<AccountClient>,
    quota: Arc<QuotaLedger>,
    model_id: impl Into<String>,
    enforce_account: bool,
    enforce_quota: bool,
  ) -> Self {
    Self {
      inner,
      account,
      quota,
      model_id: model_id.into(),
      enforce_account,
      enforce_quota,
    }
  }
}

#[async_trait]
impl LLMProvider for GatedProvider {
  async fn chat_stream(&self, messages: Vec<Message>) -> Result<LlmResponseStream> {
    if self.enforce_account {
      let state = self.account.state().await;
      if let Some(reason) = state.denial_for(&self.model_id) {
        return Err(Error::Llm(LlmError::InvalidConfig(reason)));
      }
    }

    let reservation = if self.enforce_quota {
      Some(
        self
          .quota
          .reserve(&self.model_id)
          .map_err(|error| Error::Llm(LlmError::InvalidConfig(error.to_string())))?,
      )
    } else {
      None
    };

    // A failed request drops the reservation, which refunds the prompt.
    let inner = self.inner.chat_stream(messages).await?;
    Ok(Box::pin(GatedStream {
      inner,
      reservation,
      delivered: false,
    }))
  }

  fn name(&self) -> &str {
    self.inner.name()
  }

  fn max_context_size(&self) -> usize {
    self.inner.max_context_size()
  }

  async fn on_retryable_error(&mut self, error: &LlmError) {
    self.inner.on_retryable_error(error).await;
  }
}

/// Commits the reservation as soon as real content arrives; anything else
/// (empty stream, error before content) leaves the prompt refunded.
struct GatedStream {
  inner: LlmResponseStream,
  reservation: Option<Reservation>,
  delivered: bool,
}

impl Stream for GatedStream {
  type Item = Result<LlmStreamEvent>;

  fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
    let this = self.get_mut();
    let event = this.inner.as_mut().poll_next(cx);
    if let Poll::Ready(Some(Ok(item))) = &event
      && !this.delivered
      && matches!(
        item,
        LlmStreamEvent::Content(_)
          | LlmStreamEvent::Thinking(_)
          | LlmStreamEvent::ToolCallChunk { .. }
      )
    {
      this.delivered = true;
      if let Some(reservation) = this.reservation.as_mut() {
        reservation.commit();
      }
    }
    event
  }
}

#[cfg(test)]
#[cfg(test)]
mod tests {
  use super::*;
  use futures::StreamExt;

  struct Stub {
    events: Vec<LlmStreamEvent>,
  }

  #[async_trait]
  impl LLMProvider for Stub {
    async fn chat_stream(&self, _messages: Vec<Message>) -> Result<LlmResponseStream> {
      let events: Vec<Result<LlmStreamEvent>> = self.events.iter().cloned().map(Ok).collect();
      Ok(Box::pin(futures::stream::iter(events)))
    }
    fn name(&self) -> &str {
      "stub"
    }
    fn max_context_size(&self) -> usize {
      1024
    }
  }

  fn account_client() -> Arc<AccountClient> {
    // Unreachable relay: the account gate is disabled in these tests.
    AccountClient::new(
      "https://127.0.0.1:1",
      crate::account::Credentials::default(),
    )
  }

  #[tokio::test]
  async fn content_commits_the_reservation() {
    let quota = Arc::new(QuotaLedger::in_memory(true));
    let provider = GatedProvider::new(
      Box::new(Stub {
        events: vec![LlmStreamEvent::Content("hi".into())],
      }),
      account_client(),
      quota.clone(),
      "zarnkina-glm-5.3-flash",
      false,
      true,
    );
    let mut stream = provider.chat_stream(Vec::new()).await.unwrap();
    assert!(stream.next().await.is_some());
    drop(stream);
    assert!(quota.snapshot().used > 0);
  }

  #[tokio::test]
  async fn empty_stream_refunds_the_reservation() {
    let quota = Arc::new(QuotaLedger::in_memory(true));
    let provider = GatedProvider::new(
      Box::new(Stub { events: Vec::new() }),
      account_client(),
      quota.clone(),
      "zarnkina-glm-5.3-flash",
      false,
      true,
    );
    let stream = provider.chat_stream(Vec::new()).await.unwrap();
    assert_eq!(stream.count().await, 0);
    assert_eq!(quota.snapshot().used, 0);
  }
}
