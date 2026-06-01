# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.2](https://github.com/ForeverAngry/rig-tap/compare/v0.2.1...v0.2.2) - 2026-06-01

### Documentation

- Align README Status block with shipped 0.2.1 ([#11](https://github.com/ForeverAngry/rig-tap/pull/11))

### Fixed

- Keep rate sampling pair coherent ([#13](https://github.com/ForeverAngry/rig-tap/pull/13))

## [0.2.1](https://github.com/ForeverAngry/rig-tap/compare/v0.2.0...v0.2.1) - 2026-05-28

### Documentation

- Bump documented crate version 0.1.0 -> 0.2.0 ([#9](https://github.com/ForeverAngry/rig-tap/pull/9))

## [0.2.0](https://github.com/ForeverAngry/rig-tap/compare/v0.1.2...v0.2.0) - 2026-05-28

### Added

- **Span correlation** — `ObservabilityEvent` gains an optional
  `span_id: Option<u64>` field, auto-populated by `build_event` /
  `emit_kind` from `tracing::Span::current().id()`. Surfaced as a
  `rig_tap.span_id` tracing field (with `0` as the absent sentinel)
  and serialized into the JSON envelope as `span_id` when present
  (`skip_serializing_if = "Option::is_none"`, `#[serde(default)]` for
  legacy envelopes). Lets collectors stitch `rig-tap` events into an
  existing `tracing-opentelemetry` span waterfall without
  conversation-id post-processing. Additive — does not bump
  `SCHEMA_VERSION`. New helper `current_span_id()` exposes the same
  value for downstream producers.

- **`SamplingPolicy` trait + `RatePolicy` + `AlwaysSample`** — new
  per-event-kind sampling layer for `TelemetryHook`. The default policy
  (`AlwaysSample`) preserves the existing behaviour; callers can opt into
  per-kind downsampling via
  `TelemetryHook::with_sampling_policy(Arc::new(RatePolicy::new()...))`.
  Decisions are deterministic (fixed-seed hash of a per-event
  correlator) so `tool.invoked` / `tool.completed` pairs stay coherent
  — either both ship or both drop. `prompt.*` correlator =
  conversation id; `tool.*` correlator = internal call id.
  Documented in the README under "Sampling controls".

- **OpenTelemetry exporter recipe** — new README section documents the
  stable `rig_tap.*` attribute keys, a minimum-viable OTel Collector
  config (filter + attributes processors with optional GenAI semconv
  aliases), and the in-process `tracing-opentelemetry` wiring. No
  collector-side JSON transform is required: the `rig_tap.*` tracing
  fields are already valid OTel attribute names.
- `examples/otel_exporter_recipe.rs` (gated on `subscriber`) — emits one
  event per major family (`prompt.completed`, `tool.invoked`,
  `eval.report`) and prints the exact OTel-ready attribute set a
  collector would extract from the structured `tracing` fields, plus
  the full JSON envelope. Self-contained: pulls no `opentelemetry-*`
  dependency.

- `EventKind::EvalReport` — new `eval.report` schema variant carrying a
  single retrieval/RAG metric (`report_id`, `dataset`, `metric`, `value`,
  optional bootstrap CI `ci_low`/`ci_high`, optional baseline diff
  `baseline_value`/`delta`/`verdict`, optional `sample_size`). Producers
  emit one event per `(report_id, dataset, metric)` triple so collectors
  can filter and aggregate without parsing the envelope. Pairs with
  `rig-retrieval-evals` `MultiReport` / `ReportDiff` summaries but the
  variant is producer-agnostic. Additive — does not bump `SCHEMA_VERSION`.
- `ScalarFields` gains `dataset`, `metric`, and `verdict` columns,
  surfaced on every emitted `eval.report` event as `rig_tap.dataset`,
  `rig_tap.metric`, and `rig_tap.verdict` tracing fields for OTel
  attribute extraction.
- `EventKind::is_eval_related()` classifier.

- `examples/observe_responses_api.rs` — env-driven runnable example
  (`OPENAI_API_KEY` + optional `RIG_TAP_PROMPT` / `RIG_TAP_MODEL` /
  `RIG_TAP_SESSION_ID`) that wires `CapturingLayer` to
  `ObservedResponsesSession`, drives one turn against the live OpenAI
  Responses WebSocket endpoint, and prints the captured envelope
  sequence. Exits cleanly when `OPENAI_API_KEY` is unset so it stays
  CI-runnable.
- `tests/responses_session.rs` — fixture-driven integration test
  asserting the multi-turn lifecycle, hosted-tool extraction from raw
  `ResponsesWebSocketDoneEvent.response` payloads, error-path
  finalization, and full schema-v1 JSON round-trip.

- `openai-responses-websocket` cargo feature (off by default; forwards
  `rig/websocket`) gating a new `responses_session` module on
  non-WASM targets:
  - `ResponsesSessionObserver` — stateless state machine that ingests
    `send` / `next_event` / `close` signals and emits
    `response.session_started`, `response.turn_started`,
    `response.turn_completed`, `response.session_ended`,
    `tool.hosted_invoked`, and `tool.hosted_completed` envelopes.
  - `ObservedResponsesSession<H>` — drop-in decorator wrapping
    `rig::providers::openai::responses_api::websocket::ResponsesWebSocketSession`.
    Forwards `send` / `send_with_options` / `next_event` / `close` and
    drives the observer on each call. Lazy-finalizes the active turn
    on the next `send`, on `close`, or on `into_inner` if the caller
    stops short of a `Done` event so partial turns still emit a
    `turn_completed` envelope.
  - Hosted-tool extraction runs automatically on the raw
    `ResponsesWebSocketDoneEvent.response` payload via
    `extract_hosted_tools`, sidestepping `rig-core`'s
    `#[serde(other)] Unknown` discard path for typed `Output` items.

- `openai-responses` cargo feature (off by default, no extra runtime
  dependency) gating a new `responses_extract` module:
  - `extract_hosted_tools(payload: &serde_json::Value) -> Vec<HostedToolCall>`
    walks the `output[]` array of a Responses-API JSON payload and
    returns every hosted-tool invocation it finds (`web_search`,
    `file_search`, `computer_use`, `code_interpreter`, and any future
    `*_call` kind). Function-tool items are skipped so they don't
    double-emit on top of `tool.invoked`.
  - `emit_hosted_tools(conversation_id, response_id, payload) -> usize`
    convenience helper that emits a `tool.hosted_invoked` /
    `tool.hosted_completed` pair per call directly through `tracing`.
  - `HostedToolCall` struct (`#[non_exhaustive]`) exposing
    `tool_name`, `provider_call_id`, `call_id`, `status`, compact
    `args_json` / `result_json` summaries, the raw `Value`, and the
    `output_index`.

  The extractor operates on raw JSON because `rig-core`'s typed
  `responses_api::Output` enum carries `#[serde(other)] Unknown` and
  silently discards hosted-tool payloads at deserialization. Feed it
  the `ResponsesWebSocketDoneEvent.response` value (raw `serde_json::Value`,
  never typed-deserialized) or any pre-rig-core HTTP body.

- Schema v1.1 (additive) for OpenAI Responses-style stateful endpoints:
  - `EventKind::PromptCompleted` now carries an optional
    `previous_response_id` so consumers can reconstruct the
    server-side turn chain without joining a separate trace.
  - `EventKind::ToolHostedInvoked` / `EventKind::ToolHostedCompleted` for
    provider-native hosted tools (`web_search`, `file_search`,
    `computer_use`, `code_interpreter`) that never fire
    `PromptHook::on_tool_call`. Producers wire these from a streaming-
    chunk tap or session decorator.
  - `EventKind::ResponseSessionStarted` / `ResponseTurnStarted` /
    `ResponseTurnCompleted` / `ResponseSessionEnded` for stateful
    WebSocket-mode sessions. Each turn carries the chain ancestor and an
    observed hosted-tool-call count.
  - `EventKind::is_response_lifecycle_related()` classifier.
  - `ScalarFields::response_id` and `ScalarFields::previous_response_id`
    surface as `rig_tap.response_id` / `rig_tap.previous_response_id`
    on every emitted `tracing` event so collectors can route on them
    without parsing the JSON envelope.
- `TelemetryHook::with_previous_response_id_resolver` — per-request
  escape hatch that stamps the chain ancestor on `prompt.completed` from
  caller-tracked state (task-local, span field, session object).
  Public `PreviousResponseIdResolver<R>` alias re-exported from the
  crate root.

### Changed

- `ScalarFields` is now `#[non_exhaustive]` so future schema-additive
  releases can append new scalar correlators without a breaking change.
  Existing reads (`fields.tool_name`, `fields.kernel_id`, …) are
  unaffected; construct values via
  `ScalarFields { tool_name, ..Default::default() }` rather than the
  full struct literal.

## [0.1.2](https://github.com/ForeverAngry/rig-tap/compare/v0.1.1...v0.1.2) - 2026-05-27

### Added

- Add query helpers for captured observability events.

## [0.1.1](https://github.com/ForeverAngry/rig-tap/compare/v0.1.0...v0.1.1) - 2026-05-27

### Added

- Emit compose lifecycle telemetry

## [0.1.0] - Unreleased

### Fixed

- `DispatchObserveHook` now emits `tool.skipped` for `rig-compose` synthetic
  skip outcomes instead of reporting them as `tool.completed`.
- `rig_tap` tracing events now include stable scalar `rig_tap.*` fields next
  to the JSON envelope so OpenTelemetry collectors can route and index them
  without parsing the `event` JSON string.

### Added

- `ObservabilityEvent` v1 schema covering the prompt / tool / context /
  memory lifecycle plus additive `compose.*` kernel-loop event kinds for
  kernel start/shutdown, loop iteration, skill resolution, retry attempts,
  and recovery paths.
- `TelemetryHook<M>: PromptHook<M>` that emits `prompt.*` and `tool.*`
  events.
- `TelemetryHook::with_conversation_id_resolver` — per-request escape
  hatch that wins over `TelemetryHookConfig::conversation_id` when the
  resolver returns `Some(id)`. Wire it to a task-local or span field
  while waiting on upstream `PromptHook` per-request context.
- `TelemetryHook::with_model_resolver` — extracts the concrete model
  identifier from each `CompletionResponse<M::Response>` for routed
  providers (OpenRouter, Bedrock routing, vendor multi-model
  endpoints) where the configured model name is a logical alias.
- `ObservedMemory<M>: ConversationMemory` decorator that samples context
  size on every `load`.
- `ChainedHook<A, B>` combinator for composing two `PromptHook`s on a
  single agent. Opt in to synthetic `tool.skipped` / `tool.terminated`
  emissions via `ChainedHook::observe_with`.
- `emit` helper that serializes an event under
  `tracing::info!(target = "rig_tap", event = %json)`.
- Optional `subscriber` feature exposing `CapturingLayer`, a public
  `tracing_subscriber::Layer` that buffers decoded
  `ObservabilityEvent`s in memory. Off by default; intended for tests,
  examples, and small in-process tools.
- Optional `compose` feature exposing `DispatchObserveHook`, a
  `rig_compose::ToolDispatchHook` adapter that emits `tool.invoked`,
  `tool.completed`, and `tool.terminated` from kernel-direct dispatch. The
  same adapter now implements `rig_compose::AgentLifecycleHook` and emits
  `compose.*` kernel-loop events from `GenericAgent` step execution.
