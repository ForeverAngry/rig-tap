//! Extraction helpers for decoding emitted observability events.

use tracing::field::{Field, Visit};

use crate::emit::EVENT_TARGET;
use crate::event::ObservabilityEvent;

struct EventVisitor {
    json: Option<String>,
}

impl Visit for EventVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "event" {
            self.json = Some(value.to_string());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "event" && self.json.is_none() {
            self.json = Some(format!("{value:?}"));
        }
    }
}

/// Extracts an [`ObservabilityEvent`] from a given tracing event, if the event
/// belongs to the `rig_observe` target and is valid JSON.
///
/// This helper is intended for consumers who want to write their own custom
/// `tracing_subscriber::Layer` without duplicating the extraction boilerplate.
pub fn extract_event(event: &tracing::Event<'_>) -> Option<ObservabilityEvent> {
    if event.metadata().target() != EVENT_TARGET {
        return None;
    }
    let mut visitor = EventVisitor { json: None };
    event.record(&mut visitor);
    let json = visitor.json?;
    serde_json::from_str(&json).ok()
}
