# rig-observe

Backend-agnostic observability event schema and taps for [Rig](https://crates.io/crates/rig-core)
agents. Defines a stable, versioned `ObservabilityEvent` stream emitted via
`tracing` so any consumer (Phoenix, Langfuse, a custom dashboard, a log
shipper) can subscribe without crate-specific glue.

This crate is the *contract* — it does not ship a UI. See the schema and tap
points below.

## What it provides

- **`ObservabilityEvent` + `EventKind`** — the wire schema. Ten event kinds
  covering the prompt / tool / context / memory lifecycle.
- **`TelemetryHook<M>`** — implements `rig::agent::PromptHook<M>` and emits
  `prompt.*` and `tool.*` events from the five `PromptHook` lifecycle methods.
- **`DispatchObserveHook`** (feature `compose`) — implements
  `rig_compose::ToolDispatchHook` and emits `tool.invoked` /
  `tool.completed` / `tool.terminated` from the kernel-direct dispatch path.
- **`ObservedMemory<M>`** — decorator that wraps any `rig::memory::ConversationMemory`
  and emits `context.sampled` on every `load`.
- **`ChainedHook<A, B>`** — compose two `PromptHook`s on a single agent (e.g.
  pair `MemvidPersistHook` with `TelemetryHook`).

## Wire format

All events are flat JSON serialized via `tracing::info!(target: "rig_observe", event = %json)`:

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

`prompt.*`, `tool.*` (via `TelemetryHook`/`DispatchObserveHook`), and
`context.sampled` are emitted by this crate. The remaining `tool.*` and
`memory.*` / `context.compacted` events are emitted by producer crates
(e.g. `rig-memvid`) using the same schema — construct an `EventKind`
variant and pass it through `ObservabilityEvent::new(conversation_id, kind)`
or the `emit_kind` helper.

## Consumer example

```rust,no_run
use tracing_subscriber::{EnvFilter, prelude::*};

fn main() {
    tracing_subscriber::registry()
        .with(EnvFilter::new("rig_observe=info"))
        .with(tracing_subscriber::fmt::layer().json())
        .init();

    // ... build agent with `TelemetryHook` and `ObservedMemory<...>` ...
}
```

A consumer wanting typed events can attach a custom `tracing_subscriber::Layer`
that parses the `event` field via `serde_json::from_str::<ObservabilityEvent>`.

## Coexistence with `rig-core::telemetry`

This crate is additive to Rig's existing GenAI span conventions
(`gen_ai.input.messages`, `gen_ai.usage.input_tokens`, etc.). Consumers using
`tracing-opentelemetry` for Phoenix / Langfuse keep their existing setup;
`rig_observe` events live under a separate target and can be filtered
independently.

## Status

This crate currently lives in the `rig-ecosystem` workspace as a path
dependency (consumed by `rig-memvid` behind its `observe` feature) but does
not have its own git history or `release-plz.toml` yet. A `crates.io`
publish + release-plz wiring will land before downstream crates pin a
versioned dep.

## License

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT)
at your option.
