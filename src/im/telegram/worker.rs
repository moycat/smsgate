//! Dedicated Telegram send worker.
//!
//! The polling thread owns its own Telegram client for `getUpdates`; this worker
//! owns the outbound client for notifications and command replies.

use super::{
    build_set_my_commands_body, http::TelegramHttpClient, send_retry_delay_after,
    should_restart_after_send_retry, should_retry_send_error, telegram_restart_after,
    telegram_send_retry_interval, TelegramMessenger,
};
use crate::im::{InlineKeyboard, MessageFormat, MessageId, MessageSink, MessengerError};
use crate::log_ring::LogEvent;
use crate::modem::ModemPort;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{sync_channel, Receiver, RecvTimeoutError, Sender, SyncSender},
    Arc, Mutex,
};
use std::time::Instant;

const OUTBOUND_QUEUE_DEPTH: usize = 8;

#[derive(Debug, Clone)]
pub enum TelegramSendEvent {
    Log(LogEvent),
    Restart(LogEvent),
}

fn emit_event(tx: &Sender<TelegramSendEvent>, event: TelegramSendEvent) {
    let _ = tx.send(event);
}

fn emit_log(tx: &Sender<TelegramSendEvent>, detail: &str) {
    log::error!("[tg-send] {}", detail);
    emit_event(
        tx,
        TelegramSendEvent::Log(LogEvent::network("telegram", detail, false)),
    );
}

fn emit_restart(tx: &Sender<TelegramSendEvent>, detail: &str) {
    log::error!("[tg-send] {}", detail);
    emit_event(
        tx,
        TelegramSendEvent::Restart(LogEvent::network("telegram", detail, false)),
    );
}

fn reset_current_task_watchdog() {
    unsafe {
        let _ = esp_idf_sys::esp_task_wdt_reset();
    }
}

fn run_with_retries<T, F>(
    operation: &'static str,
    event_tx: &Sender<TelegramSendEvent>,
    mut operation_fn: F,
) -> Result<T, MessengerError>
where
    F: FnMut() -> Result<T, MessengerError>,
{
    let started = Instant::now();
    let mut attempt: u16 = 1;

    loop {
        let attempt_started = Instant::now();
        match operation_fn() {
            Ok(value) => return Ok(value),
            Err(e) => {
                let attempt_elapsed = attempt_started.elapsed();
                let total_elapsed = started.elapsed();
                let error = e.to_string();
                emit_log(
                    event_tx,
                    &format!(
                        "{} failed attempt {} after {}s: {}",
                        operation,
                        attempt,
                        attempt_elapsed.as_secs(),
                        error
                    ),
                );

                if !should_retry_send_error(&e) {
                    return Err(e);
                }

                if should_restart_after_send_retry(total_elapsed) {
                    let detail = format!(
                        "{} retrying for {}s; rebooting: {}",
                        operation,
                        total_elapsed.as_secs(),
                        error
                    );
                    emit_restart(event_tx, &detail);
                    return Err(MessengerError::Timeout(detail));
                }

                let retry_delay = send_retry_delay_after(attempt_elapsed);
                if !retry_delay.is_zero() {
                    let remaining_before_reboot =
                        telegram_restart_after().saturating_sub(total_elapsed);
                    let delay = retry_delay.min(remaining_before_reboot);
                    std::thread::sleep(delay);
                    if should_restart_after_send_retry(started.elapsed()) {
                        let detail = format!(
                            "{} retrying for {}s; rebooting before next retry",
                            operation,
                            started.elapsed().as_secs()
                        );
                        emit_restart(event_tx, &detail);
                        return Err(MessengerError::Timeout(detail));
                    }
                }

                attempt = attempt.saturating_add(1);
            }
        }
    }
}

enum Request {
    Send {
        text: String,
        keyboard: Option<InlineKeyboard>,
        format: MessageFormat,
        reply: SyncSender<Result<MessageId, MessengerError>>,
    },
    Edit {
        message_id: MessageId,
        text: String,
        keyboard: Option<InlineKeyboard>,
        format: MessageFormat,
        reply: SyncSender<Result<(), MessengerError>>,
    },
    AnswerCallback {
        callback_query_id: String,
        text: Option<String>,
        reply: SyncSender<Result<(), MessengerError>>,
    },
    RegisterCommands {
        body: String,
        reply: SyncSender<Result<(), MessengerError>>,
    },
}

/// Synchronous `MessageSink` facade backed by a dedicated Telegram worker task.
pub struct TelegramSendWorker {
    tx: SyncSender<Request>,
    event_tx: Sender<TelegramSendEvent>,
}

impl TelegramSendWorker {
    pub fn spawn(
        modem: Arc<Mutex<dyn ModemPort + Send>>,
        token: String,
        chat_id: i64,
        transport_cellular: Arc<AtomicBool>,
        event_tx: Sender<TelegramSendEvent>,
    ) -> Self {
        let (tx, rx) = sync_channel::<Request>(OUTBOUND_QUEUE_DEPTH);
        let worker_event_tx = event_tx.clone();
        std::thread::Builder::new()
            .name("tg-send".into())
            .stack_size(16 * 1024)
            .spawn(move || {
                let mut current_cellular = transport_cellular.load(Ordering::SeqCst);
                let mut messenger =
                    build_worker_messenger(current_cellular, modem.clone(), token.clone(), chat_id)
                        .ok();

                while let Ok(req) = rx.recv() {
                    let desired_cellular = transport_cellular.load(Ordering::SeqCst);
                    if desired_cellular != current_cellular || messenger.is_none() {
                        messenger = build_worker_messenger(
                            desired_cellular,
                            modem.clone(),
                            token.clone(),
                            chat_id,
                        )
                        .ok();
                        current_cellular = desired_cellular;
                    }

                    match req {
                        Request::Send {
                            text,
                            keyboard,
                            format,
                            reply,
                        } => {
                            let result = run_with_retries("sendMessage", &worker_event_tx, || {
                                ensure_worker_messenger(
                                    &mut messenger,
                                    current_cellular,
                                    &modem,
                                    &token,
                                    chat_id,
                                );
                                let result = match messenger.as_mut() {
                                    Some(m) => match keyboard.as_ref() {
                                        Some(keyboard) => m.send_message_with_keyboard_and_format(
                                            &text, keyboard, format,
                                        ),
                                        None => m.send_message_with_format(&text, format),
                                    },
                                    None => Err(MessengerError::Disconnected),
                                };
                                if result.as_ref().err().is_some_and(should_retry_send_error) {
                                    messenger = None;
                                }
                                result
                            });
                            let _ = reply.send(result);
                        }
                        Request::Edit {
                            message_id,
                            text,
                            keyboard,
                            format,
                            reply,
                        } => {
                            let result =
                                run_with_retries("editMessageText", &worker_event_tx, || {
                                    ensure_worker_messenger(
                                        &mut messenger,
                                        current_cellular,
                                        &modem,
                                        &token,
                                        chat_id,
                                    );
                                    let result = match messenger.as_mut() {
                                        Some(m) => match keyboard.as_ref() {
                                            Some(keyboard) => m
                                                .edit_message_with_keyboard_and_format(
                                                    message_id, &text, keyboard, format,
                                                ),
                                            None => m.edit_message_with_format(
                                                message_id, &text, format,
                                            ),
                                        },
                                        None => Err(MessengerError::Disconnected),
                                    };
                                    if result.as_ref().err().is_some_and(should_retry_send_error) {
                                        messenger = None;
                                    }
                                    result
                                });
                            let _ = reply.send(result);
                        }
                        Request::AnswerCallback {
                            callback_query_id,
                            text,
                            reply,
                        } => {
                            let result =
                                run_with_retries("answerCallbackQuery", &worker_event_tx, || {
                                    ensure_worker_messenger(
                                        &mut messenger,
                                        current_cellular,
                                        &modem,
                                        &token,
                                        chat_id,
                                    );
                                    let result = match messenger.as_mut() {
                                        Some(m) => m.answer_callback_query(
                                            &callback_query_id,
                                            text.as_deref(),
                                        ),
                                        None => Err(MessengerError::Disconnected),
                                    };
                                    if result.as_ref().err().is_some_and(should_retry_send_error) {
                                        messenger = None;
                                    }
                                    result
                                });
                            let _ = reply.send(result);
                        }
                        Request::RegisterCommands { body, reply } => {
                            let result =
                                run_with_retries("setMyCommands", &worker_event_tx, || {
                                    ensure_worker_messenger(
                                        &mut messenger,
                                        current_cellular,
                                        &modem,
                                        &token,
                                        chat_id,
                                    );
                                    let result = match messenger.as_mut() {
                                        Some(m) => m.register_commands_body(&body),
                                        None => Err(MessengerError::Disconnected),
                                    };
                                    if result.as_ref().err().is_some_and(should_retry_send_error) {
                                        messenger = None;
                                    }
                                    result
                                });
                            let _ = reply.send(result);
                        }
                    }
                }
            })
            .expect("failed to spawn tg-send thread");

        TelegramSendWorker { tx, event_tx }
    }

    pub fn register_commands(&mut self, commands: &[(&str, &str)]) -> Result<(), MessengerError> {
        let (reply, rx) = sync_channel(1);
        let body = build_set_my_commands_body(commands);
        self.tx
            .send(Request::RegisterCommands { body, reply })
            .map_err(|_| MessengerError::Disconnected)?;
        self.wait_for_reply("setMyCommands", rx)
    }

    fn wait_for_reply<T>(
        &self,
        operation: &'static str,
        rx: Receiver<Result<T, MessengerError>>,
    ) -> Result<T, MessengerError> {
        let started = Instant::now();
        let mut timeouts: u16 = 0;
        loop {
            match rx.recv_timeout(telegram_send_retry_interval()) {
                Ok(result) => return result,
                Err(RecvTimeoutError::Timeout) => {
                    reset_current_task_watchdog();
                    timeouts = timeouts.saturating_add(1);
                    let elapsed = started.elapsed();
                    emit_log(
                        &self.event_tx,
                        &format!(
                            "{} request timed out waiting for tg-send worker x{} after {}s",
                            operation,
                            timeouts,
                            elapsed.as_secs()
                        ),
                    );
                    if should_restart_after_send_retry(elapsed) {
                        let detail = format!(
                            "{} request stuck for {}s; rebooting",
                            operation,
                            elapsed.as_secs()
                        );
                        emit_restart(&self.event_tx, &detail);
                        return Err(MessengerError::Timeout(detail));
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return Err(MessengerError::Disconnected),
            }
        }
    }
}

impl MessageSink for TelegramSendWorker {
    fn send_message(&mut self, text: &str) -> Result<MessageId, MessengerError> {
        let (reply, rx) = sync_channel(1);
        self.tx
            .send(Request::Send {
                text: text.to_string(),
                keyboard: None,
                format: MessageFormat::Plain,
                reply,
            })
            .map_err(|_| MessengerError::Disconnected)?;
        self.wait_for_reply("sendMessage", rx)
    }

    fn send_message_with_keyboard(
        &mut self,
        text: &str,
        keyboard: &InlineKeyboard,
    ) -> Result<MessageId, MessengerError> {
        let (reply, rx) = sync_channel(1);
        self.tx
            .send(Request::Send {
                text: text.to_string(),
                keyboard: Some(keyboard.clone()),
                format: MessageFormat::Plain,
                reply,
            })
            .map_err(|_| MessengerError::Disconnected)?;
        self.wait_for_reply("sendMessage", rx)
    }

    fn send_message_with_format(
        &mut self,
        text: &str,
        format: MessageFormat,
    ) -> Result<MessageId, MessengerError> {
        let (reply, rx) = sync_channel(1);
        self.tx
            .send(Request::Send {
                text: text.to_string(),
                keyboard: None,
                format,
                reply,
            })
            .map_err(|_| MessengerError::Disconnected)?;
        self.wait_for_reply("sendMessage", rx)
    }

    fn send_message_with_keyboard_and_format(
        &mut self,
        text: &str,
        keyboard: &InlineKeyboard,
        format: MessageFormat,
    ) -> Result<MessageId, MessengerError> {
        let (reply, rx) = sync_channel(1);
        self.tx
            .send(Request::Send {
                text: text.to_string(),
                keyboard: Some(keyboard.clone()),
                format,
                reply,
            })
            .map_err(|_| MessengerError::Disconnected)?;
        self.wait_for_reply("sendMessage", rx)
    }

    fn edit_message(&mut self, message_id: MessageId, text: &str) -> Result<(), MessengerError> {
        let (reply, rx) = sync_channel(1);
        self.tx
            .send(Request::Edit {
                message_id,
                text: text.to_string(),
                keyboard: None,
                format: MessageFormat::Plain,
                reply,
            })
            .map_err(|_| MessengerError::Disconnected)?;
        self.wait_for_reply("editMessageText", rx)
    }

    fn edit_message_with_keyboard(
        &mut self,
        message_id: MessageId,
        text: &str,
        keyboard: &InlineKeyboard,
    ) -> Result<(), MessengerError> {
        let (reply, rx) = sync_channel(1);
        self.tx
            .send(Request::Edit {
                message_id,
                text: text.to_string(),
                keyboard: Some(keyboard.clone()),
                format: MessageFormat::Plain,
                reply,
            })
            .map_err(|_| MessengerError::Disconnected)?;
        self.wait_for_reply("editMessageText", rx)
    }

    fn edit_message_with_format(
        &mut self,
        message_id: MessageId,
        text: &str,
        format: MessageFormat,
    ) -> Result<(), MessengerError> {
        let (reply, rx) = sync_channel(1);
        self.tx
            .send(Request::Edit {
                message_id,
                text: text.to_string(),
                keyboard: None,
                format,
                reply,
            })
            .map_err(|_| MessengerError::Disconnected)?;
        self.wait_for_reply("editMessageText", rx)
    }

    fn edit_message_with_keyboard_and_format(
        &mut self,
        message_id: MessageId,
        text: &str,
        keyboard: &InlineKeyboard,
        format: MessageFormat,
    ) -> Result<(), MessengerError> {
        let (reply, rx) = sync_channel(1);
        self.tx
            .send(Request::Edit {
                message_id,
                text: text.to_string(),
                keyboard: Some(keyboard.clone()),
                format,
                reply,
            })
            .map_err(|_| MessengerError::Disconnected)?;
        self.wait_for_reply("editMessageText", rx)
    }

    fn answer_callback_query(
        &mut self,
        callback_query_id: &str,
        text: Option<&str>,
    ) -> Result<(), MessengerError> {
        let (reply, rx) = sync_channel(1);
        self.tx
            .send(Request::AnswerCallback {
                callback_query_id: callback_query_id.to_string(),
                text: text.map(str::to_string),
                reply,
            })
            .map_err(|_| MessengerError::Disconnected)?;
        self.wait_for_reply("answerCallbackQuery", rx)
    }
}

fn ensure_worker_messenger(
    messenger: &mut Option<TelegramMessenger>,
    use_cellular: bool,
    modem: &Arc<Mutex<dyn ModemPort + Send>>,
    token: &str,
    chat_id: i64,
) {
    if messenger.is_none() {
        *messenger =
            build_worker_messenger(use_cellular, modem.clone(), token.to_string(), chat_id).ok();
    }
}

fn build_worker_messenger(
    use_cellular: bool,
    modem: Arc<Mutex<dyn ModemPort + Send>>,
    token: String,
    chat_id: i64,
) -> anyhow::Result<TelegramMessenger> {
    if use_cellular {
        Ok(TelegramMessenger::new_modem(modem, token, chat_id))
    } else {
        Ok(TelegramMessenger::new_wifi(
            TelegramHttpClient::new(None)?,
            token,
            chat_id,
        ))
    }
}
