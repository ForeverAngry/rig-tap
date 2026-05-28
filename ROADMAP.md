# Roadmap

`rig-tap` is the uniform telemetry schema and emission layer for the
Rig ecosystem. This roadmap tracks what has shipped, what is queued,
and what is deliberately out of scope. For day-to-day conventions see
[AGENTS.md](AGENTS.md).

## Landed

- `ObservabilityEvent` v1 schema spanning the prompt / tool / context /
  memory lifecycle plus `compose.*` kernel-loop lifecycle events, a stable
  monotonic `tick`, and `conversation_id` correlation.
- Stable scalar `rig_tap.*` fields emitted alongside the JSON envelope
  on every `tracing` event so OpenTelemetry collectors can route and
  index without parsing the embedded JSON string.
- `TelemetryHook<M>: PromptHook<M>` emitting `prompt.*` and `tool.*`
  events from any Rig agent.
- `TelemetryHook::with_conversation_id_resolver` — per-request escape
  hatch beating `TelemetryHookConfig::conversation_id` when the resolver
  returns `Some(id)`.
- `TelemetryHook::with_model_resolver` — extracts the concrete model id
  from `CompletionResponse<M::Response>` for routed providers
  (OpenRouter, Bedrock routing, multi-model endpoints).
- `ObservedMemory<M>: ConversationMemory` decorator sampling context
  size on every `load`.
- `ChainedHook<A, B>` combinator with opt-in synthetic
  `tool.skipped` / `tool.terminated` emissions via
  `ChainedHook::observe_with`.
- `DispatchObserveHook` correctly emits `tool.skipped` for `rig-compose`
  synthetic skip outcomes instead of mislabeling them as `tool.completed`.
- `emit` helper that serializes events under
  `tracing::info!(target = "rig_tap", event = %json)`.
- `subscriber` feature: public `CapturingLayer` `tracing_subscriber::Layer`
  buffering decoded events for tests and in-process consumers.
- `compose` feature: optional integration surface for `rig-compose`
  dispatch events.
- Additive `compose.kernel_start`, `compose.kernel_shutdown`,
  `compose.loop_iteration`, `compose.skill_resolved`,
  `compose.retry_attempt`, and `compose.recovery` event kinds establish the
  schema contract for downstream `rig-compose` producer wiring and live
  inspectors.
- **Schema v1.1 — OpenAI Responses-style stateful endpoints.**
  `EventKind::PromptCompleted` carries an optional `previous_response_id`;
  new `tool.hosted_invoked` / `tool.hosted_completed` variants cover
  provider-native hosted tools (`web_search`, `file_search`,
  `computer_use`, `code_interpreter`) that bypass `PromptHook::on_tool_call`;
  new `response.session_started` / `response.turn_started` /
  `response.turn_completed` / `response.session_ended` variants cover the
  WebSocket-mode session loop. `ScalarFields` gained `response_id` and
  `previous_response_id` (now `#[non_exhaustive]` so future additions are
  non-breaking). `TelemetryHook::with_previous_response_id_resolver`
  stamps the chain ancestor on `prompt.completed` from caller-tracked
  state.
- **`openai-responses` feature — hosted-tool extractor.** New
  `responses_extract` module with `extract_hosted_tools` /
  `emit_hosted_tools` / `HostedToolCall`. Walks raw JSON because
  rig-core's typed `Output` discards hosted-tool payloads via
  `#[serde(other)]`. Pure-Rust, no extra runtime dependency.
- **`openai-responses-websocket` feature — `ObservedResponsesSession`
  decorator + `ResponsesSessionObserver` state machine.** Wraps
  `rig-core`'s `ResponsesWebSocketSession`, fires
  `response.session_started` / `response.turn_started` /
  `response.turn_completed` / `response.session_ended` automatically,
  and runs `extract_hosted_tools` on every `ResponsesWebSocketDoneEvent`
  payload. Lazy-finalizes the active turn when callers skip `Done`
  (the upstream helper returns at the terminal Response chunk), on
  `close`, or on `into_inner`. Forwards `rig/websocket`; non-WASM only.
- **OpenAI Responses example + integration tests.**
  `examples/observe_responses_api.rs` drives a live session through
  `ObservedResponsesSession` + `CapturingLayer`;
  `tests/responses_session.rs` covers the multi-turn lifecycle,
  hosted-tool extraction, the error path, and full schema-v1 JSON
  round-trip. README has an OpenAI Responses WebSocket section linking
  to the decorator.
- **`eval.report` schema variant.** `EventKind::EvalReport` carries a
  single retrieval metric (`report_id`, `dataset`, `metric`, `value`,
  optional bootstrap CI bounds, optional baseline diff + verdict,
  optional `sample_size`). `ScalarFields` gains `dataset` / `metric` /
  `verdict` columns and `emit` wires matching `rig_tap.*` tracing
  fields so collectors can filter and aggregate without parsing the
  envelope. `EventKind::is_eval_related()` classifier rounds out the
  surface. Producer wiring lives in `rig-retrieval-evals` item #9;
  this crate just hosts the schema.
- **OpenTelemetry exporter recipe.** README now documents the stable
  `rig_tap.*` attribute keys, a minimum-viable OTel Collector pipeline
  (filter + attributes processors with optional GenAI semconv
  aliases), and the in-process `tracing-opentelemetry` wiring. Ships
  with `examples/otel_exporter_recipe.rs` (gated on `subscriber`) that
  emits one event per major family and prints the exact attribute set
  an OTel pipeline would receive. No new dependency — the `rig_tap.*`
  tracing fields are already valid OTel attribute names.
- **Sampling controls.** `SamplingPolicy` trait + `AlwaysSample`
  (default) + `RatePolicy` (deterministic, fixed-seed per-kind rate
  sampler). Wired into `TelemetryHook` via
  `with_sampling_policy(Arc<dyn SamplingPolicy>)`. The hook passes the
  resolved conversation id as the correlator for `prompt.*` events and
  the internal call id for `tool.*` events so `tool.invoked` /
  `tool.completed` pairs stay coherent. Custom policies (allowlists,
  error-only, tail-based) implement one trait method.
- **Span correlation.** `ObservabilityEvent` gains an optional
  `span_id: Option<u64>` field auto-populated by `build_event` /
  `emit_kind` from `tracing::Span::current().id()`. Emitted as a
  `rig_tap.span_id` tracing attribute (with `0` as the absent
  sentinel) and serialized into the JSON envelope when present.
  Collectors using `tracing-opentelemetry` (Tempo, Honeycomb) can
  stitch `rig-tap` events into the existing waterfall without
  conversation-id post-processing. Additive — no `SCHEMA_VERSION`
  bump, and `#[serde(default)]` keeps legacy envelopes deserializable.

## Next Work

_Backlog drained. New items will be added as upstream `rig-core`,
`rig-compose`, or producer-crate work surfaces fresh observability
needs._

## Prototype Grade

- The JSON envelope is canonical; consumers parsing only the scalar
  `rig_tap.*` fields will see a strict subset. Anything not promoted to
  a scalar in a given release is still inside `event` as JSON.
- `ObservedMemory` samples context size on `load`. It does not yet
  sample on `save`; downstream consumers that need both sides should
  pair it with `TelemetryHook` until the memory-save event lands.
- `ChainedHook` is the documented composition primitive. Layering more
  than two hooks today works but is verbose — a variadic builder is on
  the v1.1 list above only if the variadic ergonomics turn out to
  matter in practice.

## Out of Scope

- Forking or vendoring `rig-core` or `rig-compose`. The `compose`
  feature pulls `rig-compose` only when explicitly enabled.
- A UI or backend. `rig-tap` produces a stable wire shape; dashboards,
  alerting, and storage belong in the host's existing observability
  stack.
- Cross-process correlation of conversation ids. That belongs in the
  host's trace context propagation (W3C `traceparent`, etc.).
- Schema breakage. New event kinds and new fields are additive; renames
  and removals are deferred to a hypothetical v2 contract with a
  parallel `ObservabilityEventV2` type.

## Reopen Triggers

- `rig-core` ships a `PromptHook` extension that exposes per-request
  context natively — retire `with_conversation_id_resolver` and
  `with_model_resolver` in favor of the upstream surface.
- An OTel semantic convention is published for LLM telemetry that
  differs from our `rig_tap.*` scalars — add an alias layer rather than
  break the existing field names.
- `rig-compose` synthetic outcomes grow new kinds beyond skip /
  terminate — `DispatchObserveHook` adds matching `tool.*` events
  additively.
