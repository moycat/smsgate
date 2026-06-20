//! Telegram Bot API JSON types.

use crate::im::MessageId;
use serde::Deserialize;

/// Escape a string for embedding inside a JSON string literal.
///
/// Handles the characters that would produce invalid JSON: backslash,
/// double-quote, and ASCII control characters (LF, CR, TAB).
pub fn json_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

#[derive(Debug, Deserialize)]
pub struct ApiResult<T> {
    pub ok: bool,
    pub result: Option<T>,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SendMessageResult {
    pub message_id: MessageId,
}

#[derive(Debug, Deserialize)]
pub struct Update {
    pub update_id: i64,
    pub message: Option<Message>,
}

#[derive(Debug, Deserialize)]
pub struct Message {
    pub message_id: MessageId,
    pub text: Option<String>,
    pub reply_to_message: Option<ReplyToMessage>,
    pub chat: Chat,
}

#[derive(Debug, Deserialize)]
pub struct ReplyToMessage {
    pub message_id: MessageId,
}

#[derive(Debug, Deserialize)]
pub struct Chat {
    pub id: i64,
}
