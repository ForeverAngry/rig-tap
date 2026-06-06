//! Native metrics emission via the [`metrics`](https://crates.io/crates/metrics) crate.
//!
//! Enabled by the `metrics` feature. This module automatically emits
//! pre-aggregated metrics (counters and histograms) for major events, pulling
//! fields out of the [`ObservabilityEvent`] envelope.
//!
//! This allows operators to build TSDB or Prometheus dashboards immediately,
//! bypassing the need for an OpenTelemetry Collector log-extraction pipeline.

#[cfg(feature = "metrics")]
use crate::event::{EventKind, ObservabilityEvent};

#[cfg(feature = "metrics")]
pub(crate) fn emit_metrics(event: &ObservabilityEvent) {
    use metrics::{counter, histogram};

    // Shared labels applied to most metrics
    let mut labels = vec![("kind".to_string(), event.kind.discriminant().to_string())];

    let fields = event.kind.scalar_fields();
    if !fields.model.is_empty() {
        labels.push(("model".to_string(), fields.model.to_string()));
    }
    if !fields.tool_name.is_empty() {
        labels.push(("tool_name".to_string(), fields.tool_name.to_string()));
    }
    if !fields.error_class.is_empty() {
        labels.push(("error_class".to_string(), fields.error_class.to_string()));
    }

    // Count every event
    counter!("rig_tap.events.count", &labels).increment(1);

    // Emit detailed metrics based on event contents
    match &event.kind {
        EventKind::PromptCompleted {
            tokens_in,
            tokens_out,
            cached_tokens_in,
            reasoning_tokens,
            time_to_first_token_ms,
            duration_ms,
            ..
        } => {
            if let Some(t) = tokens_in {
                counter!("rig_tap.tokens.in", &labels).increment(*t);
            }
            if let Some(t) = tokens_out {
                counter!("rig_tap.tokens.out", &labels).increment(*t);
            }
            if let Some(t) = cached_tokens_in {
                counter!("rig_tap.tokens.cached_in", &labels).increment(*t);
            }
            if let Some(t) = reasoning_tokens {
                counter!("rig_tap.tokens.reasoning", &labels).increment(*t);
            }
            if let Some(t) = time_to_first_token_ms {
                histogram!("rig_tap.ttft_ms", &labels).record(*t as f64);
            }
            if let Some(d) = duration_ms {
                histogram!("rig_tap.duration_ms", &labels).record(*d as f64);
            }
        }
        EventKind::ToolCompleted { duration_ms, .. }
        | EventKind::ToolHostedCompleted { duration_ms, .. }
        | EventKind::ResponseTurnCompleted { duration_ms, .. }
        | EventKind::EmbeddingCompleted { duration_ms, .. }
        | EventKind::RetrievalQueried { duration_ms, .. }
        | EventKind::RerankCompleted { duration_ms, .. } => {
            if let Some(d) = duration_ms {
                histogram!("rig_tap.duration_ms", &labels).record(*d as f64);
            }
        }
        EventKind::ContextSampled {
            message_count,
            byte_size,
            ..
        }
        | EventKind::ContextPersisted {
            message_count,
            byte_size,
            ..
        } => {
            histogram!("rig_tap.memory.message_count", &labels).record(*message_count as f64);
            histogram!("rig_tap.memory.byte_size", &labels).record(*byte_size as f64);
        }
        _ => {}
    }
}
