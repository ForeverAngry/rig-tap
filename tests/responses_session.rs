//! Fixture-driven integration test for the OpenAI Responses
//! observer surface. Exercises the public
//! [`ResponsesSessionObserver`] state machine through the
//! `CapturingLayer` and asserts the resulting `rig_tap` envelopes
//! round-trip through the v1 schema with the expected scalars.

#![cfg(all(
    feature = "openai-responses-websocket",
    feature = "subscriber",
    not(target_family = "wasm")
))]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::expect_used
)]

use rig::providers::openai::responses_api::streaming::ResponseChunk;
use rig::providers::openai::responses_api::websocket::{
    ResponsesWebSocketDoneEvent, ResponsesWebSocketEvent,
};
use rig_tap::{CapturingLayer, EventKind, ResponsesSessionObserver, SCHEMA_VERSION};
use serde_json::{Value, json};
use tracing_subscriber::layer::SubscriberExt;

fn capture<F: FnOnce()>(f: F) -> Vec<rig_tap::ObservabilityEvent> {
    let layer = CapturingLayer::new();
    let probe = layer.clone();
    tracing::subscriber::with_default(tracing_subscriber::registry().with(layer), f);
    probe.snapshot()
}

fn response_chunk(kind_label: &str, response: Value) -> ResponseChunk {
    serde_json::from_value(json!({
        "type": kind_label,
        "response": response,
        "sequence_number": 0,
    }))
    .unwrap()
}

fn done_event(response: Value) -> ResponsesWebSocketDoneEvent {
    serde_json::from_value(json!({
        "type": "response.done",
        "response": response,
    }))
    .unwrap()
}

#[test]
fn multi_turn_session_envelope_is_well_formed() {
    let events = capture(|| {
        let mut observer = ResponsesSessionObserver::new("conv-multi", "gpt-5", "sess-multi");

        // Turn 1: no previous response, one hosted tool call (web_search).
        observer.observe_send(None);
        observer.observe_event(&ResponsesWebSocketEvent::Response(Box::new(
            response_chunk(
                "response.completed",
                json!({
                    "id": "resp_1",
                    "object": "response",
                    "created_at": 0,
                    "status": "completed",
                    "model": "gpt-5",
                    "usage": {
                        "input_tokens": 10,
                        "output_tokens": 20,
                        "total_tokens": 30,
                        "output_tokens_details": { "reasoning_tokens": 0 }
                    },
                    "output": [],
                }),
            ),
        )));
        observer.observe_event(&ResponsesWebSocketEvent::Done(done_event(json!({
            "id": "resp_1",
            "status": "completed",
            "usage": { "input_tokens": 10, "output_tokens": 20 },
            "output": [
                {
                    "type": "web_search_call",
                    "id": "ws_1",
                    "status": "completed",
                    "action": { "queries": ["weather sf"] },
                    "results": [ { "url": "https://example.com" } ]
                }
            ],
        }))));

        // Turn 2: continuation, no hosted tools.
        observer.observe_send(Some("resp_1"));
        observer.observe_event(&ResponsesWebSocketEvent::Done(done_event(json!({
            "id": "resp_2",
            "status": "completed",
            "usage": { "input_tokens": 5, "output_tokens": 7 },
            "output": [],
        }))));

        observer.observe_close("client_close");
    });

    let kinds: Vec<&str> = events.iter().map(|e| e.kind.discriminant()).collect();
    assert_eq!(
        kinds,
        vec![
            "response.session_started",
            "response.turn_started",
            "tool.hosted_invoked",
            "tool.hosted_completed",
            "response.turn_completed",
            "response.turn_started",
            "response.turn_completed",
            "response.session_ended",
        ],
        "envelope sequence is not well-formed"
    );

    for evt in &events {
        assert_eq!(evt.version, SCHEMA_VERSION);
        assert_eq!(evt.conversation_id, "conv-multi");
    }

    // session_started carries the model id.
    match &events[0].kind {
        EventKind::ResponseSessionStarted { model, session_id } => {
            assert_eq!(model, "gpt-5");
            assert_eq!(session_id, "sess-multi");
        }
        other => panic!("expected ResponseSessionStarted, got {other:?}"),
    }

    // Turn 1 completion: usage + 1 hosted tool call.
    match &events[4].kind {
        EventKind::ResponseTurnCompleted {
            response_id,
            previous_response_id,
            status,
            tokens_in,
            tokens_out,
            hosted_tool_calls,
            session_id,
            ..
        } => {
            assert_eq!(session_id, "sess-multi");
            assert_eq!(response_id, "resp_1");
            assert!(previous_response_id.is_none());
            assert_eq!(status, "completed");
            assert_eq!(*tokens_in, Some(10));
            assert_eq!(*tokens_out, Some(20));
            assert_eq!(*hosted_tool_calls, 1);
        }
        other => panic!("expected ResponseTurnCompleted, got {other:?}"),
    }

    // Turn 2 completion: previous_response_id threaded through, 0 hosted.
    match &events[6].kind {
        EventKind::ResponseTurnCompleted {
            response_id,
            previous_response_id,
            hosted_tool_calls,
            ..
        } => {
            assert_eq!(response_id, "resp_2");
            assert_eq!(previous_response_id.as_deref(), Some("resp_1"));
            assert_eq!(*hosted_tool_calls, 0);
        }
        other => panic!("expected ResponseTurnCompleted, got {other:?}"),
    }

    // Hosted-tool pair carries the web_search tool_name and the
    // turn's response_id.
    match &events[2].kind {
        EventKind::ToolHostedInvoked {
            tool_name,
            provider_call_id,
            response_id,
            ..
        } => {
            assert_eq!(tool_name, "web_search");
            assert_eq!(provider_call_id.as_deref(), Some("ws_1"));
            assert_eq!(response_id.as_deref(), Some("resp_1"));
        }
        other => panic!("expected ToolHostedInvoked, got {other:?}"),
    }

    // session_ended emitted exactly once with the close reason.
    match &events[7].kind {
        EventKind::ResponseSessionEnded { session_id, reason } => {
            assert_eq!(session_id, "sess-multi");
            assert_eq!(reason, "client_close");
        }
        other => panic!("expected ResponseSessionEnded, got {other:?}"),
    }

    // Round-trip every envelope through serde_json so the schema stays
    // self-describing for downstream consumers.
    for evt in &events {
        let value = serde_json::to_value(evt).unwrap();
        let round: rig_tap::ObservabilityEvent = serde_json::from_value(value).unwrap();
        assert_eq!(round.tick, evt.tick);
        assert_eq!(round.kind.discriminant(), evt.kind.discriminant());
    }
}

#[test]
fn error_then_close_emits_failed_turn_and_session_ended() {
    let events = capture(|| {
        let mut observer = ResponsesSessionObserver::new("conv-err", "gpt-5", "sess-err");
        observer.observe_send(None);
        observer.observe_event(&ResponsesWebSocketEvent::Error(
            serde_json::from_value(json!({
                "type": "error",
                "error": { "type": "server_error", "message": "boom" },
            }))
            .unwrap(),
        ));
        observer.observe_close("transport_error");
    });

    let kinds: Vec<&str> = events.iter().map(|e| e.kind.discriminant()).collect();
    assert_eq!(
        kinds,
        vec![
            "response.session_started",
            "response.turn_started",
            "response.turn_completed",
            "response.session_ended",
        ]
    );

    match &events[2].kind {
        EventKind::ResponseTurnCompleted { status, .. } => assert_eq!(status, "error"),
        other => panic!("expected ResponseTurnCompleted, got {other:?}"),
    }
    match &events[3].kind {
        EventKind::ResponseSessionEnded { reason, .. } => assert_eq!(reason, "transport_error"),
        other => panic!("expected ResponseSessionEnded, got {other:?}"),
    }
}

#[test]
fn turn_completed_stamps_duration() {
    let events = capture(|| {
        let mut observer = ResponsesSessionObserver::new("conv-dur", "gpt-5", "sess-dur");
        observer.observe_send(None);
        observer.observe_event(&ResponsesWebSocketEvent::Done(done_event(json!({
            "id": "resp_1",
            "status": "completed",
        }))));
        observer.observe_close("client_close");
    });

    let turn = events
        .iter()
        .find(|e| e.kind.discriminant() == "response.turn_completed")
        .expect("turn_completed emitted");
    match &turn.kind {
        EventKind::ResponseTurnCompleted { duration_ms, .. } => {
            assert!(
                duration_ms.is_some(),
                "observer owns both ends of the turn so duration_ms must be populated"
            );
        }
        other => panic!("expected ResponseTurnCompleted, got {other:?}"),
    }
}
