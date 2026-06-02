# Roadmap

`rig-tap` is the uniform telemetry schema and emission layer for the
Rig ecosystem. This roadmap tracks what has shipped, what is queued,
and what is deliberately out of scope. For day-to-day conventions see
[AGENTS.md](AGENTS.md).

## Landed

- **M3 — Latency milestones:** Added optional `duration_ms` to `prompt.completed`, `tool.completed`, `tool.hosted_completed`, and `response.turn_completed`, plus `time_to_first_token_ms` to `prompt.completed`. All additive + `skip_serializing_if`.
- **M2 — Token economics:** Added optional `cached_tokens_in`, `reasoning_tokens`, `cost_usd`, and `finish_reason` to `prompt.completed` schema, drawing directly from rig's `Usage` metrics.
- **M1 — Failure family:** Added `prompt.failed` and `tool.failed` invariants to `EventKind`, mapped to `TelemetryHook::observe_prompt_error` and `TelemetryHook::observe_tool_error`.
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

Guiding principle: `rig-tap`'s value is a **small, stable, actually-emitted**
vocabulary. The schema already carries producer-only kinds (`compose.*`,
`eval.report`, `memory.*`) that this crate never fires; piling on more
speculative variants makes the contract aspirational rather than real.
The items below are prioritised so that what lands first is what
`rig-tap`'s own hooks (`TelemetryHook`, `ObservedMemory`) can emit and
what every analytics / debugging workflow actually needs. All additions
are additive — new optional fields and new `#[non_exhaustive]` variants,
no `SCHEMA_VERSION` bump.

### Tier 1 — committed (real blind spots, emittable by this crate)

- **Failure family.** New `prompt.failed` / `tool.failed` kinds plus a
  stable `ErrorClass` enum (`Timeout | RateLimit | Auth | Transport |
  Validation | ProviderServer | Cancelled | Unknown`). Today every
  lifecycle pair models only the happy path and errors disappear into
  free-text `result` strings, so error-budget / SLO dashboards are
  impossible without log-grepping. `TelemetryHook` can emit these from
  the `PromptHook` error surface; producers reuse the same kinds.
  `ErrorClass::RateLimit` subsumes a dedicated throttle signal — no
  separate `provider.throttled` kind. Add an `is_failure_related()`
  classifier mirroring `is_eval_related()`.
- **Token economics on `prompt.completed`.** Optional additive fields
  `cached_tokens_in`, `reasoning_tokens`, and producer-computed
  `cost_usd`. Every current provider (Anthropic prompt caching, OpenAI
  cached input, o-series / Gemini reasoning tokens) reports these and
  they are what budgeting and analytics actually key on — more useful
  than raw `tokens_in` / `tokens_out` alone.
- **`finish_reason` on `prompt.completed`.** Optional string
  (`stop | length | tool_calls | content_filter | error`). The single
  most-filtered field on any LLM dashboard; trivially additive.
- **Latency milestones.** Optional `duration_ms` on every `*.completed`
  / `response.turn_completed`, plus `time_to_first_token_ms` on
  `prompt.completed`. Durations are inferable today by subtracting paired
  `tick`s, but first-class fields are cheap, standard, and unlock
  streaming-UX SLOs without consumer-side join logic.

### Tier 2 — planned (structural correlators for agents / debugging)

- **Identity correlators on the envelope.** Optional `agent_id` (promoted
  to a `rig_tap.*` scalar) so multi-agent `rig-compose` swarms can
  distinguish actors; optional `trace_id` to pair with the already-shipped
  `span_id` so log-only consumers can stitch into Tempo / Honeycomb /
  Datadog without an in-process OTel layer. Both `#[serde(default,
  skip_serializing_if)]` to keep legacy envelopes deserializable.
- **`context.persisted` — memory save symmetry.** `ObservedMemory`
  samples only on `load` today (see Prototype Grade). A
  `context.persisted { message_count, byte_size }` on `save` closes the
  loop so consumers no longer need to pair with `TelemetryHook` for the
  write side.

### Tier 3 — deferred (speculative; gated on a real producer)

Held behind the Reopen Triggers below rather than speculatively baked
into the contract:

- Embedding / retrieval / rerank pipeline kinds
  (`embedding.completed`, `retrieval.queried`, `rerank.completed`). These
  tie offline `eval.report` to live traffic, but should wait until a
  retrieval producer (`rig-retrieval-evals` or similar) is ready to wire
  them — otherwise they join the pile of never-emitted variants.
- A `severity` enum on the envelope — likely redundant once the failure
  family lands; revisit only if a non-error "warn" signal (partial
  results, recovered degradation) proves it needs its own axis.
- Replay metadata on `prompt.started` (`temperature`, `top_p`,
  `max_tokens`). Useful for repro but raises payload-size and config
  surface; defer until a concrete debugging workflow asks for it.

## Action Plan

Ordered, dependency-aware execution plan for the work above. Each
milestone is independently shippable, leaves the schema additive, and
must pass `just check` (fmt + clippy across feature combos + `cargo test
--all-features`) before it is considered done. Tackle milestones
top-to-bottom; checklist items inside a milestone can be parallelised.

### M3 — Latency milestones (Tier 1)

Why third: depends on no other milestone but is lower urgency since
durations are already inferable from `tick`.

1. **Schema.** Add `duration_ms: Option<u64>` to `PromptCompleted`,
   `ToolCompleted`, `ToolHostedCompleted`, and `ResponseTurnCompleted`;
   add `time_to_first_token_ms: Option<u64>` to `PromptCompleted`. All
   optional + `skip_serializing_if`.
2. **Producer.** Where `TelemetryHook` / `ObservedResponsesSession` own
   both ends of a pair, measure with `std::time::Instant` and stamp
   `duration_ms`. Document that `time_to_first_token_ms` is populated only
   by streaming producers.
3. **Tests.** Presence-optional round-trip; assert absence keeps the
   legacy shape byte-compatible.

### M4 — Identity correlators (Tier 2)

Why fourth: envelope change (touches every event), so land after the
Tier 1 variant work is stable to minimise churn.

1. **Schema.** Add `agent_id: Option<String>` and `trace_id: Option<u64>`
   to `ObservabilityEvent` in [src/event.rs](src/event.rs), both
   `#[serde(default, skip_serializing_if = "Option::is_none")]`.
2. **Plumbing.** Thread an optional `agent_id` through `build_event` /
   `emit_kind` in [src/emit.rs](src/emit.rs) (new builder method or
   optional arg; keep the existing signatures working). Resolve `trace_id`
   the same way `current_span_id()` resolves `span_id`.
3. **Scalar wiring.** Emit `rig_tap.agent_id` (empty-string sentinel) in
   `try_emit`; keep `trace_id` JSON-only unless a collector need surfaces.
4. **Config.** Optional `agent_id` on `TelemetryHookConfig` in
   [src/hook.rs](src/hook.rs).
5. **Tests + docs.** Round-trip with and without the fields; README
   correlation section gains an `agent_id` line.

### M5 — `context.persisted` memory symmetry (Tier 2)

Why last of the committed work: smallest blast radius, closes the
documented Prototype-Grade `save`-side gap.

1. **Schema.** Add `EventKind::ContextPersisted { message_count,
   byte_size }` (`kind = "context.persisted"`); extend `discriminant()`
   and the memory branch of `is_memory_related()`.
2. **Producer.** Emit from `ObservedMemory::save` in
   [src/observed_memory.rs](src/observed_memory.rs), mirroring the
   existing `context.sampled` `load` path.
3. **Tests + docs.** `CapturingLayer` test asserting `save` emits the kind;
   README table row; flip the Prototype-Grade `load`-only caveat to
   "Landed".

### Backlog gate (Tier 3)

Do **not** start `embedding.*` / `retrieval.*` / `rerank.*`, the
`severity` enum, or replay metadata until a Reopen Trigger fires (see
below). Revisit at that point and slot into this plan as M6+.

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
- A retrieval / embedding producer (`rig-retrieval-evals` or similar) is
  ready to emit live pipeline telemetry — promote the deferred Tier 3
  `embedding.*` / `retrieval.*` / `rerank.*` kinds into the schema then,
  not before.
