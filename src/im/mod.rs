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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageFormat {
    Plain,
    Html,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineKeyboardButton {
    pub text: String,
    pub callback_data: String,
}

impl InlineKeyboardButton {
    pub fn new(text: impl Into<String>, callback_data: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            callback_data: callback_data.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineKeyboard {
    pub rows: Vec<Vec<InlineKeyboardButton>>,
}

impl InlineKeyboard {
    pub fn single_row(buttons: Vec<InlineKeyboardButton>) -> Self {
        Self {
            rows: if buttons.is_empty() {
                Vec::new()
            } else {
                vec![buttons]
            },
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rows.iter().all(Vec::is_empty)
    }
}

/// A callback query received from an inline keyboard button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundCallback {
    pub id: String,
    pub data: String,
    pub message_id: MessageId,
}

/// A message received from the IM backend.
#[derive(Debug, Clone)]
pub struct InboundMessage {
    /// Monotonically increasing cursor; pass as `since` on the next poll.
    pub cursor: i64,
    /// Message text (starts with "/" for commands).
    pub text: String,
    /// If this is a reply to a previously sent message, its ID.
    pub reply_to: Option<MessageId>,
    /// Attached document metadata, if the IM backend delivered a file.
    pub document: Option<InboundDocument>,
    /// Inline keyboard callback metadata, if this update came from a button.
    pub callback: Option<InboundCallback>,
}

/// A document received from the IM backend.
#[derive(Debug, Clone)]
pub struct InboundDocument {
    /// Backend-specific file identifier used to request a temporary download URL.
    pub file_id: String,
    /// Stable backend-specific file identifier; not usable for download.
    pub file_unique_id: String,
    /// Original file name, when supplied by the sender.
    pub file_name: Option<String>,
    /// MIME type, when supplied by the sender.
    pub mime_type: Option<String>,
    /// File size in bytes, when supplied by the backend.
    pub file_size: Option<u64>,
    /// Caption sent with the document, when supplied.
    pub caption: Option<String>,
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

    /// Deliver a text notification with an explicit backend text format.
    fn send_message_with_format(
        &mut self,
        text: &str,
        format: MessageFormat,
    ) -> Result<MessageId, MessengerError> {
        let _ = format;
        self.send_message(text)
    }

    /// Deliver a text notification with inline keyboard markup.
    fn send_message_with_keyboard(
        &mut self,
        text: &str,
        keyboard: &InlineKeyboard,
    ) -> Result<MessageId, MessengerError> {
        let _ = keyboard;
        self.send_message(text)
    }

    /// Deliver a formatted text notification with inline keyboard markup.
    fn send_message_with_keyboard_and_format(
        &mut self,
        text: &str,
        keyboard: &InlineKeyboard,
        format: MessageFormat,
    ) -> Result<MessageId, MessengerError> {
        let _ = format;
        self.send_message_with_keyboard(text, keyboard)
    }

    /// Edit a previously delivered text notification when the backend supports it.
    fn edit_message(&mut self, message_id: MessageId, text: &str) -> Result<(), MessengerError> {
        let _ = message_id;
        self.send_message(text).map(|_| ())
    }

    /// Edit a message with an explicit backend text format.
    fn edit_message_with_format(
        &mut self,
        message_id: MessageId,
        text: &str,
        format: MessageFormat,
    ) -> Result<(), MessengerError> {
        let _ = format;
        self.edit_message(message_id, text)
    }

    /// Edit a message and replace its inline keyboard markup.
    fn edit_message_with_keyboard(
        &mut self,
        message_id: MessageId,
        text: &str,
        keyboard: &InlineKeyboard,
    ) -> Result<(), MessengerError> {
        let _ = keyboard;
        self.edit_message(message_id, text)
    }

    /// Edit a formatted message and replace its inline keyboard markup.
    fn edit_message_with_keyboard_and_format(
        &mut self,
        message_id: MessageId,
        text: &str,
        keyboard: &InlineKeyboard,
        format: MessageFormat,
    ) -> Result<(), MessengerError> {
        let _ = format;
        self.edit_message_with_keyboard(message_id, text, keyboard)
    }

    /// Acknowledge an inline keyboard callback query.
    fn answer_callback_query(
        &mut self,
        callback_query_id: &str,
        text: Option<&str>,
    ) -> Result<(), MessengerError> {
        let _ = (callback_query_id, text);
        Ok(())
    }
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
