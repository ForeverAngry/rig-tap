//! Sampling policies for [`TelemetryHook`](crate::TelemetryHook).
//!
//! High-volume `tool.*` traffic can dwarf `prompt.*` and `memory.*` events
//! in a busy agent. The [`SamplingPolicy`] trait lets callers downsample
//! per-event-kind without losing the lower-volume lifecycle events that
//! collectors care about most.
//!
//! The default policy ([`AlwaysSample`]) keeps every event. The bundled
//! [`RatePolicy`] downsamples deterministically via SipHash of a
//! per-event correlator so that **paired events stay coherent**: a
//! `tool.invoked` and its matching `tool.completed` share the same
//! internal call id, hash to the same bucket, and are therefore either
//! both kept or both dropped.
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//! use rig_tap::{RatePolicy, TelemetryHook, TelemetryHookConfig};
//!
//! # fn make_hook<M: rig::completion::CompletionModel>() -> TelemetryHook<M> {
//! let policy = RatePolicy::new()
//!     .with_rate("tool.invoked", 0.1)
//!     .with_rate("tool.completed", 0.1);
//!
//! TelemetryHook::new(TelemetryHookConfig::new("gpt-4o", "thread-1"))
//!     .with_sampling_policy(Arc::new(policy))
//! # }
//! ```

use std::collections::HashMap;
use std::hash::BuildHasher;

/// Decide whether to emit a given event based on its kind discriminant
/// and a stable correlator string.
///
/// Implementations should be deterministic: invoking
/// [`SamplingPolicy::should_sample`] twice with the same arguments must
/// return the same result. This guarantee is what lets [`TelemetryHook`]
/// keep paired events (`tool.invoked` ↔ `tool.completed`) coherent — the
/// hook passes the same correlator on both sides of the pair, so the
/// policy decision is symmetric.
///
/// [`TelemetryHook`]: crate::TelemetryHook
pub trait SamplingPolicy: Send + Sync + std::fmt::Debug {
    /// Return `true` if the event with the given `kind` discriminant
    /// (e.g. `"tool.invoked"`, `"prompt.completed"`) and per-emission
    /// `correlator` should be emitted.
    ///
    /// The `correlator` is producer-supplied and intended to be the
    /// most natural pairing key for the event family — for `tool.*`
    /// the internal call id, for `prompt.*` the conversation id, etc.
    /// Policies that ignore it (e.g. [`AlwaysSample`]) are free to do
    /// so; policies that hash it (e.g. [`RatePolicy`]) get
    /// deterministic, paired-event-safe sampling for free.
    fn should_sample(&self, kind: &str, correlator: &str) -> bool;
}

/// Policy that keeps every event. The default for [`TelemetryHook`].
///
/// [`TelemetryHook`]: crate::TelemetryHook
#[derive(Debug, Default, Clone, Copy)]
pub struct AlwaysSample;

impl SamplingPolicy for AlwaysSample {
    fn should_sample(&self, _kind: &str, _correlator: &str) -> bool {
        true
    }
}

/// A policy that wraps an underlying downsampler but guarantees that critical
/// error and recovery paths always bypass the drop rate and get emitted 100%
/// of the time.
///
/// This provides simple "Adaptive Sampling" or "Tail-based Sampling" so you
/// can run a busy swarm at `0.01` rate for happy paths but capture `1.0` of
/// any failures or recoveries.
///
/// The always-keep set is a deliberate allowlist of *anomaly* kinds, not a
/// broad suffix match. It covers genuine failures and recovery signals:
///
/// - `prompt.failed`, `tool.failed`, `tool.hosted_completed` is **not**
///   included (it is a success terminal);
/// - `tool.terminated` — a hook aborted the agent loop;
/// - `compose.recovery`, `compose.retry_attempt`.
///
/// `tool.skipped` is intentionally **not** retained: a gate choosing not to
/// run a tool is a routine control-flow decision, not an anomaly, and keeping
/// 100% of skips would flood a sampled stream with non-error volume. Callers
/// who want additional kinds always kept can list them via
/// [`AdaptiveErrorPolicy::also_keep`].
#[derive(Debug, Clone)]
pub struct AdaptiveErrorPolicy<P: SamplingPolicy> {
    inner: P,
    extra_keep: Vec<String>,
}

/// Event kinds the [`AdaptiveErrorPolicy`] always keeps regardless of the
/// inner policy's drop rate.
const ALWAYS_KEEP_KINDS: &[&str] = &[
    "prompt.failed",
    "tool.failed",
    "tool.terminated",
    "compose.recovery",
    "compose.retry_attempt",
];

impl<P: SamplingPolicy> AdaptiveErrorPolicy<P> {
    /// Wrap an existing policy (like [`RatePolicy`]) with error-bypassing logic.
    pub fn new(inner: P) -> Self {
        Self {
            inner,
            extra_keep: Vec::new(),
        }
    }

    /// Add an extra event-kind discriminant to the always-keep allowlist
    /// (e.g. `"tool.skipped"` if your deployment treats skips as signal).
    #[must_use]
    pub fn also_keep(mut self, kind: impl Into<String>) -> Self {
        self.extra_keep.push(kind.into());
        self
    }
}

impl<P: SamplingPolicy> SamplingPolicy for AdaptiveErrorPolicy<P> {
    fn should_sample(&self, kind: &str, correlator: &str) -> bool {
        // Deterministically retain genuine failure / recovery anomalies.
        if ALWAYS_KEEP_KINDS.contains(&kind) || self.extra_keep.iter().any(|extra| extra == kind) {
            return true;
        }

        self.inner.should_sample(kind, correlator)
    }
}

/// Per-kind rate sampler with deterministic, paired-event-safe
/// decisions.
///
/// Unspecified kinds default to `default_rate` (initially `1.0`, i.e.
/// always sample). Configured kinds use the supplied rate in `[0, 1]`,
/// clamped to that range. Rates outside the unit interval are treated
/// as the nearest valid value.
///
/// Sampling is computed by hashing the `correlator` with [`std::hash`]'s
/// default hasher and comparing the bottom 32 bits, scaled to `[0, 1)`,
/// against the configured rate. Because event kind is deliberately not part
/// of the bucket hash, paired emissions (e.g. `tool.invoked` and
/// `tool.completed` sharing an internal call id) use the same bucket and are
/// either both kept or both dropped when configured with the same rate.
#[derive(Debug, Clone)]
pub struct RatePolicy {
    rates: HashMap<String, f64>,
    default_rate: f64,
}

impl Default for RatePolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl RatePolicy {
    /// Build a policy that keeps every event (`default_rate = 1.0`)
    /// until rates are configured per-kind via [`with_rate`].
    ///
    /// [`with_rate`]: Self::with_rate
    pub fn new() -> Self {
        Self {
            rates: HashMap::new(),
            default_rate: 1.0,
        }
    }

    /// Override the rate applied to event kinds that have no explicit
    /// entry. Useful when downsampling is the rule and full sampling
    /// is the exception.
    #[must_use]
    pub fn with_default_rate(mut self, rate: f64) -> Self {
        self.default_rate = clamp_unit(rate);
        self
    }

    /// Set the sampling rate for `kind` to `rate` (clamped to `[0, 1]`).
    ///
    /// `kind` should match an [`EventKind::discriminant()`](crate::EventKind::discriminant)
    /// return value such as `"tool.invoked"`, `"tool.completed"`,
    /// `"prompt.completed"`, `"eval.report"`.
    #[must_use]
    pub fn with_rate(mut self, kind: impl Into<String>, rate: f64) -> Self {
        self.rates.insert(kind.into(), clamp_unit(rate));
        self
    }

    fn rate_for(&self, kind: &str) -> f64 {
        self.rates.get(kind).copied().unwrap_or(self.default_rate)
    }
}

impl SamplingPolicy for RatePolicy {
    fn should_sample(&self, kind: &str, correlator: &str) -> bool {
        let rate = self.rate_for(kind);
        if rate >= 1.0 {
            return true;
        }
        if rate <= 0.0 {
            return false;
        }
        // `std::hash::RandomState::new()` would randomise the decision
        // across processes; we want determinism per-process *and* per
        // correlator, so we use a fixed seed hasher.
        let bucket = (FixedHasher.hash_one(correlator) as u32) as f64 / (u32::MAX as f64 + 1.0);
        bucket < rate
    }
}

fn clamp_unit(rate: f64) -> f64 {
    if rate.is_nan() {
        return 0.0;
    }
    rate.clamp(0.0, 1.0)
}

/// Fixed-seed `BuildHasher` so sampling decisions are reproducible
/// across processes. We deliberately do not use `RandomState` here.
#[derive(Debug, Default, Clone, Copy)]
struct FixedHasher;

impl BuildHasher for FixedHasher {
    type Hasher = std::collections::hash_map::DefaultHasher;

    fn build_hasher(&self) -> Self::Hasher {
        std::collections::hash_map::DefaultHasher::new()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn always_sample_keeps_every_event() {
        let policy = AlwaysSample;
        assert!(policy.should_sample("tool.invoked", "call-1"));
        assert!(policy.should_sample("prompt.completed", "conv-1"));
        assert!(policy.should_sample("anything", ""));
    }

    #[test]
    fn rate_zero_drops_everything_rate_one_keeps_everything() {
        let policy = RatePolicy::new()
            .with_rate("tool.invoked", 0.0)
            .with_rate("tool.completed", 1.0);
        for i in 0..100 {
            let id = format!("call-{i}");
            assert!(!policy.should_sample("tool.invoked", &id));
            assert!(policy.should_sample("tool.completed", &id));
        }
    }

    #[test]
    fn rate_decisions_are_deterministic_and_pair_coherent() {
        let policy = RatePolicy::new()
            .with_rate("tool.invoked", 0.5)
            .with_rate("tool.completed", 0.5);
        for i in 0..50 {
            let id = format!("call-{i}");
            let invoked = policy.should_sample("tool.invoked", &id);
            assert_eq!(invoked, policy.should_sample("tool.invoked", &id));
            let completed = policy.should_sample("tool.completed", &id);
            assert_eq!(
                invoked, completed,
                "tool.invoked/tool.completed must share the same bucket for {id}"
            );
        }
    }

    #[test]
    fn rate_default_rate_is_used_for_unspecified_kinds() {
        let policy = RatePolicy::new().with_default_rate(0.0);
        assert!(!policy.should_sample("memory.frame_written", "conv-1"));
        // Configured kinds still win.
        let policy = policy.with_rate("memory.frame_written", 1.0);
        assert!(policy.should_sample("memory.frame_written", "conv-1"));
    }

    #[test]
    fn rate_clamps_out_of_range_inputs() {
        let policy = RatePolicy::new()
            .with_rate("a", -0.5)
            .with_rate("b", 1.5)
            .with_rate("c", f64::NAN);
        assert!(!policy.should_sample("a", "x"));
        assert!(policy.should_sample("b", "x"));
        assert!(!policy.should_sample("c", "x"));
    }

    #[test]
    fn rate_approximates_configured_rate_over_a_population() {
        let policy = RatePolicy::new().with_rate("tool.invoked", 0.30);
        let mut kept = 0;
        let total = 5_000;
        for i in 0..total {
            let id = format!("call-{i}");
            if policy.should_sample("tool.invoked", &id) {
                kept += 1;
            }
        }
        let observed = kept as f64 / total as f64;
        // Wide tolerance — this is a smoke test, not a statistical proof.
        assert!(
            (observed - 0.30).abs() < 0.05,
            "observed rate {observed} drifted from configured 0.30"
        );
    }
}
