//! Observability event schema (v1).
//!
//! All events flow through the [`ObservabilityEvent`] envelope so consumers
//! see a single, flat JSON shape regardless of the producing crate.

use serde::{Deserialize, Serialize};

/// Current schema version. Bumped on breaking changes to the wire format.
pub const SCHEMA_VERSION: u32 = 1;

/// Maximum byte length of inline `args_json` / `result_json` payloads before
/// they are truncated and marked with `"truncated": true`.
pub const PAYLOAD_TRUNCATE_BYTES: usize = 4096;

/// A single observability event with envelope metadata.
///
/// `kind` is flattened so the wire JSON is a single flat object:
///
/// ```json
/// {
///   "version": 1,
///   "occurred_at_millis": 1715000000000,
///   "tick": 42,
///   "conversation_id": "thread-1",
///   "kind": "prompt.started",
///   "model": "gpt-4o",
///   "messages_in": 3
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservabilityEvent {
    /// Schema version. See [`SCHEMA_VERSION`].
    pub version: u32,
    /// Wall-clock timestamp in milliseconds since the Unix epoch.
    pub occurred_at_millis: u64,
    /// Monotonic per-process counter. Use to order events without clock skew.
    pub tick: u64,
    /// Conversation / thread identifier this event belongs to.
    pub conversation_id: String,
    /// Event-specific payload. Flattened into the parent object.
    #[serde(flatten)]
    pub kind: EventKind,
}

impl ObservabilityEvent {
    /// Build a new envelope around `kind` using the current schema version.
    /// Callers normally use [`crate::emit::emit`] which fills in `tick` and
    /// `occurred_at_millis` automatically.
    pub fn new(conversation_id: impl Into<String>, kind: EventKind) -> Self {
        Self {
            version: SCHEMA_VERSION,
            occurred_at_millis: 0,
            tick: 0,
            conversation_id: conversation_id.into(),
            kind,
        }
    }
}

/// Per-variant scalar correlation fields surfaced as direct `tracing`
/// attributes alongside the JSON event blob. See [`EventKind::scalar_fields`].
///
/// Absent fields are represented as `""` rather than `Option<&str>` because
/// `tracing` 0.1's static-field model requires every field at the call site
/// to satisfy `tracing::Value`, which is not implemented for `Option<T>`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ScalarFields<'a> {
    /// `compose.*` event kernel identifier.
    pub kernel_id: &'a str,
    /// `tool.*` and `compose.retry_attempt` target/tool name.
    pub tool_name: &'a str,
    /// `tool.*` stable correlation identifier.
    pub call_id: &'a str,
    /// `compose.skill_resolved` / `compose.loop_iteration` skill identifier.
    pub skill_id: &'a str,
    /// `prompt.*` model identifier.
    pub model: &'a str,
}

/// Payload variants. Tagged on the wire as `"kind": "<dotted.name>"`.
///
/// New variants are additive; rename or remove is a breaking change requiring
/// a bump of [`SCHEMA_VERSION`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
#[non_exhaustive]
pub enum EventKind {
    /// A prompt is about to be sent to the model provider.
    #[serde(rename = "prompt.started")]
    PromptStarted {
        /// Model name as declared on the agent.
        model: String,
        /// Number of messages in the history at the time of the call.
        messages_in: usize,
    },
    /// A prompt finished; the model returned a completion response.
    #[serde(rename = "prompt.completed")]
    PromptCompleted {
        /// Model name as reported by the provider response (may differ from
        /// the requested model for routed providers).
        model: String,
        /// Provider-reported input tokens, if known.
        #[serde(skip_serializing_if = "Option::is_none")]
        tokens_in: Option<u64>,
        /// Provider-reported output tokens, if known.
        #[serde(skip_serializing_if = "Option::is_none")]
        tokens_out: Option<u64>,
        /// Provider response ID, if supplied.
        #[serde(skip_serializing_if = "Option::is_none")]
        response_id: Option<String>,
    },
    /// A tool is about to be invoked.
    #[serde(rename = "tool.invoked")]
    ToolInvoked {
        /// Tool name as registered on the agent.
        tool_name: String,
        /// Provider-supplied tool-call ID, when present.
        #[serde(skip_serializing_if = "Option::is_none")]
        provider_call_id: Option<String>,
        /// Stable internal correlation ID (always present).
        call_id: String,
        /// JSON-encoded arguments (possibly truncated; see `truncated`).
        args_json: String,
        /// `true` if `args_json` was truncated to
        /// [`PAYLOAD_TRUNCATE_BYTES`].
        truncated: bool,
    },
    /// A tool finished executing.
    #[serde(rename = "tool.completed")]
    ToolCompleted {
        /// Tool name (matches the paired `tool.invoked`).
        tool_name: String,
        /// Provider-supplied tool-call ID, when present.
        #[serde(skip_serializing_if = "Option::is_none")]
        provider_call_id: Option<String>,
        /// Stable internal correlation ID (matches the paired `tool.invoked`).
        call_id: String,
        /// Tool result text (possibly truncated; see `truncated`).
        result: String,
        /// `true` if `result` was truncated to [`PAYLOAD_TRUNCATE_BYTES`].
        truncated: bool,
    },
    /// A previously-`ToolInvoked` call was skipped by a gating hook before
    /// the tool body ran. Pairs by `call_id` and closes the
    /// `tool.invoked`/`tool.completed` gap that would otherwise leave the
    /// invoke event orphaned.
    #[serde(rename = "tool.skipped")]
    ToolSkipped {
        /// Tool name (matches the paired `tool.invoked`).
        tool_name: String,
        /// Stable internal correlation ID (matches the paired `tool.invoked`).
        call_id: String,
        /// Human-readable reason from the gate.
        reason: String,
    },
    /// A previously-`ToolInvoked` call triggered a hook-driven termination
    /// of the agent loop. Pairs by `call_id`.
    #[serde(rename = "tool.terminated")]
    ToolTerminated {
        /// Tool name (matches the paired `tool.invoked`).
        tool_name: String,
        /// Stable internal correlation ID (matches the paired `tool.invoked`).
        call_id: String,
        /// Human-readable reason from the hook.
        reason: String,
    },
    /// The active context was sampled (typically on `ConversationMemory::load`).
    #[serde(rename = "context.sampled")]
    ContextSampled {
        /// Number of messages in the loaded history.
        message_count: usize,
        /// JSON byte size of the loaded history (rough size estimate).
        byte_size: usize,
        /// Optional token-count estimate. `None` in the default build; populated
        /// by consumers that wire a tokenizer.
        #[serde(skip_serializing_if = "Option::is_none")]
        token_estimate: Option<u64>,
    },
    /// A compactor fired, replacing some evicted history with a summary
    /// artifact.
    #[serde(rename = "context.compacted")]
    ContextCompacted {
        /// Number of messages evicted from the active context.
        evicted_count: usize,
        /// Approximate byte size of the evicted messages.
        evicted_bytes: usize,
        /// `true` if the compactor produced a carry-over artifact for the
        /// next compaction cycle.
        carry_over: bool,
        /// Byte size of the summary text written to long-term memory.
        summary_bytes: usize,
    },
    /// A demotion hook moved messages to long-term storage.
    #[serde(rename = "memory.demoted")]
    MemoryDemoted {
        /// Number of messages demoted.
        demoted_count: usize,
        /// Tags applied to the demoted frames.
        tags: Vec<String>,
    },
    /// A frame was written to the long-term store.
    #[serde(rename = "memory.frame_written")]
    MemoryFrameWritten {
        /// Frame kind as classified by the producer (e.g. `"summary"`,
        /// `"demoted"`).
        frame_kind: String,
        /// Total frame count in the store after the write. `None` when the
        /// producer does not expose a cheap cumulative count (e.g. memvid).
        /// Consumers SHOULD NOT assume `0` means "empty store" — use this
        /// `Option` and treat absence as "unknown".
        #[serde(skip_serializing_if = "Option::is_none")]
        frame_count_after: Option<u64>,
        /// Byte size of the written frame's text payload.
        bytes_written: usize,
    },
    /// A `rig-compose` kernel became active for a conversation.
    #[serde(rename = "compose.kernel_start")]
    ComposeKernelStart {
        /// Stable kernel identifier chosen by the producer.
        kernel_id: String,
        /// Number of skills registered at startup, when known.
        #[serde(skip_serializing_if = "Option::is_none")]
        skills_registered: Option<usize>,
        /// Number of tools registered at startup, when known.
        #[serde(skip_serializing_if = "Option::is_none")]
        tools_registered: Option<usize>,
    },
    /// A `rig-compose` kernel stopped processing.
    #[serde(rename = "compose.kernel_shutdown")]
    ComposeKernelShutdown {
        /// Stable kernel identifier chosen by the producer.
        kernel_id: String,
        /// Producer-specific shutdown reason (e.g. `"normal"`, `"error"`).
        reason: String,
    },
    /// One iteration of a `rig-compose` agent/kernel loop began.
    #[serde(rename = "compose.loop_iteration")]
    ComposeLoopIteration {
        /// Stable kernel identifier chosen by the producer.
        kernel_id: String,
        /// Monotonic iteration counter inside the kernel.
        iteration: u64,
        /// Skill being considered or executed during this iteration.
        #[serde(skip_serializing_if = "Option::is_none")]
        skill_id: Option<String>,
        /// Current confidence score, when exposed by the producer.
        #[serde(skip_serializing_if = "Option::is_none")]
        confidence: Option<f64>,
    },
    /// A `rig-compose` skill resolution completed.
    #[serde(rename = "compose.skill_resolved")]
    ComposeSkillResolved {
        /// Stable kernel identifier chosen by the producer.
        kernel_id: String,
        /// Skill identifier.
        skill_id: String,
        /// Whether the skill applied to the current context.
        applies: bool,
        /// Confidence delta returned by the skill, when present.
        #[serde(skip_serializing_if = "Option::is_none")]
        delta: Option<f64>,
        /// Post-application confidence score, when exposed by the producer.
        /// For `applies = false` resolutions this is the unchanged context
        /// confidence; for `applies = true` it reflects `confidence + delta`
        /// clamped to `[0.0, 1.0]`.
        #[serde(skip_serializing_if = "Option::is_none", default)]
        confidence: Option<f64>,
    },
    /// A retry attempt occurred in a `rig-compose` dispatch or recovery path.
    ///
    /// `rig-tap` does not emit this variant itself: the
    /// [`crate::DispatchObserveHook`] only observes the lifecycle hooks
    /// surfaced by `rig-compose` and `rig-compose` does not currently expose
    /// a per-tool retry hook. Producers with their own retry policy (custom
    /// skills, transports, or higher-level orchestrators) should emit this
    /// variant directly via [`crate::emit_kind`] so consumers receive a
    /// consistent shape.
    #[serde(rename = "compose.retry_attempt")]
    ComposeRetryAttempt {
        /// Stable kernel identifier chosen by the producer.
        kernel_id: String,
        /// Tool or operation being retried.
        target: String,
        /// One-based retry attempt number.
        attempt: u64,
        /// Retry classification chosen by the producer.
        classification: String,
    },
    /// A `rig-compose` recovery path completed.
    #[serde(rename = "compose.recovery")]
    ComposeRecovery {
        /// Stable kernel identifier chosen by the producer.
        kernel_id: String,
        /// Recovery reason or source error classification.
        reason: String,
        /// Whether the recovery path restored normal execution.
        recovered: bool,
    },
}

impl EventKind {
    /// Returns the wire `kind` discriminant for this event.
    pub fn discriminant(&self) -> &'static str {
        match self {
            EventKind::PromptStarted { .. } => "prompt.started",
            EventKind::PromptCompleted { .. } => "prompt.completed",
            EventKind::ToolInvoked { .. } => "tool.invoked",
            EventKind::ToolCompleted { .. } => "tool.completed",
            EventKind::ToolSkipped { .. } => "tool.skipped",
            EventKind::ToolTerminated { .. } => "tool.terminated",
            EventKind::ContextSampled { .. } => "context.sampled",
            EventKind::ContextCompacted { .. } => "context.compacted",
            EventKind::MemoryDemoted { .. } => "memory.demoted",
            EventKind::MemoryFrameWritten { .. } => "memory.frame_written",
            EventKind::ComposeKernelStart { .. } => "compose.kernel_start",
            EventKind::ComposeKernelShutdown { .. } => "compose.kernel_shutdown",
            EventKind::ComposeLoopIteration { .. } => "compose.loop_iteration",
            EventKind::ComposeSkillResolved { .. } => "compose.skill_resolved",
            EventKind::ComposeRetryAttempt { .. } => "compose.retry_attempt",
            EventKind::ComposeRecovery { .. } => "compose.recovery",
        }
    }

    /// Extract the per-variant scalar correlation fields that
    /// [`crate::emit()`] surfaces directly on the `tracing` event so that
    /// OpenTelemetry collectors and log indexers can route on them without
    /// parsing the JSON `event` blob.
    ///
    /// Absent fields are returned as `""` rather than `Option<&str>`
    /// because `tracing` 0.1's static-field model does not accept
    /// `Option<&str>` as a `Value`. Consumers should filter
    /// `rig_tap.<field> != ""` to detect presence.
    pub fn scalar_fields(&self) -> ScalarFields<'_> {
        let mut f = ScalarFields::default();
        match self {
            EventKind::PromptStarted { model, .. } => f.model = model,
            EventKind::PromptCompleted { model, .. } => f.model = model,
            EventKind::ToolInvoked {
                tool_name, call_id, ..
            }
            | EventKind::ToolCompleted {
                tool_name, call_id, ..
            } => {
                f.tool_name = tool_name;
                f.call_id = call_id;
            }
            EventKind::ToolSkipped {
                tool_name, call_id, ..
            }
            | EventKind::ToolTerminated {
                tool_name, call_id, ..
            } => {
                f.tool_name = tool_name;
                f.call_id = call_id;
            }
            EventKind::ComposeKernelStart { kernel_id, .. }
            | EventKind::ComposeKernelShutdown { kernel_id, .. }
            | EventKind::ComposeRecovery { kernel_id, .. } => {
                f.kernel_id = kernel_id;
            }
            EventKind::ComposeLoopIteration {
                kernel_id,
                skill_id,
                ..
            } => {
                f.kernel_id = kernel_id;
                if let Some(s) = skill_id {
                    f.skill_id = s;
                }
            }
            EventKind::ComposeSkillResolved {
                kernel_id,
                skill_id,
                ..
            } => {
                f.kernel_id = kernel_id;
                f.skill_id = skill_id;
            }
            EventKind::ComposeRetryAttempt {
                kernel_id, target, ..
            } => {
                f.kernel_id = kernel_id;
                f.tool_name = target;
            }
            EventKind::ContextSampled { .. }
            | EventKind::ContextCompacted { .. }
            | EventKind::MemoryDemoted { .. }
            | EventKind::MemoryFrameWritten { .. } => {}
        }
        f
    }

    /// Returns `true` if the event is part of the prompt lifecycle (`prompt.started`, `prompt.completed`).
    pub fn is_prompt_related(&self) -> bool {
        matches!(
            self,
            EventKind::PromptStarted { .. } | EventKind::PromptCompleted { .. }
        )
    }

    /// Returns `true` if the event is part of the tool lifecycle (`tool.invoked`, `tool.completed`, `tool.skipped`, `tool.terminated`).
    pub fn is_tool_related(&self) -> bool {
        matches!(
            self,
            EventKind::ToolInvoked { .. }
                | EventKind::ToolCompleted { .. }
                | EventKind::ToolSkipped { .. }
                | EventKind::ToolTerminated { .. }
        )
    }

    /// Returns `true` if the event is related to memory and context management.
    pub fn is_memory_related(&self) -> bool {
        matches!(
            self,
            EventKind::ContextSampled { .. }
                | EventKind::ContextCompacted { .. }
                | EventKind::MemoryDemoted { .. }
                | EventKind::MemoryFrameWritten { .. }
        )
    }

    /// Returns `true` if the event is related to a `rig-compose` kernel or agent loop.
    pub fn is_compose_related(&self) -> bool {
        matches!(
            self,
            EventKind::ComposeKernelStart { .. }
                | EventKind::ComposeKernelShutdown { .. }
                | EventKind::ComposeLoopIteration { .. }
                | EventKind::ComposeSkillResolved { .. }
                | EventKind::ComposeRetryAttempt { .. }
                | EventKind::ComposeRecovery { .. }
        )
    }

    /// Extacts the stable `call_id` for tool events, if present.
    pub fn tool_call_id(&self) -> Option<&str> {
        match self {
            EventKind::ToolInvoked { call_id, .. } => Some(call_id),
            EventKind::ToolCompleted { call_id, .. } => Some(call_id),
            EventKind::ToolSkipped { call_id, .. } => Some(call_id),
            EventKind::ToolTerminated { call_id, .. } => Some(call_id),
            _ => None,
        }
    }
}

/// Truncate a UTF-8 string to at most `max_bytes`, returning the (possibly
/// truncated) string and a flag indicating whether truncation occurred.
///
/// Truncation always happens on a `char` boundary to keep the result valid
/// UTF-8.
pub fn truncate_utf8(input: &str, max_bytes: usize) -> (String, bool) {
    if input.len() <= max_bytes {
        return (input.to_string(), false);
    }

    let mut end = max_bytes;
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }

    match input.get(..end) {
        Some(slice) => (slice.to_string(), true),
        None => (String::new(), true),
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::expect_used
)]
mod tests {
    use super::*;

    #[test]
    fn envelope_serializes_flat() {
        let event = ObservabilityEvent {
            version: SCHEMA_VERSION,
            occurred_at_millis: 1715000000000,
            tick: 42,
            conversation_id: "thread-1".into(),
            kind: EventKind::PromptStarted {
                model: "gpt-4o".into(),
                messages_in: 3,
            },
        };

        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["kind"], "prompt.started");
        assert_eq!(json["model"], "gpt-4o");
        assert_eq!(json["messages_in"], 3);
        assert_eq!(json["tick"], 42);
        assert_eq!(json["version"], SCHEMA_VERSION);

        // Round-trip.
        let parsed: ObservabilityEvent = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn truncate_at_char_boundary() {
        let s = "café-α-β-γ-δ-ε-ζ-η-θ-ι-κ-λ-μ-ν-ξ-ο-π";
        let (out, truncated) = truncate_utf8(s, 6);
        assert!(truncated);
        // Must remain valid UTF-8 — round-tripping through String guarantees this.
        assert!(out.is_char_boundary(out.len()));
        assert!(out.len() <= 6);
    }

    #[test]
    fn truncate_no_op_when_short() {
        let (out, truncated) = truncate_utf8("ok", 100);
        assert!(!truncated);
        assert_eq!(out, "ok");
    }

    #[test]
    fn all_discriminants_round_trip() {
        let kinds = [
            EventKind::PromptStarted {
                model: "m".into(),
                messages_in: 1,
            },
            EventKind::PromptCompleted {
                model: "m".into(),
                tokens_in: Some(10),
                tokens_out: Some(20),
                response_id: Some("r".into()),
            },
            EventKind::ToolInvoked {
                tool_name: "t".into(),
                provider_call_id: None,
                call_id: "c".into(),
                args_json: "{}".into(),
                truncated: false,
            },
            EventKind::ToolCompleted {
                tool_name: "t".into(),
                provider_call_id: None,
                call_id: "c".into(),
                result: "ok".into(),
                truncated: false,
            },
            EventKind::ToolSkipped {
                tool_name: "t".into(),
                call_id: "c".into(),
                reason: "policy".into(),
            },
            EventKind::ToolTerminated {
                tool_name: "t".into(),
                call_id: "c".into(),
                reason: "abort".into(),
            },
            EventKind::ContextSampled {
                message_count: 5,
                byte_size: 1024,
                token_estimate: None,
            },
            EventKind::ContextCompacted {
                evicted_count: 3,
                evicted_bytes: 200,
                carry_over: false,
                summary_bytes: 80,
            },
            EventKind::MemoryDemoted {
                demoted_count: 2,
                tags: vec!["t".into()],
            },
            EventKind::MemoryFrameWritten {
                frame_kind: "summary".into(),
                frame_count_after: Some(7),
                bytes_written: 42,
            },
            EventKind::ComposeKernelStart {
                kernel_id: "k".into(),
                skills_registered: Some(2),
                tools_registered: Some(3),
            },
            EventKind::ComposeKernelShutdown {
                kernel_id: "k".into(),
                reason: "normal".into(),
            },
            EventKind::ComposeLoopIteration {
                kernel_id: "k".into(),
                iteration: 1,
                skill_id: Some("skill".into()),
                confidence: Some(0.5),
            },
            EventKind::ComposeSkillResolved {
                kernel_id: "k".into(),
                skill_id: "skill".into(),
                applies: true,
                delta: Some(0.25),
                confidence: Some(0.75),
            },
            EventKind::ComposeRetryAttempt {
                kernel_id: "k".into(),
                target: "tool".into(),
                attempt: 2,
                classification: "transient".into(),
            },
            EventKind::ComposeRecovery {
                kernel_id: "k".into(),
                reason: "retry_exhausted".into(),
                recovered: false,
            },
        ];

        for kind in kinds {
            let discriminant = kind.discriminant();
            let evt = ObservabilityEvent::new("c", kind.clone());
            let json = serde_json::to_value(&evt).unwrap();
            assert_eq!(json["kind"], discriminant);
            let back: ObservabilityEvent = serde_json::from_value(json).unwrap();
            assert_eq!(back.kind, kind);
        }
    }

    #[test]
    fn compose_events_are_classified() {
        let event = EventKind::ComposeLoopIteration {
            kernel_id: "kernel".into(),
            iteration: 4,
            skill_id: None,
            confidence: None,
        };

        assert!(event.is_compose_related());
        assert!(!event.is_prompt_related());
        assert!(!event.is_tool_related());
        assert!(!event.is_memory_related());
    }
}
