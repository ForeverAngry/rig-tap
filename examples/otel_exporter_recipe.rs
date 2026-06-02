//! OpenTelemetry exporter recipe for `rig-tap` events.
//!
//! Demonstrates the **wire shape an OTel collector receives** when a
//! consumer subscribes to the `rig_tap` tracing target. No
//! `opentelemetry-*` crate is pulled in — the point of this example is
//! to show what an in-process `tracing-opentelemetry` layer (or any
//! collector pipeline ingesting the structured `tracing` fields) sees,
//! so operators can write the attribute mapping ahead of time.
//!
//! Run:
//!
//! ```bash
//! cargo run --example otel_exporter_recipe --features subscriber
//! ```
//!
//! Output: one JSON object per emitted event, each containing both the
//! full `event` envelope and the flat `rig_tap.*` scalar attributes
//! that map 1:1 to OTel span / log attribute keys.
//!
//! ## Attribute mapping
//!
//! The recipe is "no transform required". Every `rig_tap.*` field is
//! already an OpenTelemetry-compatible attribute name (dot-separated,
//! snake_case). The minimum-viable collector pipeline is:
//!
//! - **Receiver**: anything that ingests `tracing` output (an in-process
//!   `tracing-opentelemetry` layer, or `stdout` → `filelog` receiver →
//!   OTel collector if running out-of-process).
//! - **Processor**: optional `attributes` processor to rename keys (see
//!   the README's "OpenTelemetry exporter recipe" section for a YAML
//!   sample).
//! - **Exporter**: any OTLP-compatible backend (Tempo, Jaeger, Honeycomb,
//!   Datadog).
//!
//! ## What this example does NOT do
//!
//! - It does not configure an actual OTel SDK. That's a one-line
//!   `tracing-opentelemetry::layer().with_tracer(tracer)` swap in the
//!   subscriber stack and depends on the deployment's exporter choice.
//! - It does not emit spans. `rig-tap` events are `info!` log records
//!   (intentionally — they survive sampling and don't require a parent
//!   span). Collectors that prefer spans can wrap each event in a
//!   zero-duration span keyed by `rig_tap.conversation_id`.

#[cfg(feature = "subscriber")]
mod recipe {
    use rig_tap::{CapturingLayer, EventKind, emit_kind};
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    pub fn run() {
        let layer = CapturingLayer::new();
        let snapshot_handle = layer.clone();
        let _guard = tracing_subscriber::registry().with(layer).set_default();

        // Fan out one event per major family so consumers can preview
        // the attribute set they'll need to budget for.
        emit_kind(
            "demo-conversation",
            EventKind::PromptCompleted {
                model: "gpt-5".into(),
                tokens_in: Some(412),
                tokens_out: Some(96),
                cached_tokens_in: Some(128),
                reasoning_tokens: Some(64),
                cost_usd: Some(0.0123),
                finish_reason: Some("stop".into()),
                response_id: Some("resp_abc".into()),
                previous_response_id: Some("resp_aaz".into()),
                time_to_first_token_ms: Some(180),
                duration_ms: Some(742),
            },
        );

        emit_kind(
            "demo-conversation",
            EventKind::ToolInvoked {
                tool_name: "web_search".into(),
                provider_call_id: Some("prov_001".into()),
                call_id: "call_001".into(),
                args_json: r#"{"q":"otel"}"#.into(),
                truncated: false,
            },
        );

        emit_kind(
            "demo-conversation",
            EventKind::EvalReport {
                report_id: "run-2026-05-27".into(),
                dataset: "beir/scifact".into(),
                metric: "ndcg@10".into(),
                value: 0.512,
                ci_low: Some(0.487),
                ci_high: Some(0.538),
                baseline_value: Some(0.498),
                delta: Some(0.014),
                verdict: Some("improved".into()),
                sample_size: Some(300),
            },
        );

        let snapshot = snapshot_handle.snapshot();
        println!(
            "Captured {} events. Each row below shows the JSON envelope plus the \
             flat OTel-ready attribute set a collector would extract from the \
             tracing fields.\n",
            snapshot.len()
        );

        for envelope in snapshot.iter() {
            let kind = envelope.kind.discriminant();
            let scalars = envelope.kind.scalar_fields();
            println!("── event: {kind} ──");
            // Stable scalar attributes — every rig_tap.* field that's
            // non-empty maps directly to an OTel attribute key.
            print_attr("rig_tap.kind", kind);
            print_attr("rig_tap.conversation_id", &envelope.conversation_id);
            print_attr("rig_tap.tick", &envelope.tick.to_string());
            print_attr(
                "rig_tap.occurred_at_millis",
                &envelope.occurred_at_millis.to_string(),
            );
            print_attr("rig_tap.version", &envelope.version.to_string());
            print_attr("rig_tap.kernel_id", scalars.kernel_id);
            print_attr("rig_tap.tool_name", scalars.tool_name);
            print_attr("rig_tap.call_id", scalars.call_id);
            print_attr("rig_tap.skill_id", scalars.skill_id);
            print_attr("rig_tap.model", scalars.model);
            print_attr("rig_tap.response_id", scalars.response_id);
            print_attr("rig_tap.previous_response_id", scalars.previous_response_id);
            print_attr("rig_tap.dataset", scalars.dataset);
            print_attr("rig_tap.metric", scalars.metric);
            print_attr("rig_tap.verdict", scalars.verdict);
            // Full JSON envelope — collectors that want everything can
            // forward this as a single `event` attribute and decode
            // server-side.
            if let Ok(json) = serde_json::to_string(envelope) {
                println!("  event = {json}");
            }
            println!();
        }
    }

    fn print_attr(key: &str, value: &str) {
        if !value.is_empty() {
            println!("  {key} = {value}");
        }
    }
}

#[cfg(feature = "subscriber")]
fn main() {
    recipe::run();
}

#[cfg(not(feature = "subscriber"))]
fn main() {
    eprintln!("This example requires --features subscriber.");
}
