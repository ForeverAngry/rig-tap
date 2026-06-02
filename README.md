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

## Why rig-tap (vs. what Rig gives you today)

Rig already exposes the raw *callbacks* and *data*: `PromptHook`
(`on_completion_call` / `on_completion_response` / `on_tool_call` /
`on_tool_result`), the `Usage` token counts on `CompletionResponse`, typed
`PromptError` / `CompletionError` values, and GenAI span conventions
(`gen_ai.usage.input_tokens`, etc.). That is a callback surface scoped to one
agent loop. It is ephemeral, provider-shaped, and has no on-the-wire form —
nothing leaves the process, correlates across calls, or speaks a vocabulary
that other crates share unless you write that glue yourself.

`rig-tap` turns those callbacks into a stable, versioned, queryable telemetry
contract. What it adds over the raw Rig surface:

- **A versioned wire schema, not just callbacks.** Every event is a flat,
  `serde`-stable `ObservabilityEvent` envelope (`version`, `tick`,
  `occurred_at_millis`, `conversation_id`, `span_id`, flattened `kind`).
  `SCHEMA_VERSION` + `#[non_exhaustive]` make additive evolution a
  non-breaking contract. Rig's hooks have no wire shape at all.
- **One vocabulary across the whole ecosystem.** The same `EventKind` covers
  agent prompts/tools *and* `rig-compose` kernel dispatch, memory/context,
  eval reports, and stateful provider sessions. `PromptHook` only sees the
  in-loop agent path — it never fires for kernel-direct dispatch or
  provider-hosted tools.
- **OTel-routable scalars without JSON parsing.** Each event surfaces
  `rig_tap.*` attributes (`model`, `tool_name`, `call_id`, `error_class`,
  `response_id`, …) as first-class `tracing` fields next to the JSON blob,
  plus `span_id` mirroring so events stitch into an existing OTel span
  waterfall. The difference between "I have a callback" and "my collector can
  index and route on it."
- **Lifecycle pairing + correlation.** A stable `call_id` pairs
  `tool.invoked` → `tool.completed` / `failed` / `skipped` / `terminated`, and
  `previous_response_id` chains stateful turns. Rig hands you two unrelated
  callbacks; `rig-tap` closes them into spans.
- **Failure semantics.** `ErrorClass` normalizes provider-shaped errors into a
  backend-agnostic taxonomy (timeout / rate_limit / auth / transport /
  validation / provider_server / cancelled / unknown) with a `retriable` flag
  and HTTP status, so SLOs and alerting build on a uniform shape instead of
  matching `CompletionError` variants per provider.
- **Things the hooks structurally can't see.** Provider-*hosted* tools
  (`web_search`, `file_search`, …) run inside the provider, so `on_tool_call`
  never fires — `rig-tap` taps the stream/session and emits `tool.hosted_*`.
  Latency milestones (`duration_ms`, `time_to_first_token_ms`) are measured
  where a producer owns both ends of a pair. Stateful Responses-WebSocket
  sessions (`response.session_*` / `response.turn_*`) have no `PromptHook`
  analog at all.
- **Operational plumbing.** Pluggable `SamplingPolicy` to downsample hot
  paths, char-boundary payload truncation, an in-process `EventQuery` layer,
  and runtime-agnostic emission (no `tokio` dependency).

In one line: Rig gives you callbacks and data scoped to one agent loop;
`rig-tap` gives you a stable, OTel-routable event contract with a single
vocabulary spanning agent + compose + memory + sessions + evals, plus failure
classification, lifecycle correlation, latency, hosted-tool visibility, and
sampling. If you only need to log one agent's tokens, Rig's hooks are enough;
the moment you want cross-crate observability you can ship to a collector and
build SLOs on, that is the gap `rig-tap` fills. It is **additive** — see
[Coexistence with `rig-core::telemetry`](#coexistence-with-rig-coretelemetry).

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
| `prompt.failed`         | `TelemetryHook::observe_prompt_error`               |
| `tool.invoked`          | `TelemetryHook::on_tool_call` / `DispatchObserveHook`       |
| `tool.completed`        | `TelemetryHook::on_tool_result` / `DispatchObserveHook`     |
| `tool.failed`           | `TelemetryHook::observe_tool_error`                 |
| `tool.skipped`          | Producer crate (kernel hook with `Skip` semantics)          |
| `tool.terminated`       | `DispatchObserveHook` (kernel gate / runtime error)         |
| `tool.hosted_invoked`   | Producer crate (Responses streaming/WebSocket tap), `responses_extract::emit_hosted_tools`, or `ObservedResponsesSession` |
| `tool.hosted_completed` | Producer crate (Responses streaming/WebSocket tap), `responses_extract::emit_hosted_tools`, or `ObservedResponsesSession` |
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
| `response.session_started` | `ObservedResponsesSession` (`openai-responses-websocket`) |
| `response.turn_started`    | `ObservedResponsesSession` (`openai-responses-websocket`) |
| `response.turn_completed`  | `ObservedResponsesSession` (`openai-responses-websocket`) |
| `response.session_ended`   | `ObservedResponsesSession` (`openai-responses-websocket`) |
| `eval.report`              | Producer crate (`rig-retrieval-evals` `MultiReport` / `ReportDiff`) |

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

## OpenAI Responses WebSocket sessions

The `openai-responses-websocket` feature (forwards `rig/websocket`, non-WASM
only) wires `rig-core`'s
`rig::providers::openai::responses_api::websocket::ResponsesWebSocketSession`
into the schema:

```rust,no_run
use rig::client::CompletionClient;
use rig::providers::openai;
use rig_tap::ObservedResponsesSession;

# async fn run() -> Result<(), Box<dyn std::error::Error>> {
let client = openai::Client::new("YOUR_API_KEY")?;
let session = client.responses_websocket(openai::GPT_5_2).await?;
let mut observed = ObservedResponsesSession::new(
    session,
    "conversation-1",        // conversation_id stamped on every envelope
    openai::GPT_5_2,         // model id, recorded once on session_started
    "ws-session-abc",        // stable session correlator
);
// observed.send(request).await?;
// while let Ok(event) = observed.next_event().await { /* ... */ }
// observed.close().await?;
# Ok(()) }
```

The decorator emits, in order, one `response.session_started`, an
alternating `response.turn_started` / `response.turn_completed` pair per
turn, paired `tool.hosted_invoked` / `tool.hosted_completed` events for
every hosted tool call extracted from the raw `Done.response` payload
(`web_search`, `file_search`, `computer_use`, `code_interpreter`, and any
future `*_call` kind), and exactly one `response.session_ended` on close.

Turn finalization is lazy: when callers stop reading after the terminal
`Response` chunk (as `ResponsesWebSocketSession::completion` does
upstream) the open turn is closed on the next `send`, on `close`, or on
`into_inner` — so the envelope stays well-formed even when the caller
short-circuits before `Done`. For raw HTTP / streaming hosted-tool
extraction without the WebSocket session, see
`responses_extract::{extract_hosted_tools, emit_hosted_tools}` under the
lighter `openai-responses` feature.

## Sampling controls

`TelemetryHook` accepts a [`SamplingPolicy`] so high-volume `tool.*`
traffic can be downsampled without losing low-volume lifecycle events
such as `prompt.*` or `memory.*`. The default policy is `AlwaysSample`
(keep everything); the bundled `RatePolicy` is a deterministic per-kind
rate sampler:

```rust,no_run
use std::sync::Arc;
use rig_tap::{RatePolicy, TelemetryHook, TelemetryHookConfig};

# fn make_hook<M: rig::completion::CompletionModel>() -> TelemetryHook<M> {
TelemetryHook::new(TelemetryHookConfig::new("gpt-4o", "thread-1"))
    .with_sampling_policy(Arc::new(
        RatePolicy::new()
            .with_rate("tool.invoked", 0.1)
            .with_rate("tool.completed", 0.1),
        // `prompt.*`, `memory.*`, `compose.*` keep their default
        // rate of 1.0 and are emitted unchanged.
    ))
# }
```

Sampling decisions are deterministic: the policy hashes a per-event
correlator with a fixed seed. The hook passes the resolved conversation
id on `prompt.*` events and the internal call id on `tool.*` events, so
a `tool.invoked` and its matching `tool.completed` either both ship or
are both dropped — pairs stay coherent.

Custom policies (allowlists, error-only, tail-based) can implement
`SamplingPolicy::should_sample(kind, correlator)` and be plugged in via
`with_sampling_policy(Arc::new(...))`.

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

## OpenTelemetry exporter recipe

`rig-tap` events are emitted as `tracing::info!` records on the `rig_tap`
target with structured fields whose names are already valid OpenTelemetry
attribute keys — **no rename or JSON transform is required to ship them
through an OTel pipeline**. Wire `tracing-opentelemetry` into your existing
subscriber stack and the attributes flow through verbatim.

### Stable attribute keys

Every event carries the envelope scalars:

| Attribute | Source | Notes |
|-----------|--------|-------|
| `rig_tap.version` | `ObservabilityEvent::version` | schema version (currently `1`) |
| `rig_tap.kind`    | `ObservabilityEvent::kind.discriminant()` | e.g. `prompt.completed`, `eval.report` |
| `rig_tap.conversation_id` | envelope | join key for multi-event traces |
| `rig_tap.tick`    | envelope | monotonic per-process sequence |
| `rig_tap.occurred_at_millis` | envelope | UNIX epoch milliseconds |
| `rig_tap.span_id` | envelope | numeric id of the `tracing::Span` that was current when the event was emitted (`0` = absent); also serialized into the JSON envelope as `span_id` when present |

Plus per-variant correlators, populated when present and emitted as empty
strings otherwise (filter `field != ""` collector-side):

| Attribute | Populated for |
|-----------|---------------|
| `rig_tap.kernel_id` | `compose.*` |
| `rig_tap.tool_name` | `tool.*` |
| `rig_tap.call_id`   | `tool.*` |
| `rig_tap.skill_id`  | `compose.skill_resolved` / `compose.loop_iteration` |
| `rig_tap.model`     | `prompt.*`, `response.session_started` |
| `rig_tap.response_id` | `prompt.completed`, `response.turn_*` |
| `rig_tap.previous_response_id` | stateful Responses-API turns |
| `rig_tap.dataset`   | `eval.report` |
| `rig_tap.metric`    | `eval.report` |
| `rig_tap.verdict`   | `eval.report` |

The full JSON envelope (including non-scalar fields like `args_json`,
`tokens_in`, `ci_low`/`ci_high`, etc.) ships as the `event` attribute.

### Minimum-viable collector config

If you forward `tracing` records to an out-of-process OpenTelemetry
Collector, the only processor you need is `filter` (to scope to the
`rig_tap` target) plus optional `attributes` (to rename keys to your
backend's preferred taxonomy). Example fragment:

```yaml
processors:
  filter/rig_tap:
    logs:
      include:
        match_type: strict
        record_attributes:
          - key: target
            value: rig_tap
  attributes/rig_tap:
    actions:
      # Optional: align with OTel GenAI semconv where it overlaps.
      - key: gen_ai.response.model
        from_attribute: rig_tap.model
        action: insert
      - key: gen_ai.response.id
        from_attribute: rig_tap.response_id
        action: insert
```

### Runnable preview

The [`otel_exporter_recipe`](examples/otel_exporter_recipe.rs) example
emits one event per major family and prints the exact attribute set an
OTel pipeline would receive:

```bash
cargo run --example otel_exporter_recipe --features subscriber
```

### In-process tracing-opentelemetry

For in-process exporters, drop the OTel layer into the same subscriber
stack as any other `tracing-opentelemetry` user:

```rust,no_run
# #[cfg(false)]
# {
use tracing_subscriber::{EnvFilter, prelude::*};

let tracer = /* your `opentelemetry_otlp::new_pipeline()...install_simple()?` */;
let otel = tracing_opentelemetry::layer().with_tracer(tracer);

tracing_subscriber::registry()
    .with(EnvFilter::new("rig_tap=info,rig=info"))
    .with(otel)
    .init();
# }
```

No `rig-tap`-specific configuration is required — the `rig_tap.*` fields
are propagated as span attributes automatically.

## Status

Crate version: `0.2.1`. Rust edition: 2024. MSRV: 1.89. The library is
runtime-agnostic and emits through `tracing`; production consumers should use a
non-blocking tracing sink when exporting events off-host. The optional
`subscriber` feature is for tests/examples, while the optional `compose`
feature adds the `rig-compose` dispatch tap.

## License

Licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT)
at your option.
