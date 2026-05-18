# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - Unreleased

### Added

- `ObservabilityEvent` v1 schema with ten event kinds covering the
  prompt / tool / context / memory lifecycle.
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
  `tool.completed`, and `tool.terminated` from kernel-direct dispatch.
