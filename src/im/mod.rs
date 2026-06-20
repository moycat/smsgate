//! IM backend abstraction.
//!
//! Two orthogonal traits:
//! - `MessageSink`   — outbound delivery (SMS -> Telegram)
//! - `MessageSource` — inbound command polling
//!
//! The legacy `Messenger` trait is kept as a convenience super-trait for backends
//! that implement both (e.g. Telegram).

pub mod telegram;

use thiserror::Error;

/// Opaque handle to a previously sent IM message.
pub type MessageId = i64;

/// A message received from the IM backend.
#[derive(Debug, Clone)]
pub struct InboundMessage {
    /// Monotonically increasing cursor; pass as `since` on the next poll.
    pub cursor: i64,
    /// Message text (starts with "/" for commands).
    pub text: String,
    /// If this is a reply to a previously sent message, its ID.
    pub reply_to: Option<MessageId>,
}

/// Errors from the IM layer.
#[derive(Debug, Error)]
pub enum MessengerError {
    #[error("HTTP error: {0}")]
    Http(String),
    #[error("JSON parse error: {0}")]
    Json(String),
    #[error("API error: {0}")]
    Api(String),
    #[error("not connected")]
    Disconnected,
}

/// Outbound delivery target.
pub trait MessageSink {
    /// Deliver a text notification. Returns a backend-specific message ID
    /// (0 if the backend doesn't track IDs).
    fn send_message(&mut self, text: &str) -> Result<MessageId, MessengerError>;
}

/// Inbound command source.
pub trait MessageSource {
    /// Poll for new messages. `since` = cursor from last poll (0 on first call).
    fn poll(&mut self, since: i64, timeout_sec: u32)
        -> Result<Vec<InboundMessage>, MessengerError>;
}

/// Full bidirectional backend (sink + source). Telegram implements this.
/// All business logic in bridge/, commands/, etc. depends only on `MessageSink`.
/// Only the polling thread depends on `MessageSource`.
pub trait Messenger: MessageSink + MessageSource {}

/// Blanket impl: anything that is both a sink and a source is a Messenger.
impl<T: MessageSink + MessageSource> Messenger for T {}
