//! Durable user input, distinct from provider context and tool work.
use crate::llm::types::{Message, Role};
use serde::{Deserialize, Serialize};

const MAX_ITEMS: usize = 64;
const MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
  Steer,
  FollowUp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedInput {
  pub id: String,
  pub kind: InputKind,
  pub content: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub delivery_index: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InputQueue {
  pub entries: Vec<QueuedInput>,
  pub paused: bool,
}

impl InputQueue {
  pub fn validate_new(&self, content: &str) -> Result<(), String> {
    if self.entries.len() >= MAX_ITEMS {
      return Err("Input queue is full (64 messages)".into());
    }
    self.validate_content(content, None)
  }
  pub fn enqueue(&mut self, id: String, kind: InputKind, content: String) -> Result<(), String> {
    if self.entries.len() >= MAX_ITEMS {
      return Err("Input queue is full (64 messages)".into());
    }
    if self.entries.iter().any(|e| e.id == id) {
      return Err("Duplicate queued input id".into());
    }
    self.validate_content(&content, None)?;
    self.entries.push(QueuedInput {
      id,
      kind,
      content,
      delivery_index: None,
    });
    Ok(())
  }
  fn validate_content(&self, content: &str, excluding: Option<&str>) -> Result<(), String> {
    if content.trim().is_empty() {
      return Err("Queued input cannot be empty".into());
    }
    let used: usize = self
      .entries
      .iter()
      .filter(|e| Some(e.id.as_str()) != excluding)
      .map(|e| e.content.len())
      .sum();
    if content.len() > MAX_BYTES.saturating_sub(used) {
      return Err("Queued input exceeds the 16 MiB queue limit".into());
    }
    Ok(())
  }

  pub(crate) fn resolve_id(&self, id: &str) -> Option<String> {
    if let Some(entry) = self.entries.iter().find(|e| e.id == id) {
      return Some(entry.id.clone());
    }
    if id.len() < 8 {
      return None;
    }
    let matches: Vec<_> = self
      .entries
      .iter()
      .filter(|e| e.id.starts_with(id))
      .collect();
    if matches.len() == 1 {
      Some(matches[0].id.clone())
    } else {
      None
    }
  }
  pub fn edit(&mut self, id: &str, content: String) -> Result<(), String> {
    let id = self
      .resolve_id(id)
      .ok_or("Queued input id not found or ambiguous")?;
    self.validate_content(&content, Some(&id))?;
    let entry = self
      .entries
      .iter_mut()
      .find(|e| e.id == id)
      .ok_or("Queued input id not found")?;
    if entry.delivery_index.is_some() {
      return Err(
        "This input is already prepared; resume its delivery or recover the session before editing"
          .into(),
      );
    }
    entry.content = content;
    entry.delivery_index = None;
    Ok(())
  }
  pub fn remove(&mut self, id: &str) -> Option<QueuedInput> {
    let id = self.resolve_id(id)?;
    let index = self.entries.iter().position(|e| e.id == id)?;
    Some(self.entries.remove(index))
  }
  pub fn next(&self, steering_only: bool) -> Option<QueuedInput> {
    if self.paused {
      return None;
    }
    if let Some(prepared) = self.entries.iter().find(|e| e.delivery_index.is_some()) {
      return if !steering_only || prepared.kind == InputKind::Steer {
        Some(prepared.clone())
      } else {
        None
      };
    }
    self
      .entries
      .iter()
      .find(|e| e.kind == InputKind::Steer)
      .or_else(|| {
        if steering_only {
          None
        } else {
          self.entries.first()
        }
      })
      .cloned()
  }
  /// Recover a prepared delivery without appending the same user message twice.
  /// Resumed pending work stays paused until the user explicitly resumes it.
  pub fn recover(&mut self, messages: &[Message]) {
    self.entries.retain_mut(|entry| {
      let delivered = entry
        .delivery_index
        .and_then(|i| messages.iter().filter(|m| m.role == Role::User).nth(i))
        .is_some_and(|m| m.role == Role::User && m.content == entry.content);
      entry.delivery_index = None;
      !delivered
    });
    self.paused = !self.entries.is_empty();
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  #[test]
  fn steering_has_priority_and_each_kind_remains_fifo() {
    let mut q = InputQueue::default();
    for (id, kind) in [
      ("f1", InputKind::FollowUp),
      ("s1", InputKind::Steer),
      ("f2", InputKind::FollowUp),
      ("s2", InputKind::Steer),
    ] {
      q.enqueue(id.into(), kind, id.into()).unwrap();
    }
    for id in ["s1", "s2", "f1", "f2"] {
      assert_eq!(q.next(false).unwrap().id, id);
      q.remove(id).unwrap();
    }
  }
  #[test]
  fn edits_and_pause_never_drop_unrelated_input() {
    let mut q = InputQueue::default();
    q.enqueue("a".into(), InputKind::Steer, "first".into())
      .unwrap();
    q.enqueue("b".into(), InputKind::FollowUp, "later".into())
      .unwrap();
    q.edit("a", "edited".into()).unwrap();
    q.paused = true;
    assert!(q.next(false).is_none());
    assert_eq!(q.remove("a").unwrap().content, "edited");
    assert_eq!(q.entries[0].content, "later");
  }
  #[test]
  fn restart_recovers_partial_delivery_without_replaying_it() {
    let mut q = InputQueue::default();
    q.enqueue("a".into(), InputKind::Steer, "delivered".into())
      .unwrap();
    q.enqueue("b".into(), InputKind::FollowUp, "pending".into())
      .unwrap();
    q.entries[0].delivery_index = Some(0);
    let mut restored: InputQueue =
      serde_json::from_str(&serde_json::to_string(&q).unwrap()).unwrap();
    restored.recover(&[Message::user("delivered")]);
    assert_eq!(restored.entries.len(), 1);
    assert_eq!(restored.entries[0].id, "b");
    assert!(restored.paused);
  }
  #[test]
  fn rejected_edits_leave_original_content_intact() {
    let mut q = InputQueue::default();
    q.enqueue("a".into(), InputKind::Steer, "keep".into())
      .unwrap();
    assert!(q.edit("a", " ".into()).is_err());
    assert!(
      q.enqueue("a".into(), InputKind::FollowUp, "duplicate".into())
        .is_err()
    );
    assert_eq!(q.entries[0].content, "keep");
  }

  #[test]
  fn recovery_counts_user_ordinals_not_optional_system_messages() {
    let mut queue = InputQueue::default();
    queue
      .enqueue("a".into(), InputKind::Steer, "delivered".into())
      .unwrap();
    queue.entries[0].delivery_index = Some(0);
    queue.recover(&[
      Message::system("not persisted by every session writer"),
      Message::user("delivered"),
    ]);
    assert!(queue.entries.is_empty());
  }
  #[test]
  fn a_prepared_delivery_cannot_be_changed_into_a_different_message() {
    let mut queue = InputQueue::default();
    queue
      .enqueue("abcdefgh-id".into(), InputKind::Steer, "original".into())
      .unwrap();
    queue.entries[0].delivery_index = Some(0);
    assert!(queue.edit("abcdefgh", "different".into()).is_err());
    assert_eq!(queue.entries[0].content, "original");
  }
}
