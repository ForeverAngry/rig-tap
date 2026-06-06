//! Native metrics validation for the `metrics` feature.
//!
//! Uses a thread-local recorder via `metrics::with_local_recorder` rather
//! than the process-global `install()`, so the test is pollution-proof and
//! can assert exact counter values.

#![cfg(feature = "metrics")]
#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use metrics_util::debugging::{DebugValue, DebuggingRecorder};
use rig_tap::{EventKind, emit_kind};

#[test]
fn prompt_completed_emits_token_and_latency_series() {
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();

    metrics::with_local_recorder(&recorder, || {
        emit_kind(
            "metrics-conv-1",
            EventKind::PromptCompleted {
                model: "m1".into(),
                tokens_in: Some(42),
                tokens_out: Some(10),
                cached_tokens_in: None,
                reasoning_tokens: None,
                cost_usd: None,
                finish_reason: None,
                response_id: None,
                previous_response_id: None,
                time_to_first_token_ms: Some(100),
                duration_ms: Some(150),
            },
        );
    });

    let mut tokens_in = 0u64;
    let mut tokens_out = 0u64;
    let mut event_count = 0u64;
    let mut saw_ttft = false;
    let mut saw_duration = false;

    for (key, _unit, _desc, value) in snapshotter.snapshot().into_vec() {
        let name = key.key().name().to_string();
        match (name.as_str(), value) {
            ("rig_tap.tokens.in", DebugValue::Counter(v)) => tokens_in += v,
            ("rig_tap.tokens.out", DebugValue::Counter(v)) => tokens_out += v,
            ("rig_tap.events.count", DebugValue::Counter(v)) => event_count += v,
            ("rig_tap.ttft_ms", DebugValue::Histogram(samples)) => {
                saw_ttft = samples
                    .iter()
                    .any(|s| (s.into_inner() - 100.0).abs() < f64::EPSILON);
            }
            ("rig_tap.duration_ms", DebugValue::Histogram(samples)) => {
                saw_duration = samples
                    .iter()
                    .any(|s| (s.into_inner() - 150.0).abs() < f64::EPSILON);
            }
            _ => {}
        }
    }

    assert_eq!(
        tokens_in, 42,
        "tokens.in counter must equal the emitted value"
    );
    assert_eq!(
        tokens_out, 10,
        "tokens.out counter must equal the emitted value"
    );
    assert_eq!(event_count, 1, "exactly one event must be counted");
    assert!(
        saw_ttft,
        "ttft_ms histogram must record the time_to_first_token_ms sample"
    );
    assert!(
        saw_duration,
        "duration_ms histogram must record the duration_ms sample"
    );
}

#[test]
fn failure_event_carries_error_class_label() {
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();

    metrics::with_local_recorder(&recorder, || {
        emit_kind(
            "metrics-conv-2",
            EventKind::PromptFailed {
                model: "m1".into(),
                error_class: rig_tap::ErrorClass::RateLimit,
                message: "429".into(),
                retriable: true,
                provider_error_code: None,
                http_status: Some(429),
            },
        );
    });

    let labelled = snapshotter
        .snapshot()
        .into_vec()
        .into_iter()
        .any(|(key, _u, _d, _v)| {
            key.key().name() == "rig_tap.events.count"
                && key
                    .key()
                    .labels()
                    .any(|l| l.key() == "error_class" && l.value() == "rate_limit")
        });

    assert!(
        labelled,
        "failure metric must carry the error_class=rate_limit label"
    );
}
