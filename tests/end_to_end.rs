//! End-to-end: wire the public `CapturingLayer` (`subscriber` feature)
//! to capture `rig_observe` events, exercise [`ObservedMemory`] over an
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
use rig_observe::{CapturingLayer, EventKind, ObservedMemory, SCHEMA_VERSION, emit_kind};
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
