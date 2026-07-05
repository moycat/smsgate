//! Telegram Bot API backend.

#[cfg(feature = "esp32")]
pub mod http;
pub mod types;
#[cfg(feature = "esp32")]
pub mod worker;

use super::{
    InboundCallback, InboundDocument, InboundMessage, InlineKeyboard, MessageFormat, MessageId,
    MessengerError,
};
#[cfg(feature = "esp32")]
use super::{MessageSink, MessageSource};
#[cfg(feature = "esp32")]
use crate::modem::ModemPort;
#[cfg(feature = "esp32")]
use http::TelegramHttpClient;
#[cfg(feature = "esp32")]
use std::sync::{Arc, Mutex};
use std::time::Duration;
use types::Update;
#[cfg(feature = "esp32")]
use types::{ApiResult, SendMessageResult};

const GET_UPDATES_LIMIT: u8 = 10;
const MIN_POLL_TIMEOUT_SEC: u32 = 1;
const MAX_POLL_TIMEOUT_SEC: u32 = 30;
const POLL_ERROR_LOG_EVERY: u16 = 12;
const POLL_ERROR_RECOVERY_EVERY: u16 = POLL_ERROR_LOG_EVERY;
const TELEGRAM_SEND_RETRY_INTERVAL_SECS: u64 = 30;
const TELEGRAM_RESTART_AFTER_SECS: u64 = 5 * 60;

/// Build a bounded `getUpdates` request body for the embedded runtime.
pub fn build_get_updates_body(since: i64, timeout_sec: u32) -> String {
    let timeout = timeout_sec.clamp(MIN_POLL_TIMEOUT_SEC, MAX_POLL_TIMEOUT_SEC);
    format!(
        r#"{{"offset":{},"timeout":{},"limit":{},"allowed_updates":["message","callback_query"]}}"#,
        since, timeout, GET_UPDATES_LIMIT
    )
}

pub fn should_log_poll_error(consecutive_errors: u16) -> bool {
    consecutive_errors == 1 || consecutive_errors.is_multiple_of(POLL_ERROR_LOG_EVERY)
}

pub fn should_recover_after_poll_errors(consecutive_errors: u16) -> bool {
    consecutive_errors >= POLL_ERROR_RECOVERY_EVERY
        && consecutive_errors.is_multiple_of(POLL_ERROR_RECOVERY_EVERY)
}

pub fn poll_error_log_detail(consecutive_errors: u16, error: &str) -> String {
    format!("poll error x{}: {}", consecutive_errors, error)
}

pub fn poll_retry_after(error: &MessengerError) -> Option<Duration> {
    let retry_after_secs = match error {
        MessengerError::RateLimited {
            retry_after_secs, ..
        } => Some(*retry_after_secs),
        MessengerError::Api(description) => parse_retry_after_description(description),
        _ => None,
    }?;
    Some(Duration::from_secs(u64::from(retry_after_secs.max(1))))
}

pub fn telegram_send_retry_interval() -> Duration {
    Duration::from_secs(TELEGRAM_SEND_RETRY_INTERVAL_SECS)
}

pub fn telegram_restart_after() -> Duration {
    Duration::from_secs(TELEGRAM_RESTART_AFTER_SECS)
}

pub fn send_retry_delay_after(attempt_elapsed: Duration) -> Duration {
    telegram_send_retry_interval().saturating_sub(attempt_elapsed)
}

pub fn should_restart_after_stale_poll(elapsed: Duration) -> bool {
    elapsed >= telegram_restart_after()
}

pub fn should_restart_after_send_retry(elapsed: Duration) -> bool {
    elapsed >= telegram_restart_after()
}

pub fn build_send_message_body(
    chat_id: i64,
    text: &str,
    keyboard: Option<&InlineKeyboard>,
) -> String {
    build_send_message_body_with_format(chat_id, text, keyboard, MessageFormat::Plain)
}

pub fn build_send_message_body_with_format(
    chat_id: i64,
    text: &str,
    keyboard: Option<&InlineKeyboard>,
    format: MessageFormat,
) -> String {
    let mut body = format!(r#"{{"chat_id":{}"#, chat_id);
    append_message_content(&mut body, text, format);
    append_reply_markup(&mut body, keyboard);
    body.push('}');
    body
}

pub fn build_edit_message_text_body(
    chat_id: i64,
    message_id: MessageId,
    text: &str,
    keyboard: Option<&InlineKeyboard>,
) -> String {
    build_edit_message_text_body_with_format(
        chat_id,
        message_id,
        text,
        keyboard,
        MessageFormat::Plain,
    )
}

pub fn build_edit_message_text_body_with_format(
    chat_id: i64,
    message_id: MessageId,
    text: &str,
    keyboard: Option<&InlineKeyboard>,
    format: MessageFormat,
) -> String {
    let mut body = format!(r#"{{"chat_id":{},"message_id":{}"#, chat_id, message_id);
    append_message_content(&mut body, text, format);
    append_reply_markup(&mut body, keyboard);
    body.push('}');
    body
}

pub fn build_answer_callback_query_body(callback_query_id: &str, text: Option<&str>) -> String {
    let mut body = String::from(r#"{"callback_query_id":"#);
    push_json_string(&mut body, callback_query_id);
    if let Some(text) = text {
        push_json_string_field(&mut body, "text", text);
    }
    body.push('}');
    body
}

fn append_reply_markup(body: &mut String, keyboard: Option<&InlineKeyboard>) {
    let Some(keyboard) = keyboard else {
        return;
    };
    if keyboard.is_empty() {
        return;
    }
    body.push_str(r#","reply_markup":"#);
    body.push_str(&inline_keyboard_json(keyboard));
}

fn append_message_content(body: &mut String, text: &str, format: MessageFormat) {
    push_json_string_field(body, "text", text);
    if format == MessageFormat::Html {
        body.push_str(r#","parse_mode":"HTML""#);
    }
}

fn inline_keyboard_json(keyboard: &InlineKeyboard) -> String {
    let mut out = String::from(r#"{"inline_keyboard":["#);
    let mut first_row = true;
    for row in keyboard.rows.iter().filter(|row| !row.is_empty()) {
        if !first_row {
            out.push(',');
        }
        first_row = false;
        out.push('[');
        let mut first_button = true;
        for button in row {
            if !first_button {
                out.push(',');
            }
            first_button = false;
            out.push_str(r#"{"text":"#);
            push_json_string(&mut out, &button.text);
            out.push_str(r#","callback_data":"#);
            push_json_string(&mut out, &button.callback_data);
            out.push('}');
        }
        out.push(']');
    }
    out.push_str("]}");
    out
}

/// Build a `getFile` request body.
pub fn build_get_file_body(file_id: &str) -> String {
    let mut body = String::from(r#"{"file_id":"#);
    push_json_string(&mut body, file_id);
    body.push('}');
    body
}

fn push_json_string_field(body: &mut String, name: &str, value: &str) {
    body.push_str(r#",""#);
    body.push_str(name);
    body.push_str(r#"":"#);
    push_json_string(body, value);
}

fn push_json_string(body: &mut String, value: &str) {
    body.push('"');
    body.push_str(&types::json_escape(value));
    body.push('"');
}

fn parse_retry_after_description(description: &str) -> Option<u32> {
    let retry_after = description.split_once("retry after ")?.1;
    let digits_len = retry_after
        .as_bytes()
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    if digits_len == 0 {
        return None;
    }
    retry_after[..digits_len].parse().ok()
}

/// Convert a Telegram update into the backend-neutral inbound representation.
pub fn update_to_inbound_message(update: Update, chat_id: i64) -> Option<InboundMessage> {
    if let Some(callback) = update.callback_query {
        let msg = callback.message?;
        if msg.chat.id != chat_id {
            log::warn!(
                "[tg] callback from unexpected chat {} - ignored",
                msg.chat.id
            );
            return None;
        }
        let data = callback.data.unwrap_or_default();
        if data.is_empty() {
            return None;
        }
        return Some(InboundMessage {
            cursor: update.update_id + 1,
            text: data.clone(),
            reply_to: None,
            document: None,
            callback: Some(InboundCallback {
                id: callback.id,
                data,
                message_id: msg.message_id,
            }),
        });
    }

    let msg = update.message?;
    // Reject messages from any chat other than the configured one.
    // Without this, anyone who adds the bot to a group can trigger commands.
    if msg.chat.id != chat_id {
        log::warn!(
            "[tg] message from unexpected chat {} - ignored",
            msg.chat.id
        );
        return None;
    }

    let text = msg
        .text
        .unwrap_or_else(|| msg.caption.clone().unwrap_or_default());
    let document = msg.document.map(|doc| {
        log::info!(
            "[tg] document update: message_id={} file_name={} mime={} size={:?} caption_len={}",
            msg.message_id,
            doc.file_name.as_deref().unwrap_or("<none>"),
            doc.mime_type.as_deref().unwrap_or("<none>"),
            doc.file_size,
            msg.caption.as_deref().map(str::len).unwrap_or(0)
        );
        InboundDocument {
            file_id: doc.file_id,
            file_unique_id: doc.file_unique_id,
            file_name: doc.file_name,
            mime_type: doc.mime_type,
            file_size: doc.file_size,
            caption: msg.caption,
        }
    });

    if text.is_empty() && document.is_none() {
        return None;
    }

    Some(InboundMessage {
        cursor: update.update_id + 1,
        text,
        reply_to: msg.reply_to_message.map(|r| r.message_id),
        document,
        callback: None,
    })
}

#[cfg(feature = "esp32")]
enum Transport {
    Wifi(TelegramHttpClient),
    Modem(Arc<Mutex<dyn ModemPort + Send>>),
}

/// Telegram Bot API messenger.
#[cfg(feature = "esp32")]
pub struct TelegramMessenger {
    transport: Transport,
    chat_id: i64,
    token: String,
}

#[cfg(feature = "esp32")]
impl TelegramMessenger {
    pub fn new_wifi(http: TelegramHttpClient, token: String, chat_id: i64) -> Self {
        TelegramMessenger {
            transport: Transport::Wifi(http),
            chat_id,
            token,
        }
    }

    /// IM over the modem's built-in HTTP stack (cellular PDP).
    /// Use a short `timeout_sec` in `poll` — the UART is shared with SMS.
    pub fn new_modem(modem: Arc<Mutex<dyn ModemPort + Send>>, token: String, chat_id: i64) -> Self {
        TelegramMessenger {
            transport: Transport::Modem(modem),
            chat_id,
            token,
        }
    }

    fn post_json(&mut self, method: &str, body: &str) -> Result<String, MessengerError> {
        let path = format!("/bot{}/{}", self.token, method);
        log::debug!(
            "[tg] API request: method={} transport={} body_len={}",
            method,
            match &self.transport {
                Transport::Wifi(_) => "wifi",
                Transport::Modem(_) => "modem",
            },
            body.len()
        );
        match &mut self.transport {
            Transport::Wifi(http) => http
                .post(&path, body)
                .map_err(|e| MessengerError::Http(format!("{:#}", e))),
            Transport::Modem(m) => {
                let mut g = m.lock().map_err(|_| MessengerError::Disconnected)?;
                let raw = g
                    .post_telegram_https(&path, body)
                    .map_err(|e| MessengerError::Http(format!("modem {}", e)))?;
                Ok(raw)
            }
        }
    }

    fn post_and_parse<T: serde::de::DeserializeOwned>(
        &mut self,
        method: &str,
        body: &str,
    ) -> Result<ApiResult<T>, MessengerError> {
        let resp = self.post_json(method, body)?;
        serde_json::from_str(&resp).map_err(|e| MessengerError::Json(e.to_string()))
    }

    fn check_ok<T>(result: ApiResult<T>) -> Result<Option<T>, MessengerError> {
        if result.ok {
            Ok(result.result)
        } else {
            let description = result.description.unwrap_or_default();
            let retry_after_secs = result
                .parameters
                .as_ref()
                .and_then(|parameters| parameters.retry_after)
                .or_else(|| parse_retry_after_description(&description));
            if let Some(retry_after_secs) = retry_after_secs {
                Err(MessengerError::RateLimited {
                    retry_after_secs,
                    description,
                })
            } else {
                Err(MessengerError::Api(description))
            }
        }
    }

    /// Register bot commands with Telegram (called once at startup).
    pub fn register_commands(&mut self, commands: &[(&str, &str)]) -> Result<(), MessengerError> {
        let cmds_json: Vec<String> = commands
            .iter()
            .map(|(name, desc)| format!(r#"{{"command":"{}","description":"{}"}}"#, name, desc))
            .collect();
        let body = format!(r#"{{"commands":[{}]}}"#, cmds_json.join(","));
        let result: ApiResult<bool> = self.post_and_parse("setMyCommands", &body)?;
        Self::check_ok(result)?;
        Ok(())
    }
}

#[cfg(feature = "esp32")]
impl MessageSink for TelegramMessenger {
    fn send_message(&mut self, text: &str) -> Result<MessageId, MessengerError> {
        self.send_message_with_format(text, MessageFormat::Plain)
    }

    fn send_message_with_format(
        &mut self,
        text: &str,
        format: MessageFormat,
    ) -> Result<MessageId, MessengerError> {
        let body = build_send_message_body_with_format(self.chat_id, text, None, format);
        let result: ApiResult<SendMessageResult> = self.post_and_parse("sendMessage", &body)?;
        let r = Self::check_ok(result)?;
        Ok(r.map(|r| r.message_id).unwrap_or(0))
    }

    fn send_message_with_keyboard(
        &mut self,
        text: &str,
        keyboard: &InlineKeyboard,
    ) -> Result<MessageId, MessengerError> {
        self.send_message_with_keyboard_and_format(text, keyboard, MessageFormat::Plain)
    }

    fn send_message_with_keyboard_and_format(
        &mut self,
        text: &str,
        keyboard: &InlineKeyboard,
        format: MessageFormat,
    ) -> Result<MessageId, MessengerError> {
        let body = build_send_message_body_with_format(self.chat_id, text, Some(keyboard), format);
        let result: ApiResult<SendMessageResult> = self.post_and_parse("sendMessage", &body)?;
        let r = Self::check_ok(result)?;
        Ok(r.map(|r| r.message_id).unwrap_or(0))
    }

    fn edit_message(&mut self, message_id: MessageId, text: &str) -> Result<(), MessengerError> {
        self.edit_message_with_format(message_id, text, MessageFormat::Plain)
    }

    fn edit_message_with_format(
        &mut self,
        message_id: MessageId,
        text: &str,
        format: MessageFormat,
    ) -> Result<(), MessengerError> {
        let body =
            build_edit_message_text_body_with_format(self.chat_id, message_id, text, None, format);
        let result: ApiResult<serde_json::Value> = self.post_and_parse("editMessageText", &body)?;
        Self::check_ok(result)?;
        Ok(())
    }

    fn edit_message_with_keyboard(
        &mut self,
        message_id: MessageId,
        text: &str,
        keyboard: &InlineKeyboard,
    ) -> Result<(), MessengerError> {
        self.edit_message_with_keyboard_and_format(message_id, text, keyboard, MessageFormat::Plain)
    }

    fn edit_message_with_keyboard_and_format(
        &mut self,
        message_id: MessageId,
        text: &str,
        keyboard: &InlineKeyboard,
        format: MessageFormat,
    ) -> Result<(), MessengerError> {
        let body = build_edit_message_text_body_with_format(
            self.chat_id,
            message_id,
            text,
            Some(keyboard),
            format,
        );
        let result: ApiResult<serde_json::Value> = self.post_and_parse("editMessageText", &body)?;
        Self::check_ok(result)?;
        Ok(())
    }

    fn answer_callback_query(
        &mut self,
        callback_query_id: &str,
        text: Option<&str>,
    ) -> Result<(), MessengerError> {
        let body = build_answer_callback_query_body(callback_query_id, text);
        let result: ApiResult<serde_json::Value> =
            self.post_and_parse("answerCallbackQuery", &body)?;
        Self::check_ok(result)?;
        Ok(())
    }
}

#[cfg(feature = "esp32")]
impl MessageSource for TelegramMessenger {
    fn poll(
        &mut self,
        since: i64,
        timeout_sec: u32,
    ) -> Result<Vec<InboundMessage>, MessengerError> {
        let body = build_get_updates_body(since, timeout_sec);
        log::debug!(
            "[tg] polling updates: since={} timeout={}s",
            since,
            timeout_sec
        );
        let result: ApiResult<Vec<Update>> = self.post_and_parse("getUpdates", &body)?;
        let updates = Self::check_ok(result)?.unwrap_or_default();
        let update_count = updates.len();
        let messages: Vec<InboundMessage> = updates
            .into_iter()
            .filter_map(|u| update_to_inbound_message(u, self.chat_id))
            .collect();
        log::debug!(
            "[tg] polling result: updates={} accepted_messages={}",
            update_count,
            messages.len()
        );
        Ok(messages)
    }
}
