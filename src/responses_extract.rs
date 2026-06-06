//! Hosted-tool extractor for OpenAI Responses-style payloads.
//!
//! Provider-native hosted tools — OpenAI Responses `web_search`,
//! `file_search`, `computer_use`, `code_interpreter`, and future
//! Anthropic / Google equivalents — execute inside the provider's
//! infrastructure. They never fire `PromptHook::on_tool_call`, so they
//! are invisible to [`TelemetryHook`](crate::TelemetryHook).
//!
//! This module surfaces them as
//! [`EventKind::ToolHostedInvoked`]
//! /
//! [`EventKind::ToolHostedCompleted`]
//! event pairs by walking the raw JSON `output[]` array of a finished
//! Responses-API payload.
//!
//! # Why raw JSON?
//!
//! `rig-core`'s typed `responses_api::Output` enum carries a
//! `#[serde(other)] Unknown` variant for unknown item types
//! (`web_search_call`, `file_search_call`, ...). Deserializing through
//! that path silently discards the hosted-tool payload. The extractor
//! therefore operates on the raw `serde_json::Value` form that survives:
//!
//! - `rig::providers::openai::responses_api::websocket::ResponsesWebSocketDoneEvent::response`
//!   (raw `Value`, never deserialized through `Output`).
//! - Any raw HTTP response body the caller has access to before it is
//!   handed to `rig-core`.
//!
//! # Wire shape
//!
//! Each matched hosted-tool item emits a paired
//! `tool.hosted_invoked` + `tool.hosted_completed` event. The pair is
//! correlated by [`HostedToolCall::call_id`], which falls back to a
//! deterministic synthetic value (`"<kind>-<output_index>"`) when the
//! provider omits an `id`.
//!
//! # Example
//!
//! ```
//! use rig_tap::responses_extract::{extract_hosted_tools, HostedToolCall};
//! use serde_json::json;
//!
//! let payload = json!({
//!     "id": "resp_abc",
//!     "output": [
//!         {
//!             "type": "web_search_call",
//!             "id": "ws_001",
//!             "status": "completed",
//!             "action": { "type": "search", "queries": ["rig framework"] },
//!         }
//!     ]
//! });
//! let calls = extract_hosted_tools(&payload);
//! assert_eq!(calls.len(), 1);
//! assert_eq!(calls[0].tool_name, "web_search");
//! assert_eq!(calls[0].provider_call_id.as_deref(), Some("ws_001"));
//! assert_eq!(calls[0].status.as_deref(), Some("completed"));
//! ```

use serde_json::Value;

use crate::emit::emit_kind;
use crate::event::{EventKind, PAYLOAD_TRUNCATE_BYTES, truncate_utf8};

/// A hosted-tool call extracted from a Responses-API JSON payload.
///
/// Use [`extract_hosted_tools`] to obtain a `Vec<HostedToolCall>` from a
/// raw payload, or [`emit_hosted_tools`] to walk a payload and emit the
/// matching `tool.hosted_invoked` / `tool.hosted_completed` event pairs
/// directly.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostedToolCall {
    /// Hosted-tool name with the trailing `_call` stripped — e.g.
    /// `"web_search"`, `"file_search"`, `"computer_use"`,
    /// `"code_interpreter"`. Unknown providers pass through verbatim
    /// (minus the suffix), so the extractor stays forward-compatible.
    pub tool_name: String,
    /// Provider-supplied call ID, when the payload includes one.
    pub provider_call_id: Option<String>,
    /// Stable correlation ID used to pair `tool.hosted_invoked` with
    /// `tool.hosted_completed`. Falls back to a synthetic
    /// `"<tool_name>-<output_index>"` when the provider omits an `id`.
    pub call_id: String,
    /// Provider-reported status (`"in_progress"`, `"completed"`,
    /// `"failed"`, ...). `None` when the payload omits it.
    pub status: Option<String>,
    /// Compact JSON-encoded view of the hosted-tool *inputs* — the
    /// fields the model handed the hosted tool. Pulled from the union
    /// of known input-bearing keys (`action`, `queries`, `query`,
    /// `code`, `input`, `arguments`, `container_id`, `parameters`).
    /// Empty string when none are present.
    pub args_json: String,
    /// JSON-encoded view of the hosted-tool *result*. Pulled from the
    /// union of known output-bearing keys (`output`, `results`,
    /// `result`, `output_text`). Empty string when none are present.
    pub result_json: String,
    /// The raw JSON sub-object as it appeared in `output[]`. Callers
    /// can dig further for provider-specific fields that the extractor
    /// does not model directly.
    pub raw: Value,
    /// Zero-based position in the `output[]` array. Useful as a
    /// fallback ordering key and for surfacing in diagnostics.
    pub output_index: usize,
}

const INPUT_KEYS: &[&str] = &[
    "action",
    "queries",
    "query",
    "code",
    "input",
    "arguments",
    "container_id",
    "parameters",
];

const OUTPUT_KEYS: &[&str] = &["output", "results", "result", "output_text"];

/// Walks the `output[]` array of a Responses-API JSON payload and
/// returns every hosted-tool call it finds.
///
/// A hosted-tool item is any object whose `type` field ends in `_call`
/// **other than** `function_call` (function tools already fire
/// `PromptHook::on_tool_call` and surface as
/// [`EventKind::ToolInvoked`]).
///
/// Returns an empty vector when `payload.output` is missing, not an
/// array, or empty.
#[must_use]
pub fn extract_hosted_tools(payload: &Value) -> Vec<HostedToolCall> {
    let Some(items) = payload.get("output").and_then(Value::as_array) else {
        return Vec::new();
    };

    let mut calls = Vec::new();
    for (output_index, item) in items.iter().enumerate() {
        let Some(call) = parse_hosted_item(item, output_index) else {
            continue;
        };
        calls.push(call);
    }
    calls
}

fn parse_hosted_item(item: &Value, output_index: usize) -> Option<HostedToolCall> {
    let raw_type = item.get("type").and_then(Value::as_str)?;
    // `function_call` is the typed-tool path that already fires
    // `PromptHook::on_tool_call` — skip so we don't double-emit.
    if raw_type == "function_call" || raw_type == "function_call_output" {
        return None;
    }
    let tool_name = match raw_type.strip_suffix("_call") {
        Some(stripped) if !stripped.is_empty() => stripped.to_owned(),
        _ => return None,
    };
    let provider_call_id = item.get("id").and_then(Value::as_str).map(str::to_owned);
    let call_id = provider_call_id
        .clone()
        .unwrap_or_else(|| format!("{tool_name}-{output_index}"));
    let status = item
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let args_json = compact_json_subset(item, INPUT_KEYS);
    let result_json = compact_json_subset(item, OUTPUT_KEYS);
    Some(HostedToolCall {
        tool_name,
        provider_call_id,
        call_id,
        status,
        args_json,
        result_json,
        raw: item.clone(),
        output_index,
    })
}

/// Serializes a stable-ordered subset of `item`'s fields whose keys
/// appear in `keys`. Returns an empty string when no keys match.
fn compact_json_subset(item: &Value, keys: &[&str]) -> String {
    let Some(obj) = item.as_object() else {
        return String::new();
    };
    let mut picked = serde_json::Map::new();
    for key in keys {
        if let Some(value) = obj.get(*key) {
            picked.insert((*key).to_owned(), value.clone());
        }
    }
    if picked.is_empty() {
        return String::new();
    }
    serde_json::to_string(&Value::Object(picked)).unwrap_or_default()
}

/// Walks `payload` for hosted-tool calls and emits one
/// `tool.hosted_invoked` + `tool.hosted_completed` pair for each via
/// [`emit_kind`].
///
/// Returns the number of pairs emitted. Use this when you already have
/// a finished Responses-API payload (e.g. `ResponsesWebSocketDoneEvent.response`)
/// and want hosted-tool visibility without manually constructing events.
///
/// `response_id` is stamped on every emitted event when `Some`. The
/// extractor also tries `payload["id"]` when the caller passes `None`.
///
/// This helper performs **no redaction**. If the payload may contain PII or
/// secrets, use [`emit_hosted_tools_redacted`] with a
/// [`RedactionPolicy`](crate::RedactionPolicy) instead.
pub fn emit_hosted_tools(
    conversation_id: impl AsRef<str>,
    response_id: Option<&str>,
    payload: &Value,
) -> usize {
    emit_hosted_tools_redacted(
        conversation_id,
        response_id,
        payload,
        &crate::redaction::IdentityRedaction,
    )
}

/// Like [`emit_hosted_tools`], but scrubs every hosted-tool `args_json` and
/// `result_json` through `redaction` before truncation and emission.
///
/// Use this on producer paths that surface provider-hosted tool payloads
/// (`web_search`, `file_search`, `code_interpreter`, …) which can echo back
/// user input or sensitive data, so the same
/// [`RedactionPolicy`](crate::RedactionPolicy) that guards the agent and
/// kernel paths also covers hosted tools.
pub fn emit_hosted_tools_redacted(
    conversation_id: impl AsRef<str>,
    response_id: Option<&str>,
    payload: &Value,
    redaction: &dyn crate::redaction::RedactionPolicy,
) -> usize {
    let conversation_id = conversation_id.as_ref();
    let resolved_response_id = response_id
        .map(str::to_owned)
        .or_else(|| payload.get("id").and_then(Value::as_str).map(str::to_owned));
    let calls = extract_hosted_tools(payload);
    let count = calls.len();
    for call in calls {
        let HostedToolCall {
            tool_name,
            provider_call_id,
            call_id,
            status,
            args_json,
            result_json,
            ..
        } = call;
        let scrubbed_args = redaction.redact_tool_args(&tool_name, &args_json);
        let scrubbed_result = redaction.redact_tool_result(&tool_name, &result_json);
        let (args_payload, args_truncated) = truncate_utf8(&scrubbed_args, PAYLOAD_TRUNCATE_BYTES);
        let (result_payload, result_truncated) =
            truncate_utf8(&scrubbed_result, PAYLOAD_TRUNCATE_BYTES);
        emit_kind(
            conversation_id,
            EventKind::ToolHostedInvoked {
                tool_name: tool_name.clone(),
                provider_call_id: provider_call_id.clone(),
                call_id: call_id.clone(),
                response_id: resolved_response_id.clone(),
                args_json: args_payload,
                truncated: args_truncated,
            },
        );
        emit_kind(
            conversation_id,
            EventKind::ToolHostedCompleted {
                tool_name,
                provider_call_id,
                call_id,
                response_id: resolved_response_id.clone(),
                status,
                result: result_payload,
                truncated: result_truncated,
                duration_ms: None,
            },
        );
    }
    count
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_web_search_call() {
        let payload = json!({
            "id": "resp_1",
            "output": [
                {
                    "type": "web_search_call",
                    "id": "ws_42",
                    "status": "completed",
                    "action": { "type": "search", "queries": ["rig observability"] },
                    "results": [{ "url": "https://example.com" }],
                }
            ]
        });
        let calls = extract_hosted_tools(&payload);
        assert_eq!(calls.len(), 1);
        let call = &calls[0];
        assert_eq!(call.tool_name, "web_search");
        assert_eq!(call.provider_call_id.as_deref(), Some("ws_42"));
        assert_eq!(call.call_id, "ws_42");
        assert_eq!(call.status.as_deref(), Some("completed"));
        assert!(call.args_json.contains("queries"));
        assert!(call.result_json.contains("example.com"));
        assert_eq!(call.output_index, 0);
    }

    #[test]
    fn extracts_multiple_kinds_in_order() {
        let payload = json!({
            "output": [
                { "type": "message", "id": "m1", "content": [] },
                { "type": "file_search_call", "id": "fs_1", "status": "in_progress",
                  "queries": ["spec"] },
                { "type": "function_call", "id": "fn_1", "name": "calc",
                  "arguments": "{}" },
                { "type": "code_interpreter_call", "id": "ci_1", "status": "completed",
                  "code": "print(1)", "output": "1\n" },
            ]
        });
        let calls = extract_hosted_tools(&payload);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].tool_name, "file_search");
        assert_eq!(calls[0].output_index, 1);
        assert_eq!(calls[1].tool_name, "code_interpreter");
        assert_eq!(calls[1].output_index, 3);
        assert!(calls[1].args_json.contains("print"));
        assert!(calls[1].result_json.contains("1"));
    }

    #[test]
    fn synthesizes_call_id_when_provider_omits_id() {
        let payload = json!({
            "output": [
                { "type": "computer_use_call", "status": "completed" }
            ]
        });
        let calls = extract_hosted_tools(&payload);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].provider_call_id, None);
        assert_eq!(calls[0].call_id, "computer_use-0");
    }

    #[test]
    fn forward_compatible_for_unknown_hosted_kind() {
        let payload = json!({
            "output": [
                { "type": "future_thing_call", "id": "ft_1", "status": "completed" }
            ]
        });
        let calls = extract_hosted_tools(&payload);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool_name, "future_thing");
    }

    #[test]
    fn ignores_function_call_and_non_call_items() {
        let payload = json!({
            "output": [
                { "type": "function_call", "id": "fn_1", "name": "x", "arguments": "{}" },
                { "type": "function_call_output", "call_id": "fn_1", "output": "ok" },
                { "type": "message", "id": "m1", "content": [] },
                { "type": "reasoning", "id": "r1", "summary": [] },
            ]
        });
        assert!(extract_hosted_tools(&payload).is_empty());
    }

    #[test]
    fn returns_empty_when_output_missing_or_wrong_shape() {
        assert!(extract_hosted_tools(&json!({})).is_empty());
        assert!(extract_hosted_tools(&json!({ "output": "not an array" })).is_empty());
        assert!(extract_hosted_tools(&json!({ "output": [] })).is_empty());
    }

    #[test]
    #[cfg(feature = "subscriber")]
    fn emit_hosted_tools_emits_invoked_and_completed_per_call() {
        use crate::subscriber::CapturingLayer;
        use tracing_subscriber::prelude::*;

        let layer = CapturingLayer::new();
        let probe = layer.clone();
        let payload = json!({
            "id": "resp_xyz",
            "output": [
                { "type": "web_search_call", "id": "ws_1", "status": "completed",
                  "action": { "type": "search", "queries": ["a"] },
                  "results": [{ "url": "u" }] },
                { "type": "file_search_call", "id": "fs_1", "status": "completed",
                  "queries": ["b"], "results": [{ "id": "doc1" }] },
            ]
        });
        let count =
            tracing::subscriber::with_default(tracing_subscriber::registry().with(layer), || {
                emit_hosted_tools("conv-1", None, &payload)
            });
        assert_eq!(count, 2);
        let events = probe.snapshot();
        assert_eq!(events.len(), 4);
        let kinds: Vec<&str> = events.iter().map(|e| e.kind.discriminant()).collect();
        assert_eq!(
            kinds,
            vec![
                "tool.hosted_invoked",
                "tool.hosted_completed",
                "tool.hosted_invoked",
                "tool.hosted_completed",
            ]
        );
        for event in &events {
            assert_eq!(event.kind.scalar_fields().response_id, "resp_xyz");
        }
    }
}
