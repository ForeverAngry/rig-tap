# rig-tap

**Emits Uniform Telemetry** for [Rig](https://crates.io/crates/rig-core)
agents and companion crates. `rig-tap` defines one stable, versioned
`ObservabilityEvent` stream for prompts, tools, memory, context, and dispatch,
then emits it through `tracing` so any consumer (OpenTelemetry collectors,
Phoenix, Langfuse, a custom dashboard, or a log shipper) can subscribe without
crate-specific glue.

This crate is the telemetry contract, not a UI. It gives every producer in the
Rig ecosystem the same event vocabulary, whether the event came from a Rig
agent hook, `rig-compose` dispatch, `rig-memvid` memory behavior, or a host
application.

## Emits Uniform Telemetry

`rig-tap` exists to make ecosystem telemetry look the same at the boundary:

- **One schema** — every event is an `ObservabilityEvent` envelope with a stable
  `kind`, schema `version`, timestamp, monotonic `tick`, and `conversation_id`.
- **One transport** — events are emitted under the dedicated `rig_tap` tracing
  target, so existing `tracing`, JSON log, and OpenTelemetry pipelines can keep
  their normal setup.
- **One lifecycle vocabulary** — prompt, tool, context, memory, and
  `rig-compose` kernel-loop events share names and correlation fields across
  crates.
- **One collector shape** — each event includes the full JSON envelope plus
  scalar `rig_tap.*` attributes for collector-side routing and indexing.

Use `rig-tap` when you want memory crates, model metadata hooks, orchestration
kernels, and application code to speak the same telemetry language.

## What it provides

- **`ObservabilityEvent` + `EventKind`** — the wire schema. Event kinds cover
  prompt / tool / context / memory plus `rig-compose` kernel and loop
  lifecycle events, all emitted with the same envelope shape.
- **`TelemetryHook<M>`** — implements `rig::agent::PromptHook<M>` and emits
  `prompt.*` and `tool.*` events from the five `PromptHook` lifecycle methods.
- **`DispatchObserveHook`** (feature `compose`) — implements
  `rig_compose::ToolDispatchHook` and emits `tool.invoked` /
  `tool.completed` / `tool.skipped` / `tool.terminated` from the kernel-direct
  dispatch path. It also implements `rig_compose::AgentLifecycleHook` and emits
  `compose.*` events around `GenericAgent` step and skill execution.
- **`ObservedMemory<M>`** — decorator that wraps any `rig::memory::ConversationMemory`
  and emits `context.sampled` on every `load`.
- **`EventQuery` + `EventFilter`** — in-process query helpers for captured
  `ObservabilityEvent` snapshots. Useful for tests, demos, and small local
  dashboards without adding a service runtime.
- **`ChainedHook<A, B>`** — compose two `PromptHook`s on a single agent (e.g.
  pair `MemvidPersistHook` with `TelemetryHook`).

## Quick start

Wire a `tracing` subscriber that keeps the dedicated `rig_tap` target, then
attach the hooks at the lifecycle boundary you want to observe:

```rust,no_run
use rig_tap::{ObservedMemory, TelemetryHook};
use tracing_subscriber::{EnvFilter, prelude::*};

fn install_observe_sink() {
    tracing_subscriber::registry()
        .with(EnvFilter::new("rig_tap=info"))
        .with(tracing_subscriber::fmt::layer().json())
        .init();
}

# fn build<M: rig::completion::CompletionModel>() -> TelemetryHook<M> {
let hook = TelemetryHook::<M>::with_defaults("qwen3.5:9b", "thread-1");
let memory = ObservedMemory::new(rig::memory::InMemoryConversationMemory::new());

// Attach `hook` to a Rig agent and use `memory` anywhere a
// `ConversationMemory` implementation is accepted.
# let _ = memory;
# hook }
```

For kernel-direct tool dispatch, enable the `compose` feature and register
`DispatchObserveHook` with `dispatch_tool_invocations_with_hooks`. The same hook
can be passed to `GenericAgentBuilder::with_lifecycle_hook` to observe the
agent step and skill loop. For deterministic tests or examples, enable
`subscriber` and use `CapturingLayer` to collect typed `ObservabilityEvent`
values in-process, then call `capture.query().filter(&EventFilter::new().kind("tool.completed"))`
to inspect a bounded snapshot.

## Architecture

`rig-tap` acts as a tap, listening to various hooks in the Rig lifecycle and
writing uniform telemetry into the `tracing` ecosystem under a dedicated
`rig_tap` target. Producer crates do not need to agree on storage backends,
model providers, or orchestration strategy; they only need to emit the shared
event vocabulary.

```text
┌─────────────────┐       ┌─────────────────┐       ┌─────────────────┐
│                 │       │                 │       │                 │
│   Host Agent    │──────►│  TelemetryHook  ├──────►│                 │
│ (rig::pipeline) │       │                 │       │                 │
└─────────────────┘       └─────────────────┘       │                 │
                                                    │                 │
┌─────────────────┐       ┌─────────────────┐       │ tracing::info!  │
│                 │       │                 │       │  (target:       │
│  Host Runtime   │──────►│DispatchObserve..├──────►│  "rig_tap") │
│  (rig_compose)  │       │                 │       │                 │
└─────────────────┘       └─────────────────┘       │                 │
                                                    │                 │
┌─────────────────┐       ┌─────────────────┐       │                 │
│                 │       │                 │       │                 │
│ ConversationMem ├──────►│ ObservedMemory  ├──────►│                 │
│  (rig::memory)  │       │                 │       └────────┬────────┘
└─────────────────┘       └─────────────────┘                │
                                                             ▼
                                                    ┌─────────────────┐
                                                    │                 │
                                                    │  Telemetry Sink │
                                                    │ (OTEL/Langfuse/ │
                                                    │  Phoenix/etc.)  │
                                                    └─────────────────┘
```

## Uniform Wire Format

All events are flat JSON serialized via `tracing::info!(target: "rig_tap", event = %json, ...)`.
The `event` field carries the complete envelope, and scalar `rig_tap.*`
attributes expose the fields collectors most often need for routing:

```json
{
  "version": 1,
  "occurred_at_millis": 1715000000000,
  "tick": 42,
  "conversation_id": "thread-1",
  "kind": "context.compacted",
  "evicted_count": 8,
  "evicted_bytes": 4096,
  "carry_over": true,
  "summary_bytes": 512
}
```

`tick` is a monotonic per-process counter so consumers can order events
without clock skew. `version` is the schema version (currently `1`).

When exported through `tracing-opentelemetry`, an OpenTelemetry collector can
filter on `rig_tap.kind = "tool.skipped"`, group by
`rig_tap.conversation_id`, or route all `rig_tap` target events without parsing
the JSON body.

## Event kinds (v1)

| `kind`                  | Producer                                                    |
|-------------------------|-------------------------------------------------------------|
| `prompt.started`        | `TelemetryHook::on_completion_call`                         |
| `prompt.completed`      | `TelemetryHook::on_completion_response`                     |
| `tool.invoked`          | `TelemetryHook::on_tool_call` / `DispatchObserveHook`       |
| `tool.completed`        | `TelemetryHook::on_tool_result` / `DispatchObserveHook`     |
| `tool.skipped`          | Producer crate (kernel hook with `Skip` semantics)          |
| `tool.terminated`       | `DispatchObserveHook` (kernel gate / runtime error)         |
| `context.sampled`       | `ObservedMemory::load`                                      |
| `context.compacted`     | Producer crate (e.g. `rig-memvid`)                          |
| `memory.demoted`        | Producer crate                                              |
| `memory.frame_written`  | Producer crate                                              |
| `compose.kernel_start`  | Producer crate (`rig-compose` kernel lifecycle)             |
| `compose.kernel_shutdown` | Producer crate (`rig-compose` kernel lifecycle)           |
| `compose.loop_iteration` | Producer crate (`rig-compose` agent loop)                  |
| `compose.skill_resolved` | Producer crate (`rig-compose` skill resolution)            |
| `compose.retry_attempt` | Producer crate (`rig-compose` retry path)                   |
| `compose.recovery`      | Producer crate (`rig-compose` recovery path)                |

`prompt.*`, `tool.*` (via `TelemetryHook`/`DispatchObserveHook`), and
`context.sampled` are emitted by this crate. The remaining `tool.*` and
`memory.*` / `context.compacted` / `compose.*` events are emitted by producer
crates (e.g. `rig-memvid` and `rig-compose`) using the same schema — construct
an `EventKind` variant and pass it through
`ObservabilityEvent::new(conversation_id, kind)` or the `emit_kind` helper.

## Consumer example

```rust,no_run
use tracing_subscriber::{EnvFilter, prelude::*};

fn main() {
    tracing_subscriber::registry()
        .with(EnvFilter::new("rig_tap=info"))
        .with(tracing_subscriber::fmt::layer().json())
        .init();

    // ... build agent with `TelemetryHook` and `ObservedMemory<...>` ...
}
```

A consumer wanting typed events can attach a custom `tracing_subscriber::Layer`
that parses the `event` field via `serde_json::from_str::<ObservabilityEvent>`.
For in-process tests, demos, or local dashboards, the optional `subscriber`
feature exposes `CapturingLayer::query()` and the default-build
`EventQuery`/`EventFilter` helpers for filtering by conversation, kind, tick
range, and scalar correlators such as tool name, call ID, skill ID, kernel ID,
or model.

## Coexistence with `rig-core::telemetry`

This crate is additive to Rig's existing GenAI span conventions
(`gen_ai.input.messages`, `gen_ai.usage.input_tokens`, etc.). Consumers using
`tracing-opentelemetry` for Phoenix / Langfuse keep their existing setup;
`rig_tap` events live under a separate target and can be filtered
independently. OpenTelemetry collectors receive the full JSON envelope in the
`event` attribute plus stable scalar attributes (`rig_tap.kind`,
`rig_tap.conversation_id`, `rig_tap.version`, `rig_tap.tick`, and
`rig_tap.occurred_at_millis`) for routing, filtering, and indexing without a
collector-side JSON transform.

## Status

Crate version: `0.1.0`. Rust edition: 2024. MSRV: 1.89. The library is
runtime-agnostic and emits through `tracing`; production consumers should use a
non-blocking tracing sink when exporting events off-host. The optional
`subscriber` feature is for tests/examples, while the optional `compose`
feature adds the `rig-compose` dispatch tap.

## License

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT)
at your option.
