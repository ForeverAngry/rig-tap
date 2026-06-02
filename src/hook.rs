//! [`TelemetryHook`]: a [`rig::agent::PromptHook`] that emits
//! `prompt.*` and `tool.*` [`ObservabilityEvent`](crate::ObservabilityEvent)s
//! for every prompt and tool call.

use std::marker::PhantomData;
use std::sync::Arc;

use rig::agent::{HookAction, PromptHook, ToolCallHookAction};
use rig::completion::{CompletionModel, CompletionResponse, Message};

use crate::emit::emit_kind;
use crate::event::{EventKind, PAYLOAD_TRUNCATE_BYTES, truncate_utf8};
use crate::sampling::{AlwaysSample, SamplingPolicy};

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

/// Caller-supplied resolver that returns the chain ancestor for the current
/// turn (the `previous_response_id` argument sent to the provider) so it can
/// be stamped on `prompt.completed`.
///
/// Useful for stateful endpoints — OpenAI Responses, future Anthropic and
/// Google equivalents — where the host runtime tracks the chain itself
/// (typically in a task-local or session object) and the provider response
/// payload does not echo the value back. The Rig `PromptHook` signature
/// does not currently propagate it, so this is the escape hatch.
///
/// When set and the resolver returns `Some(id)`, that value is stamped on
/// `prompt.completed`; `None` leaves the field unset.
pub type PreviousResponseIdResolver<R> =
    Arc<dyn Fn(&CompletionResponse<R>) -> Option<String> + Send + Sync>;

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
/// single `rig-tap` build can attach to OpenAI, Anthropic, Ollama, etc.
///
/// # Example
///
/// ```no_run
/// use rig_tap::{TelemetryHook, TelemetryHookConfig};
///
/// # fn make_hook<M: rig::completion::CompletionModel>() -> TelemetryHook<M> {
/// TelemetryHook::new(TelemetryHookConfig::new("gpt-4o", "thread-1"))
/// # }
/// ```
pub struct TelemetryHook<M: CompletionModel> {
    config: TelemetryHookConfig,
    conversation_id_resolver: Option<ConversationIdResolver>,
    model_resolver: Option<ModelResolver<M::Response>>,
    previous_response_id_resolver: Option<PreviousResponseIdResolver<M::Response>>,
    sampling: Arc<dyn SamplingPolicy>,
    _model: PhantomData<fn() -> M>,
}

impl<M: CompletionModel> TelemetryHook<M> {
    /// Build a hook from `config`.
    pub fn new(config: TelemetryHookConfig) -> Self {
        Self {
            config,
            conversation_id_resolver: None,
            model_resolver: None,
            previous_response_id_resolver: None,
            sampling: Arc::new(AlwaysSample),
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

    /// Register a resolver that returns the chain ancestor
    /// (`previous_response_id`) sent to the provider for the current turn.
    /// When the resolver returns `Some(id)`, that value is stamped on
    /// `prompt.completed`'s `previous_response_id` field.
    ///
    /// Use this with stateful endpoints — OpenAI Responses, future
    /// Anthropic/Google equivalents — where the host runtime tracks the
    /// chain (typically in a task-local or session object) and the
    /// provider response payload does not echo the value back.
    #[must_use]
    pub fn with_previous_response_id_resolver<F>(mut self, resolver: F) -> Self
    where
        F: Fn(&CompletionResponse<M::Response>) -> Option<String> + Send + Sync + 'static,
    {
        self.previous_response_id_resolver = Some(Arc::new(resolver));
        self
    }

    /// Install a [`SamplingPolicy`] that gates every `prompt.*` and
    /// `tool.*` emission from this hook. The default policy is
    /// [`AlwaysSample`](crate::AlwaysSample).
    ///
    /// Pairing: the hook passes the resolved conversation id as the
    /// correlator for `prompt.*` events and the internal call id for
    /// `tool.*` events. Policies that hash the correlator (such as
    /// [`RatePolicy`](crate::RatePolicy)) therefore keep
    /// `tool.invoked` / `tool.completed` pairs coherent
    /// automatically.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use std::sync::Arc;
    /// use rig_tap::{RatePolicy, TelemetryHook, TelemetryHookConfig};
    ///
    /// # fn make_hook<M: rig::completion::CompletionModel>() -> TelemetryHook<M> {
    /// TelemetryHook::new(TelemetryHookConfig::new("gpt-4o", "thread-1"))
    ///     .with_sampling_policy(Arc::new(
    ///         RatePolicy::new()
    ///             .with_rate("tool.invoked", 0.1)
    ///             .with_rate("tool.completed", 0.1),
    ///     ))
    /// # }
    /// ```
    #[must_use]
    pub fn with_sampling_policy(mut self, policy: Arc<dyn SamplingPolicy>) -> Self {
        self.sampling = policy;
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

    fn resolved_previous_response_id(
        &self,
        response: &CompletionResponse<M::Response>,
    ) -> Option<String> {
        self.previous_response_id_resolver
            .as_ref()
            .and_then(|f| f(response))
    }

    /// Observe a failure in the prompt loop. Call this when the agent's
    /// prompt execution returns an error.
    pub fn observe_prompt_error(&self, error: &rig::completion::PromptError) {
        let conversation_id = self.resolved_conversation_id();
        if !self
            .sampling
            .should_sample("prompt.failed", &conversation_id)
        {
            return;
        }

        let (error_class, retriable, provider_error_code, http_status) = map_prompt_error(error);

        crate::emit::emit_kind(
            conversation_id,
            crate::event::EventKind::PromptFailed {
                model: self.config.model.clone(),
                error_class,
                message: error.to_string(),
                retriable,
                provider_error_code,
                http_status,
            },
        );
    }

    /// Observe a failure in a tool invocation. Call this when a tool
    /// returns a failure.
    pub fn observe_tool_error(
        &self,
        tool_name: &str,
        call_id: &str,
        error: &dyn std::error::Error,
    ) {
        let conversation_id = self.resolved_conversation_id();
        if !self.sampling.should_sample("tool.failed", call_id) {
            return;
        }

        crate::emit::emit_kind(
            conversation_id,
            crate::event::EventKind::ToolFailed {
                tool_name: tool_name.to_string(),
                call_id: call_id.to_string(),
                error_class: crate::event::ErrorClass::Unknown,
                message: error.to_string(),
            },
        );
    }
}

impl<M: CompletionModel> Clone for TelemetryHook<M> {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            conversation_id_resolver: self.conversation_id_resolver.clone(),
            model_resolver: self.model_resolver.clone(),
            previous_response_id_resolver: self.previous_response_id_resolver.clone(),
            sampling: self.sampling.clone(),
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
            .field(
                "previous_response_id_resolver",
                &self.previous_response_id_resolver.as_ref().map(|_| "<fn>"),
            )
            .field("sampling", &self.sampling)
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
        let conversation_id = self.resolved_conversation_id();
        if self
            .sampling
            .should_sample("prompt.started", &conversation_id)
        {
            emit_kind(
                conversation_id,
                EventKind::PromptStarted {
                    model: self.config.model.clone(),
                    messages_in,
                },
            );
        }
        HookAction::cont()
    }

    async fn on_completion_response(
        &self,
        _prompt: &Message,
        response: &CompletionResponse<M::Response>,
    ) -> HookAction {
        let usage = response.usage;
        let conversation_id = self.resolved_conversation_id();
        if self
            .sampling
            .should_sample("prompt.completed", &conversation_id)
        {
            emit_kind(
                conversation_id,
                EventKind::PromptCompleted {
                    model: self.resolved_model(response),
                    tokens_in: positive(usage.input_tokens),
                    tokens_out: positive(usage.output_tokens),
                    cached_tokens_in: positive(usage.cached_input_tokens),
                    reasoning_tokens: positive(usage.reasoning_tokens),
                    cost_usd: None,
                    finish_reason: None,
                    response_id: response.message_id.clone(),
                    previous_response_id: self.resolved_previous_response_id(response),
                    // `time_to_first_token_ms` and `duration_ms` are left unset
                    // here: the Rig `PromptHook` signature delivers
                    // `on_completion_call` and `on_completion_response` as two
                    // separate `&self` invocations on a shared, cloneable hook
                    // with no per-prompt correlation key, so the hook cannot
                    // safely own both ends of the pair. Latency is stamped by
                    // streaming / stateful producers that do — see
                    // [`crate::responses_session::ResponsesSessionObserver`].
                    time_to_first_token_ms: None,
                    duration_ms: None,
                },
            );
        }
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
        if self
            .sampling
            .should_sample("tool.invoked", internal_call_id)
        {
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
        }
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
        if self
            .sampling
            .should_sample("tool.completed", internal_call_id)
        {
            emit_kind(
                self.resolved_conversation_id(),
                EventKind::ToolCompleted {
                    tool_name: tool_name.to_string(),
                    provider_call_id: tool_call_id,
                    call_id: internal_call_id.to_string(),
                    result,
                    truncated,
                    // Unset here for the same reason as `prompt.completed`:
                    // the `PromptHook` tool pair spans two `&self` calls on a
                    // shared hook. The kernel-direct dispatch observer
                    // ([`crate::DispatchObserveHook`]) owns both ends and does
                    // stamp `duration_ms`.
                    duration_ms: None,
                },
            );
        }
        HookAction::cont()
    }
}

fn positive(value: u64) -> Option<u64> {
    if value == 0 { None } else { Some(value) }
}

fn map_prompt_error(
    err: &rig::completion::PromptError,
) -> (crate::event::ErrorClass, bool, Option<String>, Option<u16>) {
    match err {
        rig::completion::PromptError::CompletionError(e) => map_completion_error(e),
        // A failure indicating the tool itself returned an error or the agent
        // hallucinates an invalid tool format.
        rig::completion::PromptError::ToolError(_) => {
            (crate::event::ErrorClass::Validation, false, None, None)
        }
        _ => (crate::event::ErrorClass::Unknown, false, None, None),
    }
}

fn map_completion_error(
    err: &rig::completion::CompletionError,
) -> (crate::event::ErrorClass, bool, Option<String>, Option<u16>) {
    use crate::event::ErrorClass;
    match err {
        rig::completion::CompletionError::HttpError(http_err) => {
            let status = match http_err {
                rig::http_client::Error::InvalidStatusCode(s) => Some(s.as_u16()),
                rig::http_client::Error::InvalidStatusCodeWithMessage(s, _) => Some(s.as_u16()),
                _ => None,
            };

            let (class, retriable) = match status {
                Some(401 | 403) => (ErrorClass::Auth, false),
                Some(429) => (ErrorClass::RateLimit, true),
                Some(400 | 422 | 404) => (ErrorClass::Validation, false),
                Some(408) => (ErrorClass::Timeout, true),
                Some(500..=599) => (ErrorClass::ProviderServer, true),
                _ => (ErrorClass::Transport, true),
            };
            (class, retriable, None, status)
        }
        rig::completion::CompletionError::JsonError(_)
        | rig::completion::CompletionError::UrlError(_) => {
            (ErrorClass::Validation, false, None, None)
        }
        rig::completion::CompletionError::ResponseError(_)
        | rig::completion::CompletionError::ProviderError(_) => {
            (ErrorClass::ProviderServer, true, None, None)
        }
        _ => (ErrorClass::Unknown, false, None, None),
    }
}
