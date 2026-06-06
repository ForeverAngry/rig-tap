//! In-process query helpers for captured observability events.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::event::ObservabilityEvent;

/// Predicate used by [`EventQuery`] to select observability events.
///
/// Every configured field must match. Tick bounds are inclusive.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventFilter {
    /// Conversation identifier to match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    /// Wire event kind to match, such as `"tool.completed"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Inclusive lower tick bound.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_tick: Option<u64>,
    /// Inclusive upper tick bound.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tick: Option<u64>,
    /// Compose kernel identifier to match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel_id: Option<String>,
    /// Tool name or retry target to match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// Tool call correlation identifier to match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// Compose skill identifier to match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill_id: Option<String>,
    /// Prompt model identifier to match.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

impl EventFilter {
    /// Build an empty filter that matches every event.
    pub fn new() -> Self {
        Self::default()
    }

    /// Match a conversation identifier.
    pub fn conversation_id(mut self, conversation_id: impl Into<String>) -> Self {
        self.conversation_id = Some(conversation_id.into());
        self
    }

    /// Match a wire event kind, such as `"prompt.started"`.
    pub fn kind(mut self, kind: impl Into<String>) -> Self {
        self.kind = Some(kind.into());
        self
    }

    /// Match events at or after `tick`.
    pub fn min_tick(mut self, tick: u64) -> Self {
        self.min_tick = Some(tick);
        self
    }

    /// Match events at or before `tick`.
    pub fn max_tick(mut self, tick: u64) -> Self {
        self.max_tick = Some(tick);
        self
    }

    /// Match a compose kernel identifier.
    pub fn kernel_id(mut self, kernel_id: impl Into<String>) -> Self {
        self.kernel_id = Some(kernel_id.into());
        self
    }

    /// Match a tool name or retry target.
    pub fn tool_name(mut self, tool_name: impl Into<String>) -> Self {
        self.tool_name = Some(tool_name.into());
        self
    }

    /// Match a tool call correlation identifier.
    pub fn call_id(mut self, call_id: impl Into<String>) -> Self {
        self.call_id = Some(call_id.into());
        self
    }

    /// Match a compose skill identifier.
    pub fn skill_id(mut self, skill_id: impl Into<String>) -> Self {
        self.skill_id = Some(skill_id.into());
        self
    }

    /// Match a prompt model identifier.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Return `true` if `event` satisfies every configured predicate.
    pub fn matches(&self, event: &ObservabilityEvent) -> bool {
        if self
            .conversation_id
            .as_ref()
            .is_some_and(|expected| expected != &event.conversation_id)
        {
            return false;
        }
        if self
            .kind
            .as_ref()
            .is_some_and(|expected| expected != event.kind.discriminant())
        {
            return false;
        }
        if self.min_tick.is_some_and(|min_tick| event.tick < min_tick) {
            return false;
        }
        if self.max_tick.is_some_and(|max_tick| event.tick > max_tick) {
            return false;
        }

        let fields = event.kind.scalar_fields();
        if self
            .kernel_id
            .as_ref()
            .is_some_and(|expected| expected != fields.kernel_id)
        {
            return false;
        }
        if self
            .tool_name
            .as_ref()
            .is_some_and(|expected| expected != fields.tool_name)
        {
            return false;
        }
        if self
            .call_id
            .as_ref()
            .is_some_and(|expected| expected != fields.call_id)
        {
            return false;
        }
        if self
            .skill_id
            .as_ref()
            .is_some_and(|expected| expected != fields.skill_id)
        {
            return false;
        }
        if self
            .model
            .as_ref()
            .is_some_and(|expected| expected != fields.model)
        {
            return false;
        }

        true
    }
}

/// Immutable query view over a snapshot of [`ObservabilityEvent`] values.
///
/// `EventQuery` is intentionally in-process and host-owned. It is useful for
/// tests, demos, and small local dashboards; production exporters should keep
/// using `tracing` sinks and external stores.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventQuery {
    events: Vec<ObservabilityEvent>,
}

impl EventQuery {
    /// Build a query view over `events` in their existing order.
    pub fn new(events: Vec<ObservabilityEvent>) -> Self {
        Self { events }
    }

    /// Return all events in snapshot order.
    pub fn all(&self) -> &[ObservabilityEvent] {
        &self.events
    }

    /// Return the number of events in this snapshot.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Return `true` when this snapshot has no events.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Return all events matching `filter` in snapshot order.
    pub fn filter(&self, filter: &EventFilter) -> Vec<ObservabilityEvent> {
        self.events
            .iter()
            .filter(|event| filter.matches(event))
            .cloned()
            .collect()
    }

    /// Return up to `limit` most recent events in ascending snapshot order.
    pub fn latest(&self, limit: usize) -> Vec<ObservabilityEvent> {
        let mut events = self
            .events
            .iter()
            .rev()
            .take(limit)
            .cloned()
            .collect::<Vec<_>>();
        events.reverse();
        events
    }

    /// Count events by wire event kind.
    pub fn count_by_kind(&self) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        for event in &self.events {
            let count = counts
                .entry(event.kind.discriminant().to_string())
                .or_insert(0);
            *count += 1;
        }
        counts
    }

    /// Return conversation identifiers present in this snapshot.
    pub fn conversations(&self) -> Vec<String> {
        self.events
            .iter()
            .map(|event| event.conversation_id.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }
}

impl From<Vec<ObservabilityEvent>> for EventQuery {
    fn from(events: Vec<ObservabilityEvent>) -> Self {
        Self::new(events)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::expect_used
)]
mod tests {
    use super::*;
    use crate::event::{EventKind, SCHEMA_VERSION};

    fn event(tick: u64, conversation_id: &str, kind: EventKind) -> ObservabilityEvent {
        ObservabilityEvent {
            version: SCHEMA_VERSION,
            occurred_at_millis: 1_715_000_000_000 + tick,
            tick,
            conversation_id: conversation_id.into(),
            span_id: None,
            agent_id: None,
            trace_id: None,
            severity: None,
            kind,
        }
    }

    #[test]
    fn filter_matches_conversation_kind_and_tick_window() {
        let query = EventQuery::new(vec![
            event(
                1,
                "a",
                EventKind::PromptStarted {
                    model: "m".into(),
                    messages_in: 1,
                    temperature: None,
                    top_p: None,
                    max_tokens: None,
                },
            ),
            event(
                2,
                "a",
                EventKind::ToolCompleted {
                    tool_name: "search".into(),
                    provider_call_id: None,
                    call_id: "call-1".into(),
                    result: "ok".into(),
                    truncated: false,
                    duration_ms: None,
                },
            ),
            event(
                3,
                "b",
                EventKind::ToolCompleted {
                    tool_name: "search".into(),
                    provider_call_id: None,
                    call_id: "call-2".into(),
                    result: "ok".into(),
                    truncated: false,
                    duration_ms: None,
                },
            ),
        ]);

        let matches = query.filter(
            &EventFilter::new()
                .conversation_id("a")
                .kind("tool.completed")
                .min_tick(2)
                .max_tick(3),
        );

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].tick, 2);
    }

    #[test]
    fn filter_matches_scalar_fields() {
        let query = EventQuery::new(vec![event(
            1,
            "thread",
            EventKind::ComposeSkillResolved {
                kernel_id: "kernel".into(),
                skill_id: "retrieval".into(),
                applies: true,
                delta: Some(0.2),
                confidence: Some(0.8),
            },
        )]);

        let matches = query.filter(&EventFilter::new().kernel_id("kernel").skill_id("retrieval"));

        assert_eq!(matches.len(), 1);
        assert!(
            query
                .filter(&EventFilter::new().tool_name("search"))
                .is_empty()
        );
    }

    #[test]
    fn query_summarizes_conversations_and_kinds() {
        let query = EventQuery::new(vec![
            event(
                1,
                "b",
                EventKind::PromptStarted {
                    model: "m".into(),
                    messages_in: 1,
                    temperature: None,
                    top_p: None,
                    max_tokens: None,
                },
            ),
            event(
                2,
                "a",
                EventKind::PromptStarted {
                    model: "m".into(),
                    messages_in: 2,
                    temperature: None,
                    top_p: None,
                    max_tokens: None,
                },
            ),
            event(
                3,
                "b",
                EventKind::ContextSampled {
                    message_count: 1,
                    byte_size: 2,
                    token_estimate: None,
                },
            ),
        ]);

        assert_eq!(query.conversations(), vec!["a", "b"]);
        assert_eq!(query.count_by_kind().get("prompt.started"), Some(&2));
        assert_eq!(
            query
                .latest(2)
                .iter()
                .map(|event| event.tick)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
    }
}
