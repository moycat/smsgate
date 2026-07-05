//! Telegram Bot API JSON types.

use crate::im::MessageId;
use serde::Deserialize;

/// Escape a string for embedding inside a JSON string literal.
///
/// Handles the characters that would produce invalid JSON: backslash,
/// double-quote, and ASCII control characters.
pub fn json_escape(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            ch if ch <= '\u{1f}' => {
                let code = ch as u8;
                out.push_str("\\u00");
                out.push(HEX[(code >> 4) as usize] as char);
                out.push(HEX[(code & 0x0f) as usize] as char);
            }
            ch => out.push(ch),
        }
    }
    out
}

#[derive(Debug, Deserialize)]
pub struct ApiResult<T> {
    pub ok: bool,
    pub result: Option<T>,
    pub description: Option<String>,
    pub parameters: Option<ResponseParameters>,
}

#[derive(Debug, Deserialize)]
pub struct ResponseParameters {
    pub retry_after: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct SendMessageResult {
    pub message_id: MessageId,
}

#[derive(Debug, Deserialize)]
pub struct Update {
    pub update_id: i64,
    pub message: Option<Message>,
    pub callback_query: Option<CallbackQuery>,
}

#[derive(Debug, Deserialize)]
pub struct Message {
    pub message_id: MessageId,
    pub text: Option<String>,
    pub caption: Option<String>,
    pub document: Option<Document>,
    pub reply_to_message: Option<ReplyToMessage>,
    pub chat: Chat,
}

#[derive(Debug, Deserialize)]
pub struct Document {
    pub file_id: String,
    pub file_unique_id: String,
    pub file_name: Option<String>,
    pub mime_type: Option<String>,
    pub file_size: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct ReplyToMessage {
    pub message_id: MessageId,
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    pub id: String,
    pub data: Option<String>,
    pub message: Option<Message>,
}

#[derive(Debug, Deserialize)]
pub struct Chat {
    pub id: i64,
}

#[derive(Debug, Deserialize)]
pub struct TelegramFile {
    pub file_id: String,
    pub file_unique_id: String,
    pub file_size: Option<u64>,
    pub file_path: Option<String>,
}
