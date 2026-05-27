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

## Next Work

1. **Schema v1.1 — additive event fields** — promote frequently-requested
   fields (e.g. `prompt.input_tokens`, `tool.attempt_index`,
   `memory.candidate_count`) into the typed envelope so collectors stop
   pulling them out of the JSON `extra` bag. All additions stay
   backward-compatible; `version` bumps minor.
2. **`MetricsEvent` kind for evaluation reports** — wire a new
   `eval.report` event kind so `rig-retrieval-evals` `MultiReport` / `ReportDiff`
   summaries (including the new `MetricCi` bootstrap intervals and
   regression-gate verdicts) ride the same tracing target as runtime
   telemetry. Coordinated with rig-retrieval-evals item #9.
3. **OpenTelemetry exporter recipe** — document the minimum viable
   collector config (attribute mapping from `rig_tap.*` scalars to
   OTel resource/attribute names) and ship a `no_run` example. No new
   dep; this is documentation + a doctest.
4. **Sampling controls** — per-event-kind sampling on `TelemetryHook` so
   high-volume `tool.*` traffic can be downsampled while keeping
   `prompt.*` and `memory.*` fully observed. Implement as a
   `SamplingPolicy` trait with a deterministic default.
5. **Span correlation** — emit `tracing` span ids on every event so
   collectors that already correlate by span (Tempo, Honeycomb) can
   stitch `rig-tap` events into the existing waterfall without
   conversation-id post-processing.

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
