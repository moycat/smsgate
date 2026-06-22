//! IM message poll loop and command dispatcher.

use crate::bridge::reply_router::ReplyRouter;
use crate::commands::{
    builtin::log_cmd, CommandContext, CommandRegistry, BLOCK_SENTINEL, PAUSE_SENTINEL,
    RESTART_SENTINEL, RESUME_SENTINEL, SEND_SENTINEL, UNBLOCK_SENTINEL,
};
use crate::im::{InboundMessage, MessageSink, MessengerError};
use crate::log_ring::{LogEvent, LogRing};
use crate::modem::ModemStatus;
use crate::persist::{keys, save_bool, Store};
use crate::sms::sender::{CmdSendResult, SmsSender};

/// Process a batch of inbound IM messages: dispatch commands and route replies to SMS.
/// Result of processing a Telegram batch.
#[derive(Debug, Clone, Default)]
pub struct DispatchOutcome {
    pub restart_requested: bool,
    pub pause_mins: Option<u32>,
    pub events: Vec<LogEvent>,
}

/// Process a batch of inbound IM messages: dispatch commands and route replies to SMS.
/// Returns command side effects plus machine-log events.
///
/// Polling is handled by a dedicated background thread; this function only processes
/// messages that have already been received. Cursor persistence is the caller's responsibility.
#[allow(clippy::too_many_arguments)]
pub fn poll_and_dispatch(
    messages: &[InboundMessage],
    messenger: &mut dyn MessageSink,
    sender: &mut SmsSender,
    router: &ReplyRouter,
    registry: &CommandRegistry,
    store: &mut dyn Store,
    log: &LogRing,
    modem_status: &ModemStatus,
    uptime_ms: u32,
    free_heap_bytes: u32,
    wifi_info: &str,
) -> Result<DispatchOutcome, MessengerError> {
    let mut restart_requested = false;
    let mut pause_mins: Option<u32> = None;
    let mut events = Vec::new();

    for msg in messages {
        let text = msg.text.trim();

        if let Some(callback) = msg.callback.as_ref() {
            if let Some(offset) = log_cmd::parse_log_callback(&callback.data) {
                let ctx = CommandContext {
                    store: store as &dyn Store,
                    modem_status,
                    log_ring: log,
                    send_queue: sender,
                    uptime_ms,
                    free_heap_bytes,
                    wifi_info,
                };
                let page = log_cmd::render_log_page(&ctx, offset);
                let result = match page.keyboard.as_ref() {
                    Some(keyboard) => messenger.edit_message_with_keyboard_and_format(
                        callback.message_id,
                        &page.text,
                        keyboard,
                        page.format,
                    ),
                    None => messenger.edit_message_with_format(
                        callback.message_id,
                        &page.text,
                        page.format,
                    ),
                };
                if let Err(e) = result {
                    log::error!("[poller] log page edit failed: {}", e);
                }
                if let Err(e) = messenger.answer_callback_query(&callback.id, None) {
                    log::warn!("[poller] callback answer failed: {}", e);
                }
                continue;
            }
        }

        if text.starts_with('/') {
            if let Some(("log", args)) = split_command(text) {
                let ctx = CommandContext {
                    store: store as &dyn Store,
                    modem_status,
                    log_ring: log,
                    send_queue: sender,
                    uptime_ms,
                    free_heap_bytes,
                    wifi_info,
                };
                let page = log_cmd::render_log_page(&ctx, log_cmd::parse_log_offset(args));
                let result = match page.keyboard.as_ref() {
                    Some(keyboard) => messenger.send_message_with_keyboard_and_format(
                        &page.text,
                        keyboard,
                        page.format,
                    ),
                    None => messenger.send_message_with_format(&page.text, page.format),
                };
                if let Err(e) = result {
                    log::error!("[poller] log reply failed: {}", e);
                }
                continue;
            }

            // Bot command
            let ctx = CommandContext {
                store: store as &dyn Store,
                modem_status,
                log_ring: log,
                send_queue: sender,
                uptime_ms,
                free_heap_bytes,
                wifi_info,
            };
            if let Some(reply) = registry.dispatch(text, &ctx) {
                let (clean, should_restart, maybe_pause, mut new_events) =
                    apply_sentinels(&reply, sender, store);
                if should_restart {
                    restart_requested = true;
                }
                if maybe_pause.is_some() {
                    pause_mins = maybe_pause;
                }
                events.append(&mut new_events);
                let display = clean.trim();
                if !display.is_empty() {
                    if let Err(e) = messenger.send_message(display) {
                        log::error!("[poller] command reply failed: {}", e);
                    }
                }
            }
        } else if let Some(reply_to_id) = msg.reply_to {
            // Reply to a forwarded SMS
            if let Some(phone) = router.lookup(reply_to_id) {
                let phone = phone.to_string();
                log::info!("[poller] reply to {} via SMS", phone);
                if sender.enqueue(phone.clone(), text.to_string()).is_none() {
                    log::warn!("[poller] queue full — reply dropped");
                    events.push(LogEvent::user("/reply", "SMS reply queue full", false));
                } else {
                    events.push(LogEvent::user(
                        "/reply",
                        &format!("queued SMS reply to {}", phone),
                        true,
                    ));
                }
            } else {
                log::warn!("[poller] reply_to={} not found in router", reply_to_id);
            }
        } else {
            log::debug!("[poller] non-command non-reply message ignored: {}", text);
        }
    }

    Ok(DispatchOutcome {
        restart_requested,
        pause_mins,
        events,
    })
}

fn split_command(text: &str) -> Option<(&str, &str)> {
    let text = text.strip_prefix('/')?;
    let (name, args) = text
        .split_once(|c: char| c.is_whitespace())
        .unwrap_or((text, ""));
    Some((name.split('@').next().unwrap_or(name), args.trim()))
}

/// Parse sentinel lines from a command reply and apply their side effects.
fn apply_sentinels(
    reply: &str,
    sender: &mut SmsSender,
    store: &mut dyn Store,
) -> (String, bool, Option<u32>, Vec<LogEvent>) {
    let mut display_lines = Vec::new();
    let mut restart = false;
    let mut pause_mins: Option<u32> = None;
    let mut events = Vec::new();

    for line in reply.lines() {
        if let Some(rest) = line.strip_prefix(SEND_SENTINEL) {
            // Format: "+phone|body" — body may have \n/\r encoded as escape sequences.
            if let Some((phone, body_encoded)) = rest.split_once('|') {
                let body = body_encoded
                    .replace("\\n", "\n")
                    .replace("\\r", "\r")
                    .replace("\\\\", "\\");
                let body_preview = preview(&body);
                log::info!("[poller] sentinel: enqueue SMS to {}", phone);
                match sender.enqueue_command_send(phone.to_string(), body) {
                    CmdSendResult::Enqueued(_) => {
                        events.push(LogEvent::user(
                            "/send",
                            &format!("queued SMS to {}: {}", phone, body_preview),
                            true,
                        ));
                    }
                    CmdSendResult::QueueFull => {
                        log::warn!("[poller] queue full — /send dropped");
                        events.push(LogEvent::user("/send", "SMS queue full", false));
                    }
                    CmdSendResult::RateLimited => {
                        display_lines.push(crate::i18n::send_rate_limited());
                        events.push(LogEvent::user("/send", "rate limited", false));
                    }
                }
            }
        } else if let Some(phone) = line.strip_prefix(BLOCK_SENTINEL) {
            log::info!("[poller] sentinel: block {}", phone);
            let ok = crate::bridge::forwarder::add_to_blocklist(phone, store).is_ok();
            events.push(LogEvent::user("/block", &format!("blocked {}", phone), ok));
        } else if let Some(phone) = line.strip_prefix(UNBLOCK_SENTINEL) {
            log::info!("[poller] sentinel: unblock {}", phone);
            let ok = crate::bridge::forwarder::remove_from_blocklist(phone, store);
            events.push(LogEvent::user(
                "/unblock",
                &format!("unblocked {}", phone),
                ok,
            ));
        } else if let Some(rest) = line.strip_prefix(PAUSE_SENTINEL) {
            let mins: u32 = rest.trim().parse().unwrap_or(60);
            log::info!("[poller] sentinel: pause forwarding for {} min", mins);
            let ok = save_bool(store, keys::FWD_ENABLED, false).is_ok();
            pause_mins = Some(mins);
            events.push(LogEvent::user(
                "/pause",
                &format!("paused forwarding for {} min", mins),
                ok,
            ));
        } else if line.starts_with(RESUME_SENTINEL) {
            log::info!("[poller] sentinel: resume forwarding");
            let ok = save_bool(store, keys::FWD_ENABLED, true).is_ok();
            events.push(LogEvent::user("/resume", "resumed forwarding", ok));
        } else if line.starts_with(RESTART_SENTINEL) {
            log::info!("[poller] sentinel: restart requested");
            restart = true;
            events.push(LogEvent::user("/restart", "restart requested", true));
        } else {
            display_lines.push(line);
        }
    }

    (display_lines.join("\n"), restart, pause_mins, events)
}

fn preview(body: &str) -> String {
    body.chars().take(50).collect()
}
