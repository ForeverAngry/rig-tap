//! Data redaction policies for [`TelemetryHook`](crate::TelemetryHook).
//!
//! Enterprise environments often require stripping PII (Personally Identifiable Information),
//! credentials, or sensitive business data from logs *before* they are serialized and emitted
//! to standard observability pipelines (like OpenTelemetry collectors).
//!
//! The [`RedactionPolicy`] trait allows host applications to intercept and scrub `args_json` and
//! `result` strings on tool invocations and completions.
//!
//! The default policy ([`IdentityRedaction`]) performs no scrubbing.

/// Defines how sensitive fields in telemetry events should be redacted before emission.
///
/// Implemented by host applications to scrub PII or secrets.
pub trait RedactionPolicy: Send + Sync + std::fmt::Debug {
    /// Scrub sensitive data from JSON-encoded tool arguments.
    ///
    /// This is called just before `tool.invoked` and `tool.hosted_invoked` events are emitted.
    /// The input `args_json` is the raw stringified JSON (pre-truncation).
    ///
    /// Returns the redacted string.
    fn redact_tool_args(&self, tool_name: &str, args_json: &str) -> String;

    /// Scrub sensitive data from tool execution results.
    ///
    /// This is called just before `tool.completed` and `tool.hosted_completed` events are emitted.
    /// The input `result` is the raw stringified result (pre-truncation).
    ///
    /// Returns the redacted string.
    fn redact_tool_result(&self, tool_name: &str, result: &str) -> String;
}

/// A no-op redaction policy that leaves all payloads intact.
///
/// This is the default policy for [`TelemetryHook`](crate::TelemetryHook).
#[derive(Debug, Default, Clone, Copy)]
pub struct IdentityRedaction;

impl RedactionPolicy for IdentityRedaction {
    fn redact_tool_args(&self, _tool_name: &str, args_json: &str) -> String {
        args_json.to_string()
    }

    fn redact_tool_result(&self, _tool_name: &str, result: &str) -> String {
        result.to_string()
    }
}
