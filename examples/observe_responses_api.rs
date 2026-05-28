//! Observe an OpenAI Responses WebSocket session end-to-end.
//!
//! Requires the `openai-responses-websocket` and `subscriber` features:
//!
//! ```bash
//! cargo run --example observe_responses_api \
//!     --features openai-responses-websocket,subscriber
//! ```
//!
//! Environment variables (all optional except `OPENAI_API_KEY` to make
//! a live call):
//!
//! - `OPENAI_API_KEY`     — required to connect. When unset, the
//!   example exits cleanly so it stays runnable in CI without a live
//!   socket.
//! - `RIG_TAP_PROMPT`     — user prompt to send. Defaults to a short
//!   probe.
//! - `RIG_TAP_MODEL`      — model id. Defaults to `gpt-5`.
//! - `RIG_TAP_SESSION_ID` — stable session identifier stamped on every
//!   `response.session_*` and `response.turn_*` envelope. Defaults to
//!   a per-process string.
//!
//! No secrets are read from any other source.

#[cfg(all(
    feature = "openai-responses-websocket",
    feature = "subscriber",
    not(target_family = "wasm")
))]
mod observe {
    use std::env;

    use anyhow::Result;
    use rig::client::CompletionClient;
    use rig::completion::{CompletionModel, Message};
    use rig::providers::openai;
    use rig::providers::openai::responses_api::websocket::ResponsesWebSocketEvent;
    use rig_tap::{CapturingLayer, ObservedResponsesSession};
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    pub async fn run() -> Result<()> {
        let capture = CapturingLayer::new();
        let _guard = tracing_subscriber::registry()
            .with(capture.clone())
            .set_default();

        let prompt =
            env::var("RIG_TAP_PROMPT").unwrap_or_else(|_| "Say hello in one sentence.".to_owned());
        let model_id = env::var("RIG_TAP_MODEL").unwrap_or_else(|_| openai::GPT_5.to_owned());
        let session_id = env::var("RIG_TAP_SESSION_ID")
            .unwrap_or_else(|_| format!("rig-tap-example-{}", std::process::id()));
        let conversation_id = "rig-tap-responses-example";

        let Ok(api_key) = env::var("OPENAI_API_KEY") else {
            eprintln!(
                "OPENAI_API_KEY not set; skipping live socket. Re-run with the key \
                 exported to drive a real session."
            );
            return Ok(());
        };

        let client = openai::Client::new(&api_key)?;
        let model = client.completion_model(&model_id);
        let session = client.responses_websocket(&model_id).await?;
        let mut observed =
            ObservedResponsesSession::new(session, conversation_id, model_id.clone(), session_id);

        let request = model.completion_request(Message::user(prompt)).build();

        observed.send(request).await?;
        loop {
            let event = observed.next_event().await?;
            if matches!(event, ResponsesWebSocketEvent::Done(_)) {
                break;
            }
        }
        observed.close().await?;

        let events = capture.snapshot();
        eprintln!("captured {} rig_tap envelopes:", events.len());
        for evt in events {
            eprintln!("  {} (tick={})", evt.kind.discriminant(), evt.tick);
        }

        Ok(())
    }
}

#[cfg(all(
    feature = "openai-responses-websocket",
    feature = "subscriber",
    not(target_family = "wasm")
))]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    observe::run().await
}

#[cfg(not(all(
    feature = "openai-responses-websocket",
    feature = "subscriber",
    not(target_family = "wasm")
)))]
fn main() {
    eprintln!(
        "This example requires --features openai-responses-websocket,subscriber on a \
         non-wasm target."
    );
}
