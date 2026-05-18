//! [`ObservedMemory`]: a [`rig::memory::ConversationMemory`] decorator that
//! emits a [`EventKind::ContextSampled`](crate::EventKind::ContextSampled)
//! event on every [`load`](rig::memory::ConversationMemory::load).
//!
//! Use it to feed the active-context-size curve of a funnel-style observability
//! UI: every time the agent loads history before a turn, the decorator measures
//! the size of the loaded conversation and emits a sample.

use rig::completion::Message;
use rig::memory::{ConversationMemory, MemoryError};
use rig::wasm_compat::WasmBoxedFuture;

use crate::emit::emit_kind;
use crate::event::EventKind;

/// Wraps any [`ConversationMemory`] and emits a `context.sampled` event on
/// every `load`. `append` and `clear` pass through unchanged.
///
/// # Example
///
/// ```no_run
/// use rig::memory::InMemoryConversationMemory;
/// use rig_tap::ObservedMemory;
///
/// let inner = InMemoryConversationMemory::new();
/// let observed = ObservedMemory::new(inner);
/// // pass `observed` to `agent.memory(observed)`
/// ```
pub struct ObservedMemory<M> {
    inner: M,
}

impl<M> ObservedMemory<M> {
    /// Wrap `inner` so its loads emit `context.sampled` events.
    pub fn new(inner: M) -> Self {
        Self { inner }
    }

    /// Return a reference to the wrapped memory.
    pub fn inner(&self) -> &M {
        &self.inner
    }

    /// Consume the decorator and return the wrapped memory.
    pub fn into_inner(self) -> M {
        self.inner
    }
}

impl<M> std::fmt::Debug for ObservedMemory<M>
where
    M: std::fmt::Debug,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObservedMemory")
            .field("inner", &self.inner)
            .finish()
    }
}

impl<M> Clone for ObservedMemory<M>
where
    M: Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<M> ConversationMemory for ObservedMemory<M>
where
    M: ConversationMemory,
{
    fn load<'a>(
        &'a self,
        conversation_id: &'a str,
    ) -> WasmBoxedFuture<'a, Result<Vec<Message>, MemoryError>> {
        Box::pin(async move {
            let messages = self.inner.load(conversation_id).await?;
            let message_count = messages.len();
            let byte_size = approx_json_size(&messages);
            emit_kind(
                conversation_id,
                EventKind::ContextSampled {
                    message_count,
                    byte_size,
                    token_estimate: None,
                },
            );
            Ok(messages)
        })
    }

    fn append<'a>(
        &'a self,
        conversation_id: &'a str,
        messages: Vec<Message>,
    ) -> WasmBoxedFuture<'a, Result<(), MemoryError>> {
        self.inner.append(conversation_id, messages)
    }

    fn clear<'a>(
        &'a self,
        conversation_id: &'a str,
    ) -> WasmBoxedFuture<'a, Result<(), MemoryError>> {
        self.inner.clear(conversation_id)
    }
}

/// Counts bytes that a streaming JSON serializer would emit, without ever
/// materializing the JSON. Implements [`std::io::Write`] so we can pass it
/// to [`serde_json::to_writer`].
#[derive(Default)]
struct CountingWriter {
    bytes: usize,
}

impl std::io::Write for CountingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buf.len());
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Approximate JSON byte size of `messages`. Cheap: serializes through a
/// counter-only writer, no allocations of the encoded payload. Returns `0`
/// on the (unreachable in practice) case where `Message` serialization
/// fails.
fn approx_json_size(messages: &[Message]) -> usize {
    let mut writer = CountingWriter::default();
    if serde_json::to_writer(&mut writer, messages).is_err() {
        return 0;
    }
    writer.bytes
}
