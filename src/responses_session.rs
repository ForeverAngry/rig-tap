//! Observer + decorator for `rig-core`'s OpenAI Responses WebSocket
//! session.
//!
//! [`rig::providers::openai::responses_api::websocket::ResponsesWebSocketSession`]
//! is a long-lived stateful socket: one `response.session_started`, N
//! `response.turn_*` pairs, one `response.session_ended` per process
//! lifetime. The schema added in v0.2 mirrors that shape with the
//! [`EventKind::ResponseSessionStarted`] /
//! [`EventKind::ResponseTurnStarted`] /
//! [`EventKind::ResponseTurnCompleted`] /
//! [`EventKind::ResponseSessionEnded`]
//! variants. This module wires them.
//!
//! Two public surfaces:
//!
//! - [`ResponsesSessionObserver`] — a pure state machine. Caller-driven
//!   (no I/O), so it is straightforward to unit-test against fixture
//!   events. Use this when you want full control of the underlying
//!   session and just need the lifecycle events emitted.
//! - [`ObservedResponsesSession`] — a thin decorator that owns a
//!   `ResponsesWebSocketSession` + an observer and forwards
//!   `send` / `next_event` / `close` with the right callbacks. Use this
//!   when you want telemetry "for free" without writing the wiring.
//!
//! # Hosted tools
//!
//! On every `ResponsesWebSocketEvent::Done` the observer runs
//! [`emit_hosted_tools`](crate::emit_hosted_tools) against the raw
//! `done.response: serde_json::Value`. This is the only payload that
//! survives rig-core deserialization (`responses_api::Output` carries
//! `#[serde(other)] Unknown`, which silently discards
//! `web_search_call` / `file_search_call` / `computer_use_call` /
//! `code_interpreter_call` items in every other code path).
//!
//! # Turn finalization
//!
//! The observer defers `response.turn_completed` until either:
//!
//! 1. a `ResponsesWebSocketEvent::Done` arrives — preferred, gives the
//!    raw `output[]` so hosted-tool extraction has data to work with;
//! 2. a `ResponsesWebSocketEvent::Error` arrives — fatal for the turn;
//! 3. the caller invokes [`ResponsesSessionObserver::observe_send`] for
//!    the next turn or [`ResponsesSessionObserver::observe_close`] —
//!    safety net for callers that read only a terminal
//!    `Response(ResponseCompleted)` and never poll for `Done`
//!    (e.g. through
//!    [`ResponsesWebSocketSession::completion`](rig::providers::openai::responses_api::websocket::ResponsesWebSocketSession::completion),
//!    which returns at the terminal `Response`).
//!
//! Deferring this way keeps the `hosted_tool_calls` count accurate when
//! `Done` is consumed, and never *omits* the event when the caller
//! short-circuits before `Done`.
//!
//! # Example
//!
//! ```no_run
//! use rig::client::CompletionClient;
//! use rig::completion::CompletionRequest;
//! use rig::providers::openai;
//! use rig::providers::openai::responses_api::websocket::ResponsesWebSocketEvent;
//! use rig_tap::responses_session::ObservedResponsesSession;
//!
//! # async fn run(request: CompletionRequest) -> Result<(), Box<dyn std::error::Error>> {
//! let client = openai::Client::new("YOUR_API_KEY")?;
//! let session = client.responses_websocket(openai::GPT_5_2).await?;
//! let mut observed = ObservedResponsesSession::new(
//!     session,
//!     "conversation-42",
//!     openai::GPT_5_2,
//!     "session-1",
//! );
//! observed.send(request).await?;
//! loop {
//!     let event = observed.next_event().await?;
//!     if event.is_terminal() {
//!         break;
//!     }
//! }
//! observed.close().await?;
//! # Ok(()) }
//! ```

use std::fmt;

use serde_json::Value;

use rig::providers::openai::responses_api::streaming::ResponseChunkKind;
use rig::providers::openai::responses_api::websocket::ResponsesWebSocketEvent;

use crate::emit::emit_kind;
use crate::event::EventKind;
use crate::responses_extract::extract_hosted_tools;

/// State machine that translates a stream of
/// [`ResponsesWebSocketEvent`]s into the
/// [`EventKind::ResponseSessionStarted`],
/// [`EventKind::ResponseTurnStarted`],
/// [`EventKind::ResponseTurnCompleted`],
/// [`EventKind::ResponseSessionEnded`],
/// [`EventKind::ToolHostedInvoked`],
/// and
/// [`EventKind::ToolHostedCompleted`]
/// events.
///
/// All emissions go through [`emit_kind`]; no I/O is performed beyond
/// the `tracing` dispatch.
pub struct ResponsesSessionObserver {
    conversation_id: String,
    model: String,
    session_id: String,
    session_started: bool,
    session_ended: bool,
    in_turn: bool,
    turn_previous_response_id: Option<String>,
    turn_response_id: Option<String>,
    turn_status: Option<String>,
    turn_tokens_in: Option<u64>,
    turn_tokens_out: Option<u64>,
    turn_hosted_tool_calls: usize,
}

impl ResponsesSessionObserver {
    /// Construct an observer scoped to a single websocket session.
    ///
    /// `conversation_id` is stamped on every emitted event. `model` is
    /// recorded once on `response.session_started`. `session_id` is the
    /// producer-chosen stable identifier that correlates every
    /// `response.turn_*` and the eventual `response.session_ended` —
    /// pick something durable (`Uuid::new_v4()`, the connection URL,
    /// the address of the underlying socket).
    #[must_use]
    pub fn new(
        conversation_id: impl Into<String>,
        model: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            conversation_id: conversation_id.into(),
            model: model.into(),
            session_id: session_id.into(),
            session_started: false,
            session_ended: false,
            in_turn: false,
            turn_previous_response_id: None,
            turn_response_id: None,
            turn_status: None,
            turn_tokens_in: None,
            turn_tokens_out: None,
            turn_hosted_tool_calls: 0,
        }
    }

    /// Returns the session identifier this observer was constructed with.
    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Returns the model identifier this observer was constructed with.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Returns `true` once `response.session_started` has been emitted.
    #[must_use]
    pub fn session_started(&self) -> bool {
        self.session_started
    }

    /// Returns `true` once `response.session_ended` has been emitted.
    #[must_use]
    pub fn session_ended(&self) -> bool {
        self.session_ended
    }

    /// Notify the observer that a new turn is about to be sent on the
    /// underlying session. Emits `response.session_started` on first
    /// call (lazily) and `response.turn_started` for every call.
    ///
    /// If a previous turn is still open (e.g. the caller stopped
    /// reading after the terminal `Response` and never observed
    /// `Done`), it is finalized with `status = "completed"` before the
    /// new turn is announced. This keeps the
    /// `session_started → turn_started → turn_completed → ... → session_ended`
    /// envelope well-formed even when the caller short-circuits.
    pub fn observe_send(&mut self, previous_response_id: Option<&str>) {
        if self.in_turn {
            self.finalize_turn();
        }
        self.start_session_if_needed();
        self.in_turn = true;
        self.turn_previous_response_id = previous_response_id.map(str::to_owned);
        self.turn_response_id = None;
        self.turn_status = None;
        self.turn_tokens_in = None;
        self.turn_tokens_out = None;
        self.turn_hosted_tool_calls = 0;
        emit_kind(
            &self.conversation_id,
            EventKind::ResponseTurnStarted {
                session_id: self.session_id.clone(),
                previous_response_id: self.turn_previous_response_id.clone(),
            },
        );
    }

    /// Notify the observer that
    /// [`ResponsesWebSocketSession::send`](rig::providers::openai::responses_api::websocket::ResponsesWebSocketSession::send)
    /// returned an error. Finalizes the open turn with
    /// `status = "send_error"` so the lifecycle stays well-formed.
    pub fn observe_send_error(&mut self) {
        if !self.in_turn {
            return;
        }
        self.turn_status = Some("send_error".to_owned());
        self.finalize_turn();
    }

    /// Notify the observer of a single server event. Accumulates the
    /// terminal `response_id`, status, and token usage for the in-flight
    /// turn; on `Done` runs hosted-tool extraction against the raw
    /// `response` value and emits the matching `tool.hosted_*` events;
    /// finalizes the turn on `Done` / `Error`.
    pub fn observe_event(&mut self, event: &ResponsesWebSocketEvent) {
        match event {
            ResponsesWebSocketEvent::Response(chunk) => {
                let response = &chunk.response;
                self.turn_response_id = Some(response.id.clone());
                if let Some(status) = response_status_string(&chunk.kind, &response.status) {
                    self.turn_status = Some(status);
                }
                if let Some(usage) = response.usage.as_ref() {
                    self.turn_tokens_in = Some(usage.input_tokens);
                    self.turn_tokens_out = Some(usage.output_tokens);
                }
                // Wait for Done before finalizing so hosted-tool
                // extraction has the raw `output[]` array.
            }
            ResponsesWebSocketEvent::Done(done) => {
                if let Some(response_id) = done.response_id() {
                    self.turn_response_id = Some(response_id.to_owned());
                }
                if let Some(status) = done
                    .response
                    .get("status")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                {
                    self.turn_status = Some(status);
                }
                if let Some(usage) = done.response.get("usage") {
                    if let Some(input) = usage.get("input_tokens").and_then(Value::as_u64) {
                        self.turn_tokens_in = Some(input);
                    }
                    if let Some(output) = usage.get("output_tokens").and_then(Value::as_u64) {
                        self.turn_tokens_out = Some(output);
                    }
                }
                let hosted = extract_hosted_tools(&done.response);
                self.turn_hosted_tool_calls =
                    self.turn_hosted_tool_calls.saturating_add(hosted.len());
                let response_id = self.turn_response_id.clone();
                for call in hosted {
                    emit_hosted_invoked_completed(
                        &self.conversation_id,
                        response_id.as_deref(),
                        call,
                    );
                }
                self.finalize_turn();
            }
            ResponsesWebSocketEvent::Error(_) => {
                if self.in_turn {
                    self.turn_status.get_or_insert_with(|| "error".to_owned());
                    self.finalize_turn();
                }
            }
            ResponsesWebSocketEvent::Item(_) => {}
        }
    }

    /// Notify the observer that the underlying session has been closed
    /// (clean handshake, transport error, or caller-driven shutdown).
    /// Finalizes any in-flight turn, then emits
    /// `response.session_ended` once. `reason` is recorded verbatim on
    /// the event (e.g. `"client_close"`, `"transport_error"`,
    /// `"response_failed"`).
    pub fn observe_close(&mut self, reason: impl Into<String>) {
        if self.in_turn {
            self.finalize_turn();
        }
        if self.session_started && !self.session_ended {
            emit_kind(
                &self.conversation_id,
                EventKind::ResponseSessionEnded {
                    session_id: self.session_id.clone(),
                    reason: reason.into(),
                },
            );
            self.session_ended = true;
        }
    }

    fn start_session_if_needed(&mut self) {
        if self.session_started {
            return;
        }
        emit_kind(
            &self.conversation_id,
            EventKind::ResponseSessionStarted {
                model: self.model.clone(),
                session_id: self.session_id.clone(),
            },
        );
        self.session_started = true;
    }

    fn finalize_turn(&mut self) {
        if !self.in_turn {
            return;
        }
        let response_id = self.turn_response_id.clone().unwrap_or_default();
        let status = self
            .turn_status
            .clone()
            .unwrap_or_else(|| "completed".to_owned());
        emit_kind(
            &self.conversation_id,
            EventKind::ResponseTurnCompleted {
                session_id: self.session_id.clone(),
                response_id,
                previous_response_id: self.turn_previous_response_id.clone(),
                status,
                tokens_in: self.turn_tokens_in,
                tokens_out: self.turn_tokens_out,
                hosted_tool_calls: self.turn_hosted_tool_calls,
            },
        );
        self.in_turn = false;
        self.turn_previous_response_id = None;
        self.turn_response_id = None;
        self.turn_status = None;
        self.turn_tokens_in = None;
        self.turn_tokens_out = None;
        self.turn_hosted_tool_calls = 0;
    }
}

impl fmt::Debug for ResponsesSessionObserver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResponsesSessionObserver")
            .field("session_id", &self.session_id)
            .field("model", &self.model)
            .field("session_started", &self.session_started)
            .field("session_ended", &self.session_ended)
            .field("in_turn", &self.in_turn)
            .finish_non_exhaustive()
    }
}

fn emit_hosted_invoked_completed(
    conversation_id: &str,
    response_id: Option<&str>,
    call: crate::responses_extract::HostedToolCall,
) {
    use crate::event::{PAYLOAD_TRUNCATE_BYTES, truncate_utf8};
    let crate::responses_extract::HostedToolCall {
        tool_name,
        provider_call_id,
        call_id,
        status,
        args_json,
        result_json,
        ..
    } = call;
    let response_id = response_id.map(str::to_owned);
    let (args_payload, args_truncated) = truncate_utf8(&args_json, PAYLOAD_TRUNCATE_BYTES);
    let (result_payload, result_truncated) = truncate_utf8(&result_json, PAYLOAD_TRUNCATE_BYTES);
    emit_kind(
        conversation_id,
        EventKind::ToolHostedInvoked {
            tool_name: tool_name.clone(),
            provider_call_id: provider_call_id.clone(),
            call_id: call_id.clone(),
            response_id: response_id.clone(),
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
            response_id,
            status,
            result: result_payload,
            truncated: result_truncated,
        },
    );
}

fn response_status_string(
    kind: &ResponseChunkKind,
    fallback: &rig::providers::openai::responses_api::ResponseStatus,
) -> Option<String> {
    let from_kind = match kind {
        ResponseChunkKind::ResponseCompleted => Some("completed"),
        ResponseChunkKind::ResponseFailed => Some("failed"),
        ResponseChunkKind::ResponseIncomplete => Some("incomplete"),
        ResponseChunkKind::ResponseCreated | ResponseChunkKind::ResponseInProgress => None,
    };
    if let Some(label) = from_kind {
        return Some(label.to_owned());
    }
    serde_json::to_value(fallback)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
}

// ---------------------------------------------------------------------------
// Decorator
// ---------------------------------------------------------------------------

use rig::completion::{CompletionError, CompletionRequest};
use rig::http_client::HttpClientExt;
use rig::providers::openai::responses_api::websocket::{
    ResponsesWebSocketCreateOptions, ResponsesWebSocketSession,
};
use rig::wasm_compat::{WasmCompatSend, WasmCompatSync};

/// Thin decorator around
/// [`ResponsesWebSocketSession`]
/// that drives a [`ResponsesSessionObserver`] from the same call sites
/// the upstream session uses.
///
/// The decorator forwards every public method of the wrapped session
/// and runs the observer side effects in the right order:
///
/// | Method                 | Observer hook                       |
/// |------------------------|-------------------------------------|
/// | [`Self::send`]         | [`ResponsesSessionObserver::observe_send`]       |
/// | [`Self::send_with_options`] | [`ResponsesSessionObserver::observe_send`]  |
/// | [`Self::next_event`]   | [`ResponsesSessionObserver::observe_event`]      |
/// | [`Self::close`]        | [`ResponsesSessionObserver::observe_close`]      |
///
/// Errors are propagated; the observer is always notified so the
/// lifecycle envelope stays well-formed even on the failure path.
pub struct ObservedResponsesSession<H = rig::http_client::ReqwestClient> {
    inner: ResponsesWebSocketSession<H>,
    observer: ResponsesSessionObserver,
}

impl<H> ObservedResponsesSession<H>
where
    H: HttpClientExt
        + Clone
        + std::fmt::Debug
        + Default
        + WasmCompatSend
        + WasmCompatSync
        + 'static,
{
    /// Wrap a `ResponsesWebSocketSession` with an observer. `model` and
    /// `session_id` are stamped on the emitted `response.*` events; see
    /// [`ResponsesSessionObserver::new`] for the semantic contract.
    pub fn new(
        inner: ResponsesWebSocketSession<H>,
        conversation_id: impl Into<String>,
        model: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Self {
        Self {
            inner,
            observer: ResponsesSessionObserver::new(conversation_id, model, session_id),
        }
    }

    /// Borrow the underlying session.
    #[must_use]
    pub fn inner(&self) -> &ResponsesWebSocketSession<H> {
        &self.inner
    }

    /// Borrow the underlying session mutably. Bypasses the observer — do
    /// not use this for `send` / `next_event` / `close`; prefer the
    /// methods on `ObservedResponsesSession`.
    pub fn inner_mut(&mut self) -> &mut ResponsesWebSocketSession<H> {
        &mut self.inner
    }

    /// Borrow the observer state machine.
    #[must_use]
    pub fn observer(&self) -> &ResponsesSessionObserver {
        &self.observer
    }

    /// Consume the decorator and return the underlying session. Useful
    /// when the caller needs full control of the close handshake.
    /// Emits `response.session_ended` with `reason = "into_inner"`.
    #[must_use]
    pub fn into_inner(mut self) -> ResponsesWebSocketSession<H> {
        self.observer.observe_close("into_inner");
        self.inner
    }

    /// See [`ResponsesWebSocketSession::previous_response_id`].
    #[must_use]
    pub fn previous_response_id(&self) -> Option<&str> {
        self.inner.previous_response_id()
    }

    /// See [`ResponsesWebSocketSession::clear_previous_response_id`].
    pub fn clear_previous_response_id(&mut self) {
        self.inner.clear_previous_response_id();
    }

    /// See [`ResponsesWebSocketSession::send`]. Calls
    /// [`ResponsesSessionObserver::observe_send`] before delegating and
    /// [`ResponsesSessionObserver::observe_send_error`] on failure.
    pub async fn send(&mut self, request: CompletionRequest) -> Result<(), CompletionError> {
        let previous = self.inner.previous_response_id().map(str::to_owned);
        self.observer.observe_send(previous.as_deref());
        match self.inner.send(request).await {
            Ok(()) => Ok(()),
            Err(error) => {
                self.observer.observe_send_error();
                Err(error)
            }
        }
    }

    /// See [`ResponsesWebSocketSession::send_with_options`]. Calls
    /// [`ResponsesSessionObserver::observe_send`] before delegating and
    /// [`ResponsesSessionObserver::observe_send_error`] on failure.
    pub async fn send_with_options(
        &mut self,
        request: CompletionRequest,
        options: ResponsesWebSocketCreateOptions,
    ) -> Result<(), CompletionError> {
        let previous = self.inner.previous_response_id().map(str::to_owned);
        self.observer.observe_send(previous.as_deref());
        match self.inner.send_with_options(request, options).await {
            Ok(()) => Ok(()),
            Err(error) => {
                self.observer.observe_send_error();
                Err(error)
            }
        }
    }

    /// See [`ResponsesWebSocketSession::next_event`]. Calls
    /// [`ResponsesSessionObserver::observe_event`] on success.
    pub async fn next_event(&mut self) -> Result<ResponsesWebSocketEvent, CompletionError> {
        let event = self.inner.next_event().await?;
        self.observer.observe_event(&event);
        Ok(event)
    }

    /// See [`ResponsesWebSocketSession::close`]. Calls
    /// [`ResponsesSessionObserver::observe_close`] after the inner
    /// close completes, recording either `"client_close"` or
    /// `"transport_error"` as the reason.
    pub async fn close(&mut self) -> Result<(), CompletionError> {
        let result = self.inner.close().await;
        let reason = if result.is_ok() {
            "client_close"
        } else {
            "transport_error"
        };
        self.observer.observe_close(reason);
        result
    }
}

impl<H> fmt::Debug for ObservedResponsesSession<H>
where
    H: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ObservedResponsesSession")
            .field("observer", &self.observer)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::ObservabilityEvent;
    use crate::subscriber::CapturingLayer;
    use rig::providers::openai::responses_api::streaming::ResponseChunk;
    use rig::providers::openai::responses_api::websocket::{
        ResponsesWebSocketDoneEvent, ResponsesWebSocketErrorEvent,
    };
    use serde_json::json;
    use tracing_subscriber::prelude::*;

    fn run<F: FnOnce()>(f: F) -> Vec<ObservabilityEvent> {
        let layer = CapturingLayer::new();
        let probe = layer.clone();
        tracing::subscriber::with_default(tracing_subscriber::registry().with(layer), f);
        probe.snapshot()
    }

    fn response_chunk(kind_label: &str, response: Value) -> ResponseChunk {
        serde_json::from_value(json!({
            "type": kind_label,
            "response": response,
            "sequence_number": 0,
        }))
        .unwrap()
    }

    fn done_event(response: Value) -> ResponsesWebSocketDoneEvent {
        serde_json::from_value(json!({
            "type": "response.done",
            "response": response,
        }))
        .unwrap()
    }

    fn error_event() -> ResponsesWebSocketErrorEvent {
        serde_json::from_value(json!({
            "type": "error",
            "error": { "type": "server_error", "message": "boom" },
        }))
        .unwrap()
    }

    fn minimal_response_body(id: &str, status: &str) -> Value {
        json!({
            "id": id,
            "object": "response",
            "created_at": 0,
            "status": status,
            "model": "gpt-5",
            "output": [],
        })
    }

    #[test]
    fn happy_path_session_turn_done_close() {
        let events = run(|| {
            let mut observer = ResponsesSessionObserver::new("conv-1", "gpt-5", "sess-1");
            observer.observe_send(None);
            observer.observe_event(&ResponsesWebSocketEvent::Response(Box::new(
                response_chunk(
                    "response.completed",
                    json!({
                        "id": "resp_1",
                        "object": "response",
                        "created_at": 0,
                        "status": "completed",
                        "model": "gpt-5",
                        "usage": { "input_tokens": 12, "output_tokens": 34, "total_tokens": 46,
                                   "output_tokens_details": { "reasoning_tokens": 0 } },
                        "output": [],
                    }),
                ),
            )));
            observer.observe_event(&ResponsesWebSocketEvent::Done(done_event(json!({
                "id": "resp_1",
                "status": "completed",
                "usage": { "input_tokens": 12, "output_tokens": 34 },
                "output": [
                    { "type": "web_search_call", "id": "ws_1", "status": "completed",
                      "action": { "queries": ["x"] }, "results": [{"u": "v"}] }
                ],
            }))));
            observer.observe_close("client_close");
        });
        let kinds: Vec<&str> = events.iter().map(|e| e.kind.discriminant()).collect();
        assert_eq!(
            kinds,
            vec![
                "response.session_started",
                "response.turn_started",
                "tool.hosted_invoked",
                "tool.hosted_completed",
                "response.turn_completed",
                "response.session_ended",
            ]
        );
        let turn_completed = events
            .iter()
            .find(|e| e.kind.discriminant() == "response.turn_completed")
            .unwrap();
        match &turn_completed.kind {
            EventKind::ResponseTurnCompleted {
                response_id,
                status,
                tokens_in,
                tokens_out,
                hosted_tool_calls,
                session_id,
                ..
            } => {
                assert_eq!(response_id, "resp_1");
                assert_eq!(status, "completed");
                assert_eq!(*tokens_in, Some(12));
                assert_eq!(*tokens_out, Some(34));
                assert_eq!(*hosted_tool_calls, 1);
                assert_eq!(session_id, "sess-1");
            }
            other => panic!("unexpected variant: {other:?}"),
        }
    }

    #[test]
    fn turn_finalized_lazily_when_caller_skips_done() {
        let events = run(|| {
            let mut observer = ResponsesSessionObserver::new("conv-2", "gpt-5", "sess-2");
            observer.observe_send(None);
            observer.observe_event(&ResponsesWebSocketEvent::Response(Box::new(
                response_chunk(
                    "response.completed",
                    minimal_response_body("resp_a", "completed"),
                ),
            )));
            // Caller skips Done and immediately starts the next turn.
            observer.observe_send(Some("resp_a"));
            observer.observe_event(&ResponsesWebSocketEvent::Done(done_event(json!({
                "id": "resp_b",
                "status": "completed",
                "output": [],
            }))));
            observer.observe_close("client_close");
        });
        let kinds: Vec<&str> = events.iter().map(|e| e.kind.discriminant()).collect();
        assert_eq!(
            kinds,
            vec![
                "response.session_started",
                "response.turn_started",
                "response.turn_completed",
                "response.turn_started",
                "response.turn_completed",
                "response.session_ended",
            ]
        );
        match &events[3].kind {
            EventKind::ResponseTurnStarted {
                previous_response_id,
                ..
            } => assert_eq!(previous_response_id.as_deref(), Some("resp_a")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn error_event_finalizes_turn_with_error_status() {
        let events = run(|| {
            let mut observer = ResponsesSessionObserver::new("conv-3", "gpt-5", "sess-3");
            observer.observe_send(None);
            observer.observe_event(&ResponsesWebSocketEvent::Error(error_event()));
            observer.observe_close("response_failed");
        });
        let turn_completed = events
            .iter()
            .find(|e| e.kind.discriminant() == "response.turn_completed")
            .unwrap();
        match &turn_completed.kind {
            EventKind::ResponseTurnCompleted { status, .. } => {
                assert_eq!(status, "error");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn send_error_finalizes_turn() {
        let events = run(|| {
            let mut observer = ResponsesSessionObserver::new("conv-4", "gpt-5", "sess-4");
            observer.observe_send(None);
            observer.observe_send_error();
            observer.observe_close("client_close");
        });
        let kinds: Vec<&str> = events.iter().map(|e| e.kind.discriminant()).collect();
        assert_eq!(
            kinds,
            vec![
                "response.session_started",
                "response.turn_started",
                "response.turn_completed",
                "response.session_ended",
            ]
        );
        match &events[2].kind {
            EventKind::ResponseTurnCompleted { status, .. } => assert_eq!(status, "send_error"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn session_ended_emitted_once() {
        let events = run(|| {
            let mut observer = ResponsesSessionObserver::new("conv-5", "gpt-5", "sess-5");
            observer.observe_send(None);
            observer.observe_event(&ResponsesWebSocketEvent::Done(done_event(json!({
                "id": "resp_1",
                "status": "completed",
                "output": [],
            }))));
            observer.observe_close("client_close");
            observer.observe_close("client_close");
        });
        let ended_count = events
            .iter()
            .filter(|e| e.kind.discriminant() == "response.session_ended")
            .count();
        assert_eq!(ended_count, 1);
    }

    #[test]
    fn close_without_any_turn_emits_nothing() {
        let events = run(|| {
            let mut observer = ResponsesSessionObserver::new("conv-6", "gpt-5", "sess-6");
            observer.observe_close("never_opened");
        });
        assert!(events.is_empty());
    }
}
