//! End-to-end: wire the public `CapturingLayer` (`subscriber` feature)
//! to capture `rig_tap` events, exercise [`ObservedMemory`] over an
//! in-memory `ConversationMemory`, and assert the captured envelope
//! round-trips through the v1 schema.

#![cfg(feature = "subscriber")]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::expect_used
)]

use rig::memory::{ConversationMemory, InMemoryConversationMemory};
use rig_tap::{CapturingLayer, EventFilter, EventKind, ObservedMemory, SCHEMA_VERSION, emit_kind};
use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[tokio::test]
async fn observed_memory_emits_context_sampled() {
    let capture = CapturingLayer::new();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    let _guard = subscriber.set_default();

    let memory = ObservedMemory::new(InMemoryConversationMemory::new());

    // Empty load.
    let loaded = memory.load("thread-x").await.unwrap();
    assert!(loaded.is_empty());

    let events = capture.snapshot();
    assert_eq!(events.len(), 1);
    let evt = &events[0];
    assert_eq!(evt.version, SCHEMA_VERSION);
    assert_eq!(evt.conversation_id, "thread-x");
    match &evt.kind {
        EventKind::ContextSampled {
            message_count,
            byte_size,
            token_estimate,
        } => {
            assert_eq!(*message_count, 0);
            // empty Vec serializes to "[]" = 2 bytes
            assert!(*byte_size >= 2);
            assert!(token_estimate.is_none());
        }
        other => panic!("expected ContextSampled, got {other:?}"),
    }
}

#[tokio::test]
async fn emit_kind_writes_full_envelope() {
    let capture = CapturingLayer::new();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    let _guard = subscriber.set_default();

    emit_kind(
        "conv-1",
        EventKind::ContextCompacted {
            evicted_count: 8,
            evicted_bytes: 4096,
            carry_over: true,
            summary_bytes: 512,
        },
    );

    let events = capture.snapshot();
    assert_eq!(events.len(), 1);
    let evt = &events[0];
    assert_eq!(evt.conversation_id, "conv-1");
    assert!(evt.occurred_at_millis > 0);
    match &evt.kind {
        EventKind::ContextCompacted {
            evicted_count,
            evicted_bytes,
            carry_over,
            summary_bytes,
        } => {
            assert_eq!(*evicted_count, 8);
            assert_eq!(*evicted_bytes, 4096);
            assert!(*carry_over);
            assert_eq!(*summary_bytes, 512);
        }
        other => panic!("expected ContextCompacted, got {other:?}"),
    }
}

#[tokio::test]
async fn emit_kind_writes_otel_routable_scalar_fields() {
    let fields = FieldCaptureLayer::default();
    let subscriber = tracing_subscriber::registry().with(fields.clone());
    let _guard = subscriber.set_default();

    emit_kind(
        "conv-otel",
        EventKind::PromptStarted {
            model: "model-a".into(),
            messages_in: 2,
        },
    );

    let snapshots = fields.snapshot();
    assert_eq!(snapshots.len(), 1);
    let snapshot = &snapshots[0];
    assert_eq!(snapshot.target, "rig_tap");
    assert_eq!(snapshot.kind.as_deref(), Some("prompt.started"));
    assert_eq!(snapshot.conversation_id.as_deref(), Some("conv-otel"));
    assert_eq!(snapshot.version, Some(SCHEMA_VERSION.into()));
    assert!(snapshot.tick.is_some());
    assert!(snapshot.occurred_at_millis.is_some());
    assert!(
        snapshot
            .event_json
            .as_deref()
            .unwrap()
            .contains("prompt.started")
    );
}

#[tokio::test]
async fn ticks_are_monotonic_across_events() {
    let capture = CapturingLayer::new();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    let _guard = subscriber.set_default();

    for i in 0..5 {
        emit_kind(
            "conv",
            EventKind::MemoryDemoted {
                demoted_count: i,
                tags: vec![],
            },
        );
    }

    let events = capture.snapshot();
    assert_eq!(events.len(), 5);
    for window in events.windows(2) {
        assert!(
            window[1].tick > window[0].tick,
            "ticks must be monotonic; got {:?} then {:?}",
            window[0].tick,
            window[1].tick,
        );
    }
}

#[tokio::test]
async fn capturing_layer_exposes_query_snapshot() {
    let capture = CapturingLayer::new();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    let _guard = subscriber.set_default();

    emit_kind(
        "conv-a",
        EventKind::ToolInvoked {
            tool_name: "search".into(),
            provider_call_id: None,
            call_id: "call-a".into(),
            args_json: "{}".into(),
            truncated: false,
        },
    );
    emit_kind(
        "conv-b",
        EventKind::PromptStarted {
            model: "model-b".into(),
            messages_in: 1,
        },
    );

    let query = capture.query();
    let tool_events = query.filter(
        &EventFilter::new()
            .conversation_id("conv-a")
            .kind("tool.invoked")
            .tool_name("search")
            .call_id("call-a"),
    );

    assert_eq!(query.len(), 2);
    assert_eq!(tool_events.len(), 1);
    assert_eq!(query.count_by_kind().get("tool.invoked"), Some(&1));
    assert_eq!(query.conversations(), vec!["conv-a", "conv-b"]);
}

#[derive(Clone, Default)]
struct FieldCaptureLayer {
    snapshots: Arc<Mutex<Vec<FieldSnapshot>>>,
}

impl FieldCaptureLayer {
    fn snapshot(&self) -> Vec<FieldSnapshot> {
        self.snapshots.lock().unwrap().clone()
    }
}

impl<S> Layer<S> for FieldCaptureLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut snapshot = FieldSnapshot {
            target: event.metadata().target().to_string(),
            ..FieldSnapshot::default()
        };
        event.record(&mut snapshot);
        self.snapshots.lock().unwrap().push(snapshot);
    }
}

#[derive(Clone, Debug, Default)]
struct FieldSnapshot {
    target: String,
    event_json: Option<String>,
    version: Option<u64>,
    kind: Option<String>,
    conversation_id: Option<String>,
    tick: Option<u64>,
    occurred_at_millis: Option<u64>,
}

impl Visit for FieldSnapshot {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "event" => self.event_json = Some(value.to_string()),
            "rig_tap.kind" => self.kind = Some(value.to_string()),
            "rig_tap.conversation_id" => self.conversation_id = Some(value.to_string()),
            _ => {}
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        match field.name() {
            "rig_tap.version" => self.version = Some(value),
            "rig_tap.tick" => self.tick = Some(value),
            "rig_tap.occurred_at_millis" => self.occurred_at_millis = Some(value),
            _ => {}
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let rendered = format!("{value:?}");
        let rendered = rendered.trim_matches('"');
        match field.name() {
            "event" if self.event_json.is_none() => self.event_json = Some(rendered.to_string()),
            "rig_tap.version" if self.version.is_none() => self.version = rendered.parse().ok(),
            "rig_tap.kind" if self.kind.is_none() => self.kind = Some(rendered.to_string()),
            "rig_tap.conversation_id" if self.conversation_id.is_none() => {
                self.conversation_id = Some(rendered.to_string());
            }
            "rig_tap.tick" if self.tick.is_none() => self.tick = rendered.parse().ok(),
            "rig_tap.occurred_at_millis" if self.occurred_at_millis.is_none() => {
                self.occurred_at_millis = rendered.parse().ok();
            }
            _ => {}
        }
    }
}
