//! Data redaction policies for [`TelemetryHook`](crate::TelemetryHook).
//!
//! Enterprise environments often require stripping PII (Personally Identifiable Information),
//! credentials, or sensitive business data from logs *before* they are serialized and emitted
//! to standard observability pipelines (like OpenTelemetry collectors).
//!
//! The [`RedactionPolicy`] trait allows host applications to intercept and scrub `args_json` and
//! `result` strings on tool invocations and completions.
//!
//! The default policy ([`IdentityRedaction`]) performs no scrubbing and is
//! zero-copy: it borrows the input straight through so the common
//! no-redaction path never allocates.

use std::borrow::Cow;

/// Defines how sensitive fields in telemetry events should be redacted before emission.
///
/// Implemented by host applications to scrub PII or secrets.
///
/// Methods return [`Cow<str>`] so a policy that does not need to modify the
/// input (the common case, and always for [`IdentityRedaction`]) can borrow it
/// back with `Cow::Borrowed` and avoid an allocation on the per-tool hot path.
/// Policies that actually rewrite the payload return `Cow::Owned`.
pub trait RedactionPolicy: Send + Sync + std::fmt::Debug {
    /// Scrub sensitive data from JSON-encoded tool arguments.
    ///
    /// This is called just before `tool.invoked` and `tool.hosted_invoked` events are emitted.
    /// The input `args_json` is the raw stringified JSON (pre-truncation).
    ///
    /// Return `Cow::Borrowed(args_json)` when no change is needed.
    fn redact_tool_args<'a>(&self, tool_name: &str, args_json: &'a str) -> Cow<'a, str>;

    /// Scrub sensitive data from tool execution results.
    ///
    /// This is called just before `tool.completed` and `tool.hosted_completed` events are emitted.
    /// The input `result` is the raw stringified result (pre-truncation).
    ///
    /// Return `Cow::Borrowed(result)` when no change is needed.
    fn redact_tool_result<'a>(&self, tool_name: &str, result: &'a str) -> Cow<'a, str>;
}

/// A no-op redaction policy that leaves all payloads intact.
///
/// This is the default policy for [`TelemetryHook`](crate::TelemetryHook). It
/// borrows its input straight through, so the default (no-redaction) path
/// performs no allocation.
#[derive(Debug, Default, Clone, Copy)]
pub struct IdentityRedaction;

impl RedactionPolicy for IdentityRedaction {
    fn redact_tool_args<'a>(&self, _tool_name: &str, args_json: &'a str) -> Cow<'a, str> {
        Cow::Borrowed(args_json)
    }

    fn redact_tool_result<'a>(&self, _tool_name: &str, result: &'a str) -> Cow<'a, str> {
        Cow::Borrowed(result)
    }
}
