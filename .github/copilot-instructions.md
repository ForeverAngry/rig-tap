# rig-observe — Copilot Instructions

See [AGENTS.md](../AGENTS.md) for the authoritative copy. Summary:

- Rust 2024, MSRV `1.89`. Library is runtime-agnostic — do not add
  `tokio` to `[dependencies]`.
- Never `.await` while holding a `Mutex`/`RwLock` guard
  (`clippy::await_holding_lock` is `deny`).
- No `unwrap`/`expect`/`panic!`/`todo!`/`unimplemented!`/`dbg!`/indexing
  in library code (clippy `deny`/`forbid`). Use `?`,
  `ok_or(…)`, `get(..)`, pattern matching.
- `unwrap`/`expect` allowed in `tests/`, `examples/`, and `#[cfg(test)]`
  blocks (gate the test module with
  `#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]`).
- Write structured JSON payloads to `tracing`, but no plain `println!` in library code.
- Document new `pub` items with `///` rustdoc. Re-export from
  [src/lib.rs](../src/lib.rs).

## Validation

```sh
just check
# fmt --check + clippy (× feature combos) + cargo test --all-features
```

Run before declaring any change done.

## Scope

The crate must remain runtime-agnostic. Do not depend heavily on other companion crates except `rig-compose` behind the `compose` feature.
