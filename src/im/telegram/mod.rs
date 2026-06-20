//! Telegram Bot API backend.

#[cfg(feature = "esp32")]
pub mod http;
pub mod types;
#[cfg(feature = "esp32")]
pub mod worker;

#[cfg(feature = "esp32")]
use super::{InboundMessage, MessageId, MessageSink, MessageSource, MessengerError};
#[cfg(feature = "esp32")]
use crate::modem::ModemPort;
#[cfg(feature = "esp32")]
use http::TelegramHttpClient;
#[cfg(feature = "esp32")]
use std::sync::{Arc, Mutex};
#[cfg(feature = "esp32")]
use types::{ApiResult, SendMessageResult, Update};

const GET_UPDATES_LIMIT: u8 = 10;
const MIN_POLL_TIMEOUT_SEC: u32 = 1;
const MAX_POLL_TIMEOUT_SEC: u32 = 30;

/// Build a bounded `getUpdates` request body for the embedded runtime.
pub fn build_get_updates_body(since: i64, timeout_sec: u32) -> String {
    let timeout = timeout_sec.clamp(MIN_POLL_TIMEOUT_SEC, MAX_POLL_TIMEOUT_SEC);
    format!(
        r#"{{"offset":{},"timeout":{},"limit":{},"allowed_updates":["message"]}}"#,
        since, timeout, GET_UPDATES_LIMIT
    )
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
        match &mut self.transport {
            Transport::Wifi(http) => http
                .post(&path, body)
                .map_err(|e| MessengerError::Http(e.to_string())),
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
            Err(MessengerError::Api(result.description.unwrap_or_default()))
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
        let escaped = types::json_escape(text);
        let body = format!(r#"{{"chat_id":{},"text":"{}"}}"#, self.chat_id, escaped);
        let result: ApiResult<SendMessageResult> = self.post_and_parse("sendMessage", &body)?;
        let r = Self::check_ok(result)?;
        Ok(r.map(|r| r.message_id).unwrap_or(0))
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
        let result: ApiResult<Vec<Update>> = self.post_and_parse("getUpdates", &body)?;
        let updates = Self::check_ok(result)?.unwrap_or_default();
        let messages = updates
            .into_iter()
            .filter_map(|u| {
                let msg = u.message?;
                // Reject messages from any chat other than the configured one.
                // Without this, anyone who adds the bot to a group can trigger commands.
                if msg.chat.id != self.chat_id {
                    log::warn!(
                        "[tg] message from unexpected chat {} — ignored",
                        msg.chat.id
                    );
                    return None;
                }
                let text = msg.text.clone().unwrap_or_default();
                if text.is_empty() {
                    return None;
                }
                Some(InboundMessage {
                    cursor: u.update_id + 1,
                    text,
                    reply_to: msg.reply_to_message.map(|r| r.message_id),
                })
            })
            .collect();
        Ok(messages)
    }
}
