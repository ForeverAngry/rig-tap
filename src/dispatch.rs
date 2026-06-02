//! [`DispatchObserveHook`]: a [`rig_compose::ToolDispatchHook`] that emits
//! `tool.*` [`ObservabilityEvent`](crate::ObservabilityEvent)s for the
//! kernel-direct dispatch path.
//!
//! The [`crate::TelemetryHook`] only fires when the caller drives tool
//! invocations through a Rig agent (which routes through `PromptHook`).
//! Callers that hand-build invocations with
//! [`rig_compose::dispatch_tool_invocations_with_hooks`] never trigger
//! that path. This hook closes that gap with the same wire shape and
//! `call_id` correlation:
//!
//! - `before_invocation` emits `tool.invoked`.
//! - `after_invocation` emits `tool.completed` paired by `call_id`.
//! - Hook-provided skip results emit `tool.skipped` with the dispatch
//!   outcome reason when available. If this observer did not see
//!   `before_invocation` because an earlier hook skipped, it first emits a
//!   synthetic `tool.invoked` so the `call_id` remains pairable.
//! - `on_invocation_error` emits `tool.terminated` paired by `call_id`
//!   for the invocation that just failed.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use async_trait::async_trait;
use rig_compose::{
    Agent, AgentLifecycleHook, AgentStepResult, GenericAgent, InvestigationContext, KernelError,
    SkillOutcome, ToolDispatchAction, ToolDispatchHook, ToolInvocation, ToolInvocationOutcome,
    ToolInvocationResult,
};

use crate::emit::emit_kind;
use crate::event::{EventKind, PAYLOAD_TRUNCATE_BYTES, truncate_utf8};

/// `rig_compose::ToolDispatchHook` that mirrors [`crate::TelemetryHook`]'s
/// emission shape for kernel-direct dispatch.
///
/// # Correlation
///
/// `call_id`s are synthesized as `dispatch-{instance}-{counter}` where
/// `instance` is a per-process unique id assigned at construction and
/// `counter` increments per invocation. The most recent in-flight
/// `call_id` is held in a `Mutex<Option<(String, Instant)>>` alongside the
/// invocation start time; the kernel guarantees sequential dispatch, so at
/// most one is pending at a time. The start time is used to stamp
/// `duration_ms` on the paired `tool.completed`.
///
/// # Example
///
/// ```no_run
/// use rig_compose::{
///     LocalTool, ToolRegistry, ToolSchema, ToolInvocation,
///     dispatch_tool_invocations_with_hooks,
/// };
/// use rig_tap::DispatchObserveHook;
/// use serde_json::json;
///
/// # async fn run() -> Result<(), Box<dyn std::error::Error>> {
/// let tools = ToolRegistry::new();
/// let observe = DispatchObserveHook::new("conv-1");
/// let invs = vec![ToolInvocation::new("noop", json!({}))?];
/// let _ = dispatch_tool_invocations_with_hooks(&tools, &invs, &[&observe]).await?;
/// # Ok(()) }
/// ```
pub struct DispatchObserveHook {
    conversation_id: String,
    payload_truncate_bytes: usize,
    instance: u64,
    counter: AtomicU64,
    loop_counter: AtomicU64,
    kernel_started: AtomicBool,
    /// Cached kernel id captured on the first lifecycle event so `Drop`
    /// can emit `compose.kernel_shutdown` without re-borrowing the agent.
    kernel_id_cache: Mutex<Option<String>>,
    in_flight: Mutex<Option<(String, Instant)>>,
}

static INSTANCE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Convert an elapsed [`Instant`] into whole milliseconds, saturating at
/// [`u64::MAX`] rather than panicking or wrapping on absurdly long spans.
fn elapsed_millis(started_at: Instant) -> u64 {
    u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
}

impl DispatchObserveHook {
    /// Build a hook stamping events with `conversation_id` and the default
    /// payload truncation threshold.
    pub fn new(conversation_id: impl Into<String>) -> Self {
        Self {
            conversation_id: conversation_id.into(),
            payload_truncate_bytes: PAYLOAD_TRUNCATE_BYTES,
            instance: INSTANCE_SEQ.fetch_add(1, Ordering::Relaxed),
            counter: AtomicU64::new(0),
            loop_counter: AtomicU64::new(0),
            kernel_started: AtomicBool::new(false),
            kernel_id_cache: Mutex::new(None),
            in_flight: Mutex::new(None),
        }
    }

    /// Override the payload truncation threshold applied to inline args
    /// and tool results.
    #[must_use]
    pub fn with_payload_truncate_bytes(mut self, bytes: usize) -> Self {
        self.payload_truncate_bytes = bytes;
        self
    }

    /// Wrap `self` in an [`Arc`] for sharing across threads or registries
    /// that want a `'static` reference.
    #[must_use]
    pub fn into_arc(self) -> Arc<Self> {
        Arc::new(self)
    }

    fn next_call_id(&self) -> String {
        let n = self.counter.fetch_add(1, Ordering::Relaxed);
        format!("dispatch-{}-{}", self.instance, n)
    }

    fn next_iteration(&self) -> u64 {
        self.loop_counter.fetch_add(1, Ordering::Relaxed)
    }

    fn kernel_id(agent: &GenericAgent) -> String {
        agent.id().to_string()
    }

    /// Cache the kernel id seen on the first lifecycle event so `Drop`
    /// can emit `compose.kernel_shutdown` without needing an agent ref.
    fn cache_kernel_id(&self, kernel_id: &str) {
        let mut guard = match self.kernel_id_cache.lock() {
            Ok(guard) => guard,
            Err(poison) => poison.into_inner(),
        };
        if guard.is_none() {
            *guard = Some(kernel_id.to_string());
        }
    }

    fn take_in_flight(&self) -> Option<(String, Instant)> {
        match self.in_flight.lock() {
            Ok(mut guard) => guard.take(),
            Err(poison) => poison.into_inner().take(),
        }
    }

    fn set_in_flight(&self, call_id: String) {
        let entry = (call_id, Instant::now());
        match self.in_flight.lock() {
            Ok(mut guard) => *guard = Some(entry),
            Err(poison) => *poison.into_inner() = Some(entry),
        }
    }

    fn emit_invoked(&self, invocation: &ToolInvocation, call_id: &str) {
        let args_raw = serde_json::to_string(&invocation.args).unwrap_or_default();
        let (args_json, truncated) = truncate_utf8(&args_raw, self.payload_truncate_bytes);
        emit_kind(
            self.conversation_id.clone(),
            EventKind::ToolInvoked {
                tool_name: invocation.name.to_string(),
                provider_call_id: None,
                call_id: call_id.to_string(),
                args_json,
                truncated,
            },
        );
    }

    fn call_id_for_after(&self, invocation: &ToolInvocation) -> (String, Option<Instant>) {
        match self.take_in_flight() {
            Some((call_id, started_at)) => (call_id, Some(started_at)),
            None => {
                let call_id = self.next_call_id();
                self.emit_invoked(invocation, &call_id);
                (call_id, None)
            }
        }
    }

    fn call_id_for_error(&self, invocation: &ToolInvocation) -> String {
        match self.take_in_flight() {
            Some((call_id, _started_at)) => call_id,
            None => {
                let call_id = self.next_call_id();
                self.emit_invoked(invocation, &call_id);
                call_id
            }
        }
    }
}

#[async_trait]
impl AgentLifecycleHook for DispatchObserveHook {
    async fn before_step(
        &self,
        agent: &GenericAgent,
        _ctx: &InvestigationContext,
    ) -> Result<(), KernelError> {
        let kernel_id = Self::kernel_id(agent);
        self.cache_kernel_id(&kernel_id);
        if !self.kernel_started.swap(true, Ordering::AcqRel) {
            // Reset the per-kernel iteration counter so the first
            // `compose.loop_iteration` after start is always `iteration = 0`.
            self.loop_counter.store(0, Ordering::Release);
            emit_kind(
                self.conversation_id.clone(),
                EventKind::ComposeKernelStart {
                    kernel_id: kernel_id.clone(),
                    skills_registered: Some(agent.skills().len()),
                    tools_registered: Some(agent.tools().len()),
                },
            );
        }
        // One `compose.loop_iteration` event per agent step. Per-skill
        // resolution is reported via `compose.skill_resolved` only.
        emit_kind(
            self.conversation_id.clone(),
            EventKind::ComposeLoopIteration {
                kernel_id,
                iteration: self.next_iteration(),
                skill_id: None,
                confidence: None,
            },
        );
        Ok(())
    }

    async fn before_skill(
        &self,
        agent: &GenericAgent,
        skill_id: &str,
        applies: bool,
        confidence: f32,
    ) -> Result<(), KernelError> {
        if !applies {
            emit_kind(
                self.conversation_id.clone(),
                EventKind::ComposeSkillResolved {
                    kernel_id: Self::kernel_id(agent),
                    skill_id: skill_id.to_string(),
                    applies: false,
                    delta: None,
                    confidence: Some(f64::from(confidence)),
                },
            );
        }
        Ok(())
    }

    async fn after_skill(
        &self,
        agent: &GenericAgent,
        skill_id: &str,
        outcome: &SkillOutcome,
        confidence: f32,
    ) -> Result<(), KernelError> {
        emit_kind(
            self.conversation_id.clone(),
            EventKind::ComposeSkillResolved {
                kernel_id: Self::kernel_id(agent),
                skill_id: skill_id.to_string(),
                applies: true,
                delta: Some(f64::from(outcome.confidence_delta)),
                confidence: Some(f64::from(confidence)),
            },
        );
        Ok(())
    }

    async fn after_step(
        &self,
        _agent: &GenericAgent,
        _result: &AgentStepResult,
    ) -> Result<(), KernelError> {
        // No emission: `compose.kernel_shutdown` belongs to lifecycle end,
        // not per-step end. `Drop` emits it once when the hook is released.
        Ok(())
    }

    async fn on_step_error(
        &self,
        agent: &GenericAgent,
        error: &KernelError,
    ) -> Result<(), KernelError> {
        // A failing step is a recovery event, not a kernel shutdown. The
        // outer driver decides whether to keep the loop alive.
        emit_kind(
            self.conversation_id.clone(),
            EventKind::ComposeRecovery {
                kernel_id: Self::kernel_id(agent),
                reason: error.to_string(),
                recovered: false,
            },
        );
        Ok(())
    }
}

impl Drop for DispatchObserveHook {
    fn drop(&mut self) {
        // Only emit `compose.kernel_shutdown` if the kernel actually started.
        // The hook may be constructed but never bound to an agent.
        if !self.kernel_started.load(Ordering::Acquire) {
            return;
        }
        let kernel_id = match self.kernel_id_cache.get_mut() {
            Ok(slot) => slot.take(),
            Err(poison) => poison.into_inner().take(),
        };
        if let Some(kernel_id) = kernel_id {
            emit_kind(
                self.conversation_id.clone(),
                EventKind::ComposeKernelShutdown {
                    kernel_id,
                    reason: "normal".to_string(),
                },
            );
        }
    }
}

impl std::fmt::Debug for DispatchObserveHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DispatchObserveHook")
            .field("conversation_id", &self.conversation_id)
            .field("payload_truncate_bytes", &self.payload_truncate_bytes)
            .field("instance", &self.instance)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl ToolDispatchHook for DispatchObserveHook {
    async fn before_invocation(
        &self,
        invocation: &ToolInvocation,
    ) -> Result<ToolDispatchAction, KernelError> {
        let call_id = self.next_call_id();
        self.emit_invoked(invocation, &call_id);
        self.set_in_flight(call_id);
        Ok(ToolDispatchAction::Continue)
    }

    async fn after_invocation_with_outcome(
        &self,
        result: &ToolInvocationResult,
        outcome: &ToolInvocationOutcome,
    ) -> Result<(), KernelError> {
        let (call_id, started_at) = self.call_id_for_after(&result.invocation);
        match outcome {
            ToolInvocationOutcome::Completed => {
                let result_raw = serde_json::to_string(&result.output).unwrap_or_default();
                let (result_text, truncated) =
                    truncate_utf8(&result_raw, self.payload_truncate_bytes);
                emit_kind(
                    self.conversation_id.clone(),
                    EventKind::ToolCompleted {
                        tool_name: result.invocation.name.to_string(),
                        provider_call_id: None,
                        call_id,
                        result: result_text,
                        truncated,
                        duration_ms: started_at.map(elapsed_millis),
                    },
                );
            }
            ToolInvocationOutcome::Skipped { reason } => {
                emit_kind(
                    self.conversation_id.clone(),
                    EventKind::ToolSkipped {
                        tool_name: result.invocation.name.to_string(),
                        call_id,
                        reason: reason.clone().unwrap_or_else(|| "skipped".to_string()),
                    },
                );
            }
        }
        Ok(())
    }

    async fn on_invocation_error(
        &self,
        invocation: &ToolInvocation,
        error: &KernelError,
    ) -> Result<(), KernelError> {
        let call_id = self.call_id_for_error(invocation);
        let reason = error.to_string();
        // The kernel calls `on_invocation_error` for both gate-driven
        // `Terminate` actions (surfaced as `KernelError::ToolDispatchTerminated`)
        // and runtime tool failures. Both map to `tool.terminated` here;
        // downstream consumers can distinguish them by inspecting `reason`
        // if they need to.
        emit_kind(
            self.conversation_id.clone(),
            EventKind::ToolTerminated {
                tool_name: invocation.name.to_string(),
                call_id,
                reason,
            },
        );
        Ok(())
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
    fn call_ids_are_unique_per_instance() {
        let hook = DispatchObserveHook::new("c");
        let a = hook.next_call_id();
        let b = hook.next_call_id();
        assert_ne!(a, b);
        assert!(a.starts_with(&format!("dispatch-{}-", hook.instance)));
    }

    #[test]
    fn instances_get_different_ids() {
        let a = DispatchObserveHook::new("c");
        let b = DispatchObserveHook::new("c");
        assert_ne!(a.instance, b.instance);
    }
}
