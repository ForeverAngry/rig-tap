#![cfg(feature = "subscriber")]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::expect_used,
    clippy::collapsible_if
)]

use rig_tap::{
    AdaptiveErrorPolicy, CapturingLayer, EventFilter, EventKind, RatePolicy, RedactionPolicy,
};
use std::sync::Arc;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[derive(Debug)]
struct Scrubber;
impl RedactionPolicy for Scrubber {
    fn redact_tool_args(&self, _tool_name: &str, args_json: &str) -> String {
        args_json.replace("secret_token_123", "[REDACTED]")
    }
    fn redact_tool_result(&self, _tool_name: &str, result: &str) -> String {
        result.replace("SSN=000-00-0000", "[REDACTED-SSN]")
    }
}

#[test]
fn redaction_policy_scrubs_payloads() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(async {
        let capture = CapturingLayer::new();
        let subscriber = tracing_subscriber::registry().with(capture.clone());
        // Use tracing::subscriber::with_default to ensure async yields don't lose context
        // inside the current thread runtime if tokio decides to park tasks.
        let _guard = subscriber.set_default();

        use rig::agent::PromptHook;
        let hook = rig_tap::TelemetryHook::<rig::providers::openai::CompletionModel>::new(
            rig_tap::TelemetryHookConfig::new("gpt-4", "conv-redact"),
        )
        .with_redaction_policy(Arc::new(Scrubber));

        let _ = hook
            .on_tool_call("fetch", None, "call-1", "{\"token\":\"secret_token_123\"}")
            .await;
        let _ = hook
            .on_tool_result(
                "fetch",
                None,
                "call-1",
                "{\"token\":\"secret_token_123\"}",
                "{\"data\":\"SSN=000-00-0000\"}",
            )
            .await;

        let invoked = capture
            .query()
            .filter(&EventFilter::new().kind("tool.invoked"));
        match &invoked[0].kind {
            EventKind::ToolInvoked { args_json, .. } => {
                assert_eq!(args_json, "{\"token\":\"[REDACTED]\"}");
            }
            _ => panic!(),
        }

        let completed = capture
            .query()
            .filter(&EventFilter::new().kind("tool.completed"));
        assert_eq!(completed.len(), 1);
        match &completed[0].kind {
            EventKind::ToolCompleted { result, .. } => {
                assert_eq!(result, "{\"data\":\"[REDACTED-SSN]\"}");
            }
            _ => panic!(),
        }
    })
}

#[tokio::test]
async fn adaptive_error_policy_bypasses_drop_rate() {
    use rig_tap::SamplingPolicy;
    // Set inner policy to drop absolutely everything
    let base_policy = RatePolicy::new()
        .with_rate("tool.invoked", 0.0)
        .with_default_rate(0.0);

    let adaptive = AdaptiveErrorPolicy::new(base_policy);

    // Happy paths should be dropped by inner
    assert!(!adaptive.should_sample("tool.invoked", "call-id"));
    assert!(!adaptive.should_sample("prompt.completed", "conv-id"));

    // Error paths MUST be retained by Adaptive wrapper
    assert!(adaptive.should_sample("tool.failed", "call-id"));
    assert!(adaptive.should_sample("prompt.failed", "conv-id"));
    assert!(adaptive.should_sample("tool.terminated", "call-id"));
    assert!(adaptive.should_sample("compose.recovery", "kernel-id"));
}

#[cfg(feature = "metrics")]
#[tokio::test]
async fn native_metrics_are_emitted() {
    use metrics_util::debugging::{DebuggingRecorder, Snapshot};
    use rig_tap::emit_kind;

    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();
    recorder.install().unwrap(); // installs globally

    // Emit some events that should trigger metrics
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

    let snapshot: Snapshot = snapshotter.snapshot();

    // Check that counters were updated correctly
    let mut tokens_in_accumulated = 0;
    for (key, _unit, _desc, value) in snapshot.into_vec() {
        let key_name = key.key().name().to_string();
        if let metrics_util::debugging::DebugValue::Counter(v) = value {
            if key_name == "rig_tap.tokens.in" {
                tokens_in_accumulated += v;
            }
        }
    }

    assert!(
        tokens_in_accumulated >= 42,
        "Expected tokens.in metric to be collected"
    );
}
