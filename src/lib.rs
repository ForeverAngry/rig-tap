//! Emits uniform telemetry for [Rig](https://crates.io/crates/rig-core) agents
//! and companion crates.
//!
//! `rig-tap` defines a stable, versioned [`ObservabilityEvent`] stream for
//! prompt, tool, context, memory, and dispatch lifecycle events. Producer
//! crates can use the same event vocabulary whether the event came from a Rig
//! agent hook, `rig-compose` dispatch, `rig-memvid` memory behavior, or host
//! application code.
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
//! # Why rig-tap (vs. Rig's hooks today)
//!
//! Rig already exposes the raw callbacks and data: [`rig::agent::PromptHook`]
//! (`on_completion_call` / `on_completion_response` / `on_tool_call` /
//! `on_tool_result`), the `Usage` token counts on `CompletionResponse`, typed
//! `PromptError` / `CompletionError` values, and GenAI span conventions. That
//! is a callback surface scoped to one agent loop — ephemeral, provider-shaped,
//! with no on-the-wire form. Nothing leaves the process, correlates across
//! calls, or speaks a vocabulary other crates share unless you write that glue.
//!
//! `rig-tap` turns those callbacks into a stable, versioned, queryable
//! telemetry contract:
//!
//! - **A versioned wire schema, not just callbacks** — every event is a flat,
//!   `serde`-stable [`ObservabilityEvent`] envelope. [`SCHEMA_VERSION`] +
//!   `#[non_exhaustive]` make additive evolution non-breaking.
//! - **One vocabulary across the ecosystem** — the same [`EventKind`] covers
//!   agent prompts/tools, `rig-compose` kernel dispatch, memory/context, eval
//!   reports, and stateful provider sessions. `PromptHook` only sees the
//!   in-loop agent path.
//! - **OTel-routable scalars** — each event surfaces [`ScalarFields`] as
//!   first-class `tracing` attributes (`model`, `tool_name`, `call_id`,
//!   `error_class`, …) plus `span_id` mirroring, so collectors route without
//!   parsing JSON.
//! - **Lifecycle pairing** — a stable `call_id` pairs `tool.invoked` with its
//!   terminal event; `previous_response_id` chains stateful turns.
//! - **Failure semantics** — [`ErrorClass`] normalizes provider errors into a
//!   backend-agnostic taxonomy with a `retriable` flag and HTTP status.
//! - **Visibility the hooks lack** — provider-hosted tools (`tool.hosted_*`),
//!   latency milestones (`duration_ms`, `time_to_first_token_ms`), and
//!   Responses-WebSocket sessions (`response.*`) have no `PromptHook` analog.
//! - **Operational plumbing** — pluggable [`SamplingPolicy`], payload
//!   truncation, an in-process [`EventQuery`], runtime-agnostic emission.
//!
//! `rig-tap` is **additive** to Rig's GenAI span conventions: its events live
//! under a separate `tracing` target and can be filtered independently.
//!
//! # Wire format
//!
//! All events are emitted as a single `tracing::info!` event on the
//! [`EVENT_TARGET`] target (`"rig_tap"`). The string field `event` carries the
//! JSON-encoded [`ObservabilityEvent`], while scalar `rig_tap.*` fields expose
//! `kind`, `conversation_id`, `version`, `tick`, and `occurred_at_millis` for
//! OpenTelemetry collector routing and indexing without JSON parsing.
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
//! use rig_tap::{TelemetryHook, ObservedMemory};
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
mod insights;
mod observed_memory;
mod query;
#[cfg(feature = "openai-responses")]
pub mod responses_extract;
#[cfg(all(feature = "openai-responses-websocket", not(target_family = "wasm")))]
pub mod responses_session;
mod sampling;
#[cfg(feature = "subscriber")]
mod subscriber;

pub use chained::ChainedHook;
#[cfg(feature = "compose")]
pub use dispatch::DispatchObserveHook;
pub use emit::{EVENT_TARGET, build_event, current_span_id, emit, emit_kind, try_emit};
pub use error::Error;
pub use event::{
    ErrorClass, EventKind, ObservabilityEvent, PAYLOAD_TRUNCATE_BYTES, SCHEMA_VERSION,
    ScalarFields, truncate_utf8,
};
pub use extract::extract_event;
pub use hook::{
    ConversationIdResolver, ModelResolver, PreviousResponseIdResolver, TelemetryHook,
    TelemetryHookConfig,
};
pub use insights::{
    Insights, LatencySummary, PromptStats, TokenTotals, ToolOutcome, ToolSpan, ToolStats,
};
pub use observed_memory::ObservedMemory;
pub use query::{EventFilter, EventQuery};
#[cfg(feature = "openai-responses")]
pub use responses_extract::{HostedToolCall, emit_hosted_tools, extract_hosted_tools};
#[cfg(all(feature = "openai-responses-websocket", not(target_family = "wasm")))]
pub use responses_session::{ObservedResponsesSession, ResponsesSessionObserver};
pub use sampling::{AlwaysSample, RatePolicy, SamplingPolicy};
#[cfg(feature = "subscriber")]
pub use subscriber::CapturingLayer;
