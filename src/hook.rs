//! [`TelemetryHook`]: a [`rig::agent::PromptHook`] that emits
//! `prompt.*` and `tool.*` [`ObservabilityEvent`](crate::ObservabilityEvent)s
//! for every prompt and tool call.

use std::marker::PhantomData;
use std::sync::Arc;

use rig::agent::{HookAction, PromptHook, ToolCallHookAction};
use rig::completion::{CompletionModel, CompletionResponse, Message};

use crate::emit::emit_kind;
use crate::event::{EventKind, PAYLOAD_TRUNCATE_BYTES, truncate_utf8};

/// Caller-supplied resolver for the conversation ID stamped on emitted
/// events. Consulted on every emission; when it returns `Some(id)`, that
/// value wins over [`TelemetryHookConfig::conversation_id`].
///
/// Use this when the host runtime threads a per-request conversation ID
/// through e.g. a task-local, a `tracing::Span` field, or a request-scoped
/// context object. The Rig `PromptHook` signature does not currently
/// propagate a conversation ID; this is the escape hatch.
pub type ConversationIdResolver = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// Caller-supplied resolver that pulls the *actual* model identifier out
/// of a provider response. Useful for routed providers (OpenRouter,
/// Bedrock model-routing, vendor multi-model endpoints) where the model
/// recorded at hook construction is a logical alias and the response's
/// raw payload carries the concrete model that served the request.
///
/// When set and the resolver returns `Some(model)`, that value is stamped
/// on `prompt.completed` instead of [`TelemetryHookConfig::model`].
pub type ModelResolver<R> = Arc<dyn Fn(&CompletionResponse<R>) -> Option<String> + Send + Sync>;

/// Conversation identifier to stamp on emitted events when the agent runtime
/// does not surface one to the hook. The Rig `PromptHook` signature does not
/// currently propagate the conversation ID, so the hook stamps events with a
/// constant chosen by the caller (typically `"default"` for single-thread
/// agents, or a unique value per agent instance for multi-thread setups).
///
/// For per-request resolution see
/// [`TelemetryHook::with_conversation_id_resolver`].
#[derive(Debug, Clone)]
pub struct TelemetryHookConfig {
    /// Default model label (e.g. `"gpt-4o"`) recorded on `prompt.*` events.
    /// For routed providers, prefer [`TelemetryHook::with_model_resolver`]
    /// to extract the model name from the actual response payload.
    pub model: String,
    /// Default conversation ID stamped on every emitted event when no
    /// per-request resolver is registered or the resolver returns `None`.
    pub conversation_id: String,
    /// Maximum byte length of inline `args_json` / `result` payloads before
    /// truncation. Defaults to [`PAYLOAD_TRUNCATE_BYTES`].
    pub payload_truncate_bytes: usize,
}

impl TelemetryHookConfig {
    /// Build a config with the given model label and conversation ID, using
    /// the default truncation threshold.
    pub fn new(model: impl Into<String>, conversation_id: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            conversation_id: conversation_id.into(),
            payload_truncate_bytes: PAYLOAD_TRUNCATE_BYTES,
        }
    }
}

/// Per-request hook that emits structured observability events from the five
/// [`PromptHook`] lifecycle methods.
///
/// `M` is the [`CompletionModel`] used by the agent. The hook is generic so a
/// single `rig-observe` build can attach to OpenAI, Anthropic, Ollama, etc.
///
/// # Example
///
/// ```no_run
/// use rig_observe::{TelemetryHook, TelemetryHookConfig};
///
/// # fn make_hook<M: rig::completion::CompletionModel>() -> TelemetryHook<M> {
/// TelemetryHook::new(TelemetryHookConfig::new("gpt-4o", "thread-1"))
/// # }
/// ```
pub struct TelemetryHook<M: CompletionModel> {
    config: TelemetryHookConfig,
    conversation_id_resolver: Option<ConversationIdResolver>,
    model_resolver: Option<ModelResolver<M::Response>>,
    _model: PhantomData<fn() -> M>,
}

impl<M: CompletionModel> TelemetryHook<M> {
    /// Build a hook from `config`.
    pub fn new(config: TelemetryHookConfig) -> Self {
        Self {
            config,
            conversation_id_resolver: None,
            model_resolver: None,
            _model: PhantomData,
        }
    }

    /// Convenience: build a hook stamping events with `model` and
    /// `conversation_id` and default truncation.
    pub fn with_defaults(model: impl Into<String>, conversation_id: impl Into<String>) -> Self {
        Self::new(TelemetryHookConfig::new(model, conversation_id))
    }

    /// Register a per-request resolver for the conversation ID. The
    /// resolver is consulted on every emission; if it returns `Some(id)`,
    /// that value is stamped on the event instead of
    /// [`TelemetryHookConfig::conversation_id`].
    ///
    /// Typical wiring: a `tokio::task_local!` (or equivalent) set by the
    /// host on every request, read by the closure.
    #[must_use]
    pub fn with_conversation_id_resolver<F>(mut self, resolver: F) -> Self
    where
        F: Fn() -> Option<String> + Send + Sync + 'static,
    {
        self.conversation_id_resolver = Some(Arc::new(resolver));
        self
    }

    /// Register a resolver that extracts the concrete model identifier
    /// from each [`CompletionResponse`]. When the resolver returns
    /// `Some(model)`, that value is stamped on `prompt.completed`
    /// instead of [`TelemetryHookConfig::model`].
    ///
    /// Use this with routed providers (OpenRouter, Bedrock routing,
    /// vendor multi-model endpoints) where the configured model name is
    /// a logical alias and the response payload carries the actual
    /// model that served the request.
    #[must_use]
    pub fn with_model_resolver<F>(mut self, resolver: F) -> Self
    where
        F: Fn(&CompletionResponse<M::Response>) -> Option<String> + Send + Sync + 'static,
    {
        self.model_resolver = Some(Arc::new(resolver));
        self
    }

    fn resolved_conversation_id(&self) -> String {
        self.conversation_id_resolver
            .as_ref()
            .and_then(|f| f())
            .unwrap_or_else(|| self.config.conversation_id.clone())
    }

    fn resolved_model(&self, response: &CompletionResponse<M::Response>) -> String {
        self.model_resolver
            .as_ref()
            .and_then(|f| f(response))
            .unwrap_or_else(|| self.config.model.clone())
    }
}

impl<M: CompletionModel> Clone for TelemetryHook<M> {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            conversation_id_resolver: self.conversation_id_resolver.clone(),
            model_resolver: self.model_resolver.clone(),
            _model: PhantomData,
        }
    }
}

impl<M: CompletionModel> std::fmt::Debug for TelemetryHook<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelemetryHook")
            .field("config", &self.config)
            .field(
                "conversation_id_resolver",
                &self.conversation_id_resolver.as_ref().map(|_| "<fn>"),
            )
            .field(
                "model_resolver",
                &self.model_resolver.as_ref().map(|_| "<fn>"),
            )
            .finish_non_exhaustive()
    }
}

impl<M> PromptHook<M> for TelemetryHook<M>
where
    M: CompletionModel,
{
    async fn on_completion_call(&self, _prompt: &Message, history: &[Message]) -> HookAction {
        // `messages_in` counts the prompt + prior history that will be sent
        // to the provider.
        let messages_in = history.len().saturating_add(1);
        emit_kind(
            self.resolved_conversation_id(),
            EventKind::PromptStarted {
                model: self.config.model.clone(),
                messages_in,
            },
        );
        HookAction::cont()
    }

    async fn on_completion_response(
        &self,
        _prompt: &Message,
        response: &CompletionResponse<M::Response>,
    ) -> HookAction {
        let usage = response.usage;
        emit_kind(
            self.resolved_conversation_id(),
            EventKind::PromptCompleted {
                model: self.resolved_model(response),
                tokens_in: positive(usage.input_tokens),
                tokens_out: positive(usage.output_tokens),
                response_id: response.message_id.clone(),
            },
        );
        HookAction::cont()
    }

    async fn on_tool_call(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> ToolCallHookAction {
        let (args_json, truncated) = truncate_utf8(args, self.config.payload_truncate_bytes);
        emit_kind(
            self.resolved_conversation_id(),
            EventKind::ToolInvoked {
                tool_name: tool_name.to_string(),
                provider_call_id: tool_call_id,
                call_id: internal_call_id.to_string(),
                args_json,
                truncated,
            },
        );
        ToolCallHookAction::cont()
    }

    async fn on_tool_result(
        &self,
        tool_name: &str,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        _args: &str,
        result: &str,
    ) -> HookAction {
        let (result, truncated) = truncate_utf8(result, self.config.payload_truncate_bytes);
        emit_kind(
            self.resolved_conversation_id(),
            EventKind::ToolCompleted {
                tool_name: tool_name.to_string(),
                provider_call_id: tool_call_id,
                call_id: internal_call_id.to_string(),
                result,
                truncated,
            },
        );
        HookAction::cont()
    }
}

fn positive(value: u64) -> Option<u64> {
    if value == 0 { None } else { Some(value) }
}
