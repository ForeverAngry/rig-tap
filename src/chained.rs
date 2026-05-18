//! [`ChainedHook`]: compose two [`PromptHook`]s on a single agent.
//!
//! Rig's `agent.with_hook(...)` slot only accepts one [`PromptHook`] value.
//! When you want to combine, say, [`crate::TelemetryHook`] with a persistence
//! hook like `rig_memvid::MemvidPersistHook`, wrap them in a `ChainedHook`.
//!
//! # Combination semantics
//!
//! - `HookAction`: if either inner hook returns `Terminate { reason }`, the
//!   combined action is `Terminate { reason }` (reasons concatenated when
//!   both terminate). Otherwise `Continue`.
//! - `ToolCallHookAction`: `Terminate` > `Skip` > `Continue`. If both hooks
//!   produce the same severity, reasons are concatenated with a `" | "`
//!   separator.
//!
//! Both hooks are *always* invoked. A `Skip` from `A` does not short-circuit
//! `B`'s `on_tool_call` — telemetry still gets to record the call attempt
//! when paired with a gating hook.

use rig::agent::{HookAction, PromptHook, ToolCallHookAction};
use rig::completion::{CompletionModel, CompletionResponse, Message};

use crate::emit::emit_kind;
use crate::event::EventKind;

/// Combine two [`PromptHook`]s into one. See module docs for combination
/// semantics.
///
/// When [`ChainedHook::observe_with`] sets a conversation ID, the chain
/// also emits synthetic `tool.skipped` / `tool.terminated` events for tool
/// calls whose combined action is `Skip` / `Terminate`. This closes the
/// `tool.invoked` / `tool.completed` correlation gap when chaining a
/// telemetry hook with a gating hook.
#[derive(Debug, Clone)]
pub struct ChainedHook<A, B> {
    a: A,
    b: B,
    observe_conversation_id: Option<String>,
}

impl<A, B> ChainedHook<A, B> {
    /// Build a chained hook running `a` before `b` for every lifecycle event.
    /// By default the chain emits no synthetic terminal events; use
    /// [`ChainedHook::observe_with`] to opt in.
    pub fn new(a: A, b: B) -> Self {
        Self {
            a,
            b,
            observe_conversation_id: None,
        }
    }

    /// Opt in to synthetic `tool.skipped` / `tool.terminated` event emission
    /// stamped with `conversation_id`. Use the same conversation ID configured
    /// on the chain's telemetry hook so pair-correlation by `call_id` works
    /// end-to-end.
    #[must_use]
    pub fn observe_with(mut self, conversation_id: impl Into<String>) -> Self {
        self.observe_conversation_id = Some(conversation_id.into());
        self
    }
}

impl<A, B, M> PromptHook<M> for ChainedHook<A, B>
where
    A: PromptHook<M>,
    B: PromptHook<M>,
    M: CompletionModel,
{
    async fn on_completion_call(&self, prompt: &Message, history: &[Message]) -> HookAction {
        let a = self.a.on_completion_call(prompt, history).await;
        let b = self.b.on_completion_call(prompt, history).await;
        combine_actions(a, b)
    }

    async fn on_completion_response(
        &self,
        prompt: &Message,
        response: &CompletionResponse<M::Response>,
    ) -> HookAction {
        let a = self.a.on_completion_response(prompt, response).await;
        let b = self.b.on_completion_response(prompt, response).await;
        combine_actions(a, b)
    }

    async fn on_tool_call(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> ToolCallHookAction {
        let a = self
            .a
            .on_tool_call(tool_name, tool_call_id.clone(), internal_call_id, args)
            .await;
        let b = self
            .b
            .on_tool_call(tool_name, tool_call_id, internal_call_id, args)
            .await;
        let combined = combine_tool_actions(a, b);
        if let Some(conversation_id) = self.observe_conversation_id.as_deref() {
            match &combined {
                ToolCallHookAction::Continue => {}
                ToolCallHookAction::Skip { reason } => emit_kind(
                    conversation_id,
                    EventKind::ToolSkipped {
                        tool_name: tool_name.to_string(),
                        call_id: internal_call_id.to_string(),
                        reason: reason.clone(),
                    },
                ),
                ToolCallHookAction::Terminate { reason } => emit_kind(
                    conversation_id,
                    EventKind::ToolTerminated {
                        tool_name: tool_name.to_string(),
                        call_id: internal_call_id.to_string(),
                        reason: reason.clone(),
                    },
                ),
            }
        }
        combined
    }

    async fn on_tool_result(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
        result: &str,
    ) -> HookAction {
        let a = self
            .a
            .on_tool_result(
                tool_name,
                tool_call_id.clone(),
                internal_call_id,
                args,
                result,
            )
            .await;
        let b = self
            .b
            .on_tool_result(tool_name, tool_call_id, internal_call_id, args, result)
            .await;
        combine_actions(a, b)
    }
}

fn combine_actions(a: HookAction, b: HookAction) -> HookAction {
    match (a, b) {
        (HookAction::Continue, HookAction::Continue) => HookAction::Continue,
        (HookAction::Terminate { reason }, HookAction::Continue)
        | (HookAction::Continue, HookAction::Terminate { reason }) => {
            HookAction::Terminate { reason }
        }
        (HookAction::Terminate { reason: ra }, HookAction::Terminate { reason: rb }) => {
            HookAction::Terminate {
                reason: join_reasons(&ra, &rb),
            }
        }
    }
}

fn combine_tool_actions(a: ToolCallHookAction, b: ToolCallHookAction) -> ToolCallHookAction {
    use ToolCallHookAction as T;
    match (a, b) {
        (T::Continue, T::Continue) => T::Continue,

        // Skip beats Continue.
        (T::Skip { reason }, T::Continue) | (T::Continue, T::Skip { reason }) => T::Skip { reason },
        (T::Skip { reason: ra }, T::Skip { reason: rb }) => T::Skip {
            reason: join_reasons(&ra, &rb),
        },

        // Terminate beats everything.
        (T::Terminate { reason }, T::Continue)
        | (T::Continue, T::Terminate { reason })
        | (T::Terminate { reason }, T::Skip { .. })
        | (T::Skip { .. }, T::Terminate { reason }) => T::Terminate { reason },
        (T::Terminate { reason: ra }, T::Terminate { reason: rb }) => T::Terminate {
            reason: join_reasons(&ra, &rb),
        },
    }
}

fn join_reasons(a: &str, b: &str) -> String {
    if a.is_empty() {
        b.to_string()
    } else if b.is_empty() {
        a.to_string()
    } else {
        format!("{a} | {b}")
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
    fn continue_dominates_when_both_continue() {
        let combined = combine_actions(HookAction::Continue, HookAction::Continue);
        assert!(matches!(combined, HookAction::Continue));
    }

    #[test]
    fn terminate_beats_continue() {
        let combined = combine_actions(
            HookAction::Continue,
            HookAction::Terminate {
                reason: "stop".into(),
            },
        );
        match combined {
            HookAction::Terminate { reason } => assert_eq!(reason, "stop"),
            HookAction::Continue => panic!("expected Terminate"),
        }
    }

    #[test]
    fn double_terminate_joins_reasons() {
        let combined = combine_actions(
            HookAction::Terminate { reason: "a".into() },
            HookAction::Terminate { reason: "b".into() },
        );
        match combined {
            HookAction::Terminate { reason } => assert_eq!(reason, "a | b"),
            HookAction::Continue => panic!("expected Terminate"),
        }
    }

    #[test]
    fn tool_terminate_beats_skip() {
        let combined = combine_tool_actions(
            ToolCallHookAction::Skip {
                reason: "policy".into(),
            },
            ToolCallHookAction::Terminate {
                reason: "abort".into(),
            },
        );
        match combined {
            ToolCallHookAction::Terminate { reason } => assert_eq!(reason, "abort"),
            other => panic!("expected Terminate, got {other:?}"),
        }
    }

    #[test]
    fn tool_skip_beats_continue() {
        let combined = combine_tool_actions(
            ToolCallHookAction::Skip {
                reason: "policy".into(),
            },
            ToolCallHookAction::Continue,
        );
        match combined {
            ToolCallHookAction::Skip { reason } => assert_eq!(reason, "policy"),
            other => panic!("expected Skip, got {other:?}"),
        }
    }
}
