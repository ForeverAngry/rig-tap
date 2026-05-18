//! Backend-agnostic observability event schema and taps for [Rig](https://crates.io/crates/rig-core).
//!
//! See the crate [README](../README.md) for the full schema and consumer
//! recipe. The two consumer-facing types are:
//!
//! - [`TelemetryHook`] — implements [`rig::agent::PromptHook`] and emits
//!   `prompt.*` and `tool.*` events.
//! - [`ObservedMemory`] — wraps any [`rig::memory::ConversationMemory`] and
//!   emits `context.sampled` on every load.
//!
//! Plus [`ChainedHook`] for composing two `PromptHook`s on a single agent.
//!
//! # Wire format
//!
//! All events are emitted as a single `tracing::info!` event on the
//! [`EVENT_TARGET`] target (`"rig_observe"`) with a single
//! string field `event` carrying the JSON-encoded
//! [`ObservabilityEvent`]. Consumers attach a `tracing_subscriber::Layer`
//! filtered to that target.
//!
//! # Subscriber sizing
//!
//! Emission is synchronous: every event runs serde + the registered
//! layers on the calling task. Per-request hot paths (every prompt, every
//! tool call, every memory load) call into the tracing dispatcher
//! directly. For production deployments — especially ones that ship
//! events off-host — wire a non-blocking sink (e.g.
//! `tracing_appender::non_blocking` or a bounded channel feeding an
//! async exporter) so a slow consumer can't backpressure the agent.
//! In-process counters and the bundled doc-test layer are fine
//! synchronous.
//!
//! # Example
//!
//! ```no_run
//! use rig_observe::{TelemetryHook, ObservedMemory};
//! use rig::memory::InMemoryConversationMemory;
//!
//! # fn build<M: rig::completion::CompletionModel>() -> TelemetryHook<M> {
//! let memory = ObservedMemory::new(InMemoryConversationMemory::new());
//! let hook = TelemetryHook::<M>::with_defaults("gpt-4o", "thread-1");
//! // agent.memory(memory).with_hook(hook)
//! # hook }
//! ```

#![deny(missing_docs)]

pub mod extract;

mod chained;
#[cfg(feature = "compose")]
mod dispatch;
pub mod emit;
mod error;
mod event;
mod hook;
mod observed_memory;
#[cfg(feature = "subscriber")]
mod subscriber;

pub use chained::ChainedHook;
#[cfg(feature = "compose")]
pub use dispatch::DispatchObserveHook;
pub use emit::{EVENT_TARGET, build_event, emit, emit_kind, try_emit};
pub use error::Error;
pub use event::{
    EventKind, ObservabilityEvent, PAYLOAD_TRUNCATE_BYTES, SCHEMA_VERSION, truncate_utf8,
};
pub use extract::extract_event;
pub use hook::{ConversationIdResolver, ModelResolver, TelemetryHook, TelemetryHookConfig};
pub use observed_memory::ObservedMemory;
#[cfg(feature = "subscriber")]
pub use subscriber::CapturingLayer;
