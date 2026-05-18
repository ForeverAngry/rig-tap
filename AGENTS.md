# Context for Coding Agents (Claude, Copilot, etc.)

This crate (`rig-tap`) is part of the `rig-ecosystem` companion crates. Its purpose is to provide a backend-agnostic `tracing` event schema for observability and hooks that tap into `rig-core` and `rig-compose` lifecycles.

## Project conventions

- **Edition**: Rust 2024, MSRV `1.89`.
- **Errors**: Return typed errors where applicable, though most of this crate is error-infallible hook emission.
- **Async**: The library is runtime-agnostic. No `tokio` in `[dependencies]`.
- **Locking**: Never `.await` while holding a `Mutex` or `RwLock` guard (`clippy::await_holding_lock` is `deny`).
- **Panics**: No panics allowed in library code. `unwrap`, `expect`, `panic`, `todo`, `unimplemented`, `dbg`, and array indexing are `deny` or `forbid` via `clippy`. Use `?`, `ok_or`, pattern matching, etc.
- **Testing**: `unwrap` and `expect` are allowed in `tests/`, `examples/`, and `#[cfg(test)]` blocks if marked with `#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]`.

## Scope

The objective is to provide a clean schema (`ObservabilityEvent`, `EventKind`) and taps (`TelemetryHook`, `ObservedMemory`, `DispatchObserveHook`).
It must not depend heavily on non-core companion crates unless explicitly feature-gated (like `compose` -> `rig-compose`).
