//! Production validation for the enterprise extension surface:
//! [`RedactionPolicy`] scrubbing on the agent `PromptHook` path and the
//! [`AdaptiveErrorPolicy`] tail-sampling contract.
//!
//! The kernel-dispatch redaction path is validated in
//! `tests/dispatch_observe.rs`; the hosted-tool redaction path in
//! `tests/responses_session.rs`; native metrics in `tests/metrics.rs`.

#![cfg(feature = "subscriber")]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::expect_used
)]

use std::borrow::Cow;
use std::sync::Arc;

use rig_tap::{
    AdaptiveErrorPolicy, CapturingLayer, EventFilter, EventKind, RatePolicy, RedactionPolicy,
    SamplingPolicy,
};
use tracing_subscriber::layer::SubscriberExt;

/// A scrubber that rewrites two known sensitive tokens. Returns
/// `Cow::Owned` only when it actually changes the input, exercising the
/// allocating branch; otherwise borrows through.
#[derive(Debug)]
struct Scrubber;

impl RedactionPolicy for Scrubber {
    fn redact_tool_args<'a>(&self, _tool_name: &str, args_json: &'a str) -> Cow<'a, str> {
        if args_json.contains("secret_token_123") {
            Cow::Owned(args_json.replace("secret_token_123", "[REDACTED]"))
        } else {
            Cow::Borrowed(args_json)
        }
    }

    fn redact_tool_result<'a>(&self, _tool_name: &str, result: &'a str) -> Cow<'a, str> {
        if result.contains("SSN=000-00-0000") {
            Cow::Owned(result.replace("SSN=000-00-0000", "[REDACTED-SSN]"))
        } else {
            Cow::Borrowed(result)
        }
    }
}

#[test]
fn redaction_policy_scrubs_agent_hook_payloads() {
    use rig::agent::PromptHook;

    let capture = CapturingLayer::new();
    let probe = capture.clone();
    let subscriber = tracing_subscriber::registry().with(capture);

    tracing::subscriber::with_default(subscriber, || {
        // `PromptHook` methods are async; drive them on a current-thread
        // runtime *inside* the subscriber guard so the thread-local
        // dispatcher stays attached across `.await` points.
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(async {
            let hook = rig_tap::TelemetryHook::<rig::providers::openai::CompletionModel>::new(
                rig_tap::TelemetryHookConfig::new("gpt-4", "conv-redact"),
            )
            .with_redaction_policy(Arc::new(Scrubber));

            hook.on_tool_call("fetch", None, "call-1", r#"{"token":"secret_token_123"}"#)
                .await;
            hook.on_tool_result(
                "fetch",
                None,
                "call-1",
                r#"{"token":"secret_token_123"}"#,
                r#"{"data":"SSN=000-00-0000"}"#,
            )
            .await;
        });
    });

    let events = probe.query();

    let invoked = events.filter(&EventFilter::new().kind("tool.invoked"));
    assert_eq!(invoked.len(), 1, "expected exactly one tool.invoked");
    match &invoked[0].kind {
        EventKind::ToolInvoked { args_json, .. } => {
            assert_eq!(
                args_json, r#"{"token":"[REDACTED]"}"#,
                "args secret must be scrubbed before emission"
            );
        }
        other => panic!("expected ToolInvoked, got {other:?}"),
    }

    let completed = events.filter(&EventFilter::new().kind("tool.completed"));
    assert_eq!(completed.len(), 1, "expected exactly one tool.completed");
    match &completed[0].kind {
        EventKind::ToolCompleted { result, .. } => {
            assert_eq!(
                result, r#"{"data":"[REDACTED-SSN]"}"#,
                "result secret must be scrubbed before emission"
            );
        }
        other => panic!("expected ToolCompleted, got {other:?}"),
    }
}

#[test]
fn adaptive_error_policy_keeps_failures_and_drops_skips() {
    // Inner policy drops everything (rate 0.0 across the board).
    let inner = RatePolicy::new().with_default_rate(0.0);
    let adaptive = AdaptiveErrorPolicy::new(inner);

    // Happy paths are dropped by the inner policy.
    assert!(!adaptive.should_sample("tool.invoked", "call-id"));
    assert!(!adaptive.should_sample("tool.completed", "call-id"));
    assert!(!adaptive.should_sample("prompt.completed", "conv-id"));

    // Genuine failure / recovery anomalies bypass the drop rate.
    assert!(adaptive.should_sample("tool.failed", "call-id"));
    assert!(adaptive.should_sample("prompt.failed", "conv-id"));
    assert!(adaptive.should_sample("tool.terminated", "call-id"));
    assert!(adaptive.should_sample("compose.recovery", "kernel-id"));
    assert!(adaptive.should_sample("compose.retry_attempt", "kernel-id"));

    // `tool.skipped` is a routine gating decision, NOT an anomaly, so the
    // default allowlist must let the inner policy drop it.
    assert!(
        !adaptive.should_sample("tool.skipped", "call-id"),
        "tool.skipped must not be force-kept by default"
    );
}

#[test]
fn adaptive_error_policy_also_keep_extends_allowlist() {
    let inner = RatePolicy::new().with_default_rate(0.0);
    let adaptive = AdaptiveErrorPolicy::new(inner).also_keep("tool.skipped");

    // Opt-in keeps the extra kind…
    assert!(adaptive.should_sample("tool.skipped", "call-id"));
    // …without force-keeping unrelated happy-path kinds.
    assert!(!adaptive.should_sample("tool.completed", "call-id"));
}
