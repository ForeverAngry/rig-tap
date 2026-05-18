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
//! - When a *prior* hook returns `Skip` or `Terminate`, the kernel
//!   short-circuits before this hook runs. Compose this hook **after**
//!   gating hooks (e.g. budget, policy) so it observes the gate decision
//!   via [`crate::ChainedHook`]-style coverage at the agent layer, or
//!   place it **first** so the kernel never reaches the gate and you
//!   instead emit a synthetic `tool.skipped` via the
//!   [`DispatchObserveHook::skip_with_reason`] helper from your gating
//!   hook.
//! - `on_invocation_error` emits `tool.terminated` paired by `call_id`
//!   for the invocation that just failed.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use rig_compose::{
    KernelError, ToolDispatchAction, ToolDispatchHook, ToolInvocation, ToolInvocationResult,
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
/// `call_id` is held in a `Mutex<Option<String>>`; the kernel guarantees
/// sequential dispatch, so at most one is pending at a time.
///
/// # Example
///
/// ```no_run
/// use rig_compose::{
///     LocalTool, ToolRegistry, ToolSchema, ToolInvocation,
///     dispatch_tool_invocations_with_hooks,
/// };
/// use rig_observe::DispatchObserveHook;
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
    in_flight: Mutex<Option<String>>,
}

static INSTANCE_SEQ: AtomicU64 = AtomicU64::new(0);

impl DispatchObserveHook {
    /// Build a hook stamping events with `conversation_id` and the default
    /// payload truncation threshold.
    pub fn new(conversation_id: impl Into<String>) -> Self {
        Self {
            conversation_id: conversation_id.into(),
            payload_truncate_bytes: PAYLOAD_TRUNCATE_BYTES,
            instance: INSTANCE_SEQ.fetch_add(1, Ordering::Relaxed),
            counter: AtomicU64::new(0),
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

    fn take_in_flight(&self) -> Option<String> {
        match self.in_flight.lock() {
            Ok(mut guard) => guard.take(),
            Err(poison) => poison.into_inner().take(),
        }
    }

    fn set_in_flight(&self, call_id: String) {
        match self.in_flight.lock() {
            Ok(mut guard) => *guard = Some(call_id),
            Err(poison) => *poison.into_inner() = Some(call_id),
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
        let args_raw = serde_json::to_string(&invocation.args).unwrap_or_default();
        let (args_json, truncated) = truncate_utf8(&args_raw, self.payload_truncate_bytes);
        emit_kind(
            self.conversation_id.clone(),
            EventKind::ToolInvoked {
                tool_name: invocation.name.to_string(),
                provider_call_id: None,
                call_id: call_id.clone(),
                args_json,
                truncated,
            },
        );
        self.set_in_flight(call_id);
        Ok(ToolDispatchAction::Continue)
    }

    async fn after_invocation(&self, result: &ToolInvocationResult) -> Result<(), KernelError> {
        let call_id = self.take_in_flight().unwrap_or_else(|| self.next_call_id());
        let result_raw = serde_json::to_string(&result.output).unwrap_or_default();
        let (result_text, truncated) = truncate_utf8(&result_raw, self.payload_truncate_bytes);
        emit_kind(
            self.conversation_id.clone(),
            EventKind::ToolCompleted {
                tool_name: result.invocation.name.to_string(),
                provider_call_id: None,
                call_id,
                result: result_text,
                truncated,
            },
        );
        Ok(())
    }

    async fn on_invocation_error(
        &self,
        invocation: &ToolInvocation,
        error: &KernelError,
    ) -> Result<(), KernelError> {
        let call_id = self.take_in_flight().unwrap_or_else(|| self.next_call_id());
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
