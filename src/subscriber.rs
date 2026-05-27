//! Optional `tracing_subscriber::Layer` that captures `rig_tap` events
//! into an in-memory buffer.
//!
//! Off by default. Enable with `--features subscriber`. The layer is suitable
//! for tests, examples, and small in-process tools (assertions, demos,
//! exploratory CLIs). Production deployments that ship events off-host
//! should wire a non-blocking sink (e.g. `tracing_appender::non_blocking`
//! or a bounded async channel) so a slow consumer cannot backpressure the
//! agent.
//!
//! # Example
//!
//! ```
//! use rig_tap::{CapturingLayer, EventKind, emit_kind};
//! use tracing_subscriber::layer::SubscriberExt;
//! use tracing_subscriber::util::SubscriberInitExt;
//!
//! let capture = CapturingLayer::new();
//! let subscriber = tracing_subscriber::registry().with(capture.clone());
//! let _guard = subscriber.set_default();
//!
//! emit_kind(
//!     "thread-1",
//!     EventKind::PromptStarted {
//!         model: "gpt-4o".into(),
//!         messages_in: 1,
//!     },
//! );
//!
//! let events = capture.snapshot();
//! assert_eq!(events.len(), 1);
//! ```

use crate::event::ObservabilityEvent;
use crate::query::EventQuery;
use std::sync::{Arc, Mutex};
use tracing_subscriber::Layer;

/// In-memory `tracing_subscriber::Layer` that decodes every `rig_tap`
/// event into an [`ObservabilityEvent`] and appends it to a shared buffer.
///
/// Clone freely: the underlying `Arc<Mutex<Vec<_>>>` is shared.
#[derive(Default, Clone)]
pub struct CapturingLayer {
    events: Arc<Mutex<Vec<ObservabilityEvent>>>,
}

impl CapturingLayer {
    /// Build an empty layer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return a copy of all captured events in insertion order. If the
    /// internal mutex was poisoned by a panicking writer in another thread,
    /// the layer still returns the events recorded before the poison.
    pub fn snapshot(&self) -> Vec<ObservabilityEvent> {
        match self.events.lock() {
            Ok(guard) => guard.clone(),
            Err(poison) => poison.into_inner().clone(),
        }
    }

    /// Return a query view over a copy of all captured events.
    pub fn query(&self) -> EventQuery {
        EventQuery::new(self.snapshot())
    }

    /// Clear the captured buffer. No-op if the mutex is poisoned and cannot
    /// be recovered.
    pub fn clear(&self) {
        match self.events.lock() {
            Ok(mut guard) => guard.clear(),
            Err(poison) => poison.into_inner().clear(),
        }
    }

    /// Number of events captured so far.
    pub fn len(&self) -> usize {
        match self.events.lock() {
            Ok(guard) => guard.len(),
            Err(poison) => poison.into_inner().len(),
        }
    }

    /// `true` when no events have been captured.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl std::fmt::Debug for CapturingLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapturingLayer")
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}

impl<S> Layer<S> for CapturingLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if let Some(parsed) = crate::extract::extract_event(event) {
            match self.events.lock() {
                Ok(mut guard) => guard.push(parsed),
                Err(poison) => poison.into_inner().push(parsed),
            }
        }
    }
}
