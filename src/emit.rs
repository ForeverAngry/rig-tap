//! Tracing transport for [`ObservabilityEvent`].
//!
//! All events are emitted as a single `tracing::info!` call under the
//! `rig_tap` target. The legacy `event` field carries the JSON-encoded
//! envelope, while stable scalar `rig_tap.*` fields make OpenTelemetry
//! collector routing and indexing possible without JSON parsing.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::Error;
use crate::event::{EventKind, ObservabilityEvent, SCHEMA_VERSION};

/// Target string used on every `rig_tap` event.
pub const EVENT_TARGET: &str = "rig_tap";

static TICK: AtomicU64 = AtomicU64::new(0);

/// Return the next monotonic per-process tick.
pub fn next_tick() -> u64 {
    TICK.fetch_add(1, Ordering::Relaxed)
}

/// Return the current wall-clock time in milliseconds since the Unix epoch.
/// Returns `0` if the clock is set before the epoch.
pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Return the numeric id of the currently-active `tracing::Span`, if any.
///
/// Mirrors [`tracing::span::Id::into_u64`]. Subscribers that respect
/// span context (e.g. `tracing-opentelemetry`) attach this id to every
/// event automatically; surfacing it on the envelope lets consumers
/// reading only structured fields stitch events into the same
/// waterfall.
pub fn current_span_id() -> Option<u64> {
    tracing::Span::current().id().map(|id| id.into_u64())
}

/// Build a fully-formed [`ObservabilityEvent`] for `kind` belonging to
/// `conversation_id`, stamped with the next tick and current wall time.
pub fn build_event(conversation_id: impl Into<String>, kind: EventKind) -> ObservabilityEvent {
    ObservabilityEvent {
        version: SCHEMA_VERSION,
        occurred_at_millis: now_millis(),
        tick: next_tick(),
        conversation_id: conversation_id.into(),
        span_id: current_span_id(),
        kind,
    }
}

/// Emit `event` over the `rig_tap` tracing target as a single
/// `info!`-level event carrying a JSON-encoded `event` field.
///
/// Returns an [`Error`] if the event fails to serialize. Callers in library
/// code typically discard the result via [`emit`] which logs serialization
/// failures rather than propagating them.
pub fn try_emit(event: &ObservabilityEvent) -> Result<(), Error> {
    let json = serde_json::to_string(event)?;
    let fields = event.kind.scalar_fields();
    tracing::info!(
        target: EVENT_TARGET,
        event = %json,
        rig_tap.version = event.version,
        rig_tap.kind = event.kind.discriminant(),
        rig_tap.conversation_id = %event.conversation_id,
        rig_tap.tick = event.tick,
        rig_tap.occurred_at_millis = event.occurred_at_millis,
        // Numeric `tracing::Span` id captured at emit time. `0` =
        // absent (no span was active). Consumers correlating via
        // `tracing-opentelemetry` already get the span via subscriber
        // context; this field is for collectors that read only the
        // structured `rig_tap.*` attributes.
        rig_tap.span_id = event.span_id.unwrap_or(0),
        // Per-variant scalar correlators. Absent values are emitted as
        // empty strings (see `ScalarFields` rustdoc) — collectors should
        // filter `rig_tap.<field> != ""` to detect presence.
        rig_tap.kernel_id = fields.kernel_id,
        rig_tap.tool_name = fields.tool_name,
        rig_tap.call_id = fields.call_id,
        rig_tap.skill_id = fields.skill_id,
        rig_tap.model = fields.model,
        rig_tap.response_id = fields.response_id,
        rig_tap.previous_response_id = fields.previous_response_id,
        rig_tap.dataset = fields.dataset,
        rig_tap.metric = fields.metric,
        rig_tap.verdict = fields.verdict,
        rig_tap.error_class = fields.error_class,
    );
    Ok(())
}

/// Emit `event` over the `rig_tap` tracing target. Serialization failures
/// are logged at `warn` level under the same target and otherwise swallowed
/// so that telemetry never panics the agent loop.
pub fn emit(event: &ObservabilityEvent) {
    if let Err(err) = try_emit(event) {
        tracing::warn!(
            target: EVENT_TARGET,
            error = %err,
            error_kind = err.kind(),
            "rig-tap: failed to emit event",
        );
    }
}

/// Convenience: build + emit in one call.
pub fn emit_kind(conversation_id: impl Into<String>, kind: EventKind) {
    let event = build_event(conversation_id, kind);
    emit(&event);
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

    #[test]
    fn tick_is_monotonic() {
        let a = next_tick();
        let b = next_tick();
        assert!(b > a);
    }

    #[test]
    fn build_event_stamps_envelope() {
        let evt = build_event(
            "c",
            EventKind::PromptStarted {
                model: "m".into(),
                messages_in: 0,
            },
        );
        assert_eq!(evt.version, SCHEMA_VERSION);
        assert_eq!(evt.conversation_id, "c");
        // occurred_at_millis is 0 only if the system clock is broken; in tests
        // it should always be a positive value.
        assert!(evt.occurred_at_millis > 0);
    }
}
