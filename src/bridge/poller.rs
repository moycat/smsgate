//! IM message poll loop and command dispatcher.

use crate::bridge::reply_router::ReplyRouter;
use crate::commands::{
    builtin::log_cmd, decode_sentinel_body, CommandContext, CommandRegistry, BLOCK_SENTINEL,
    PAUSE_SENTINEL, RESTART_SENTINEL, RESUME_SENTINEL, SEND_SENTINEL, UNBLOCK_SENTINEL,
};
use crate::im::{CommandResponder, InboundMessage, MessageFormat, MessengerError};
use crate::log_ring::{LogEvent, LogRing};
use crate::modem::ModemStatus;
use crate::persist::{keys, save_bool, save_i64, Store, StoreError};
use crate::sms::sender::{CmdSendResult, SmsSender};

/// Process a batch of inbound IM messages: dispatch commands and route replies to SMS.
/// Result of processing a Telegram batch.
#[derive(Debug, Clone, Default)]
pub struct DispatchOutcome {
    pub restart_requested: bool,
    pub restart_reply: Option<String>,
    pub pause_mins: Option<u32>,
    pub events: Vec<LogEvent>,
}

/// Durably checkpoint a polled update before allowing the polling task to
/// advance its Telegram offset. A failed write leaves `persisted` unchanged.
pub fn checkpoint_cursor(
    store: &mut dyn Store,
    persisted: &mut i64,
    next: i64,
) -> Result<(), StoreError> {
    if next > *persisted {
        save_i64(store, keys::IM_CURSOR, next)?;
        *persisted = next;
    }
    Ok(())
}

/// Process a batch of inbound IM messages: dispatch commands and route replies to SMS.
/// Returns command side effects plus machine-log events.
///
/// Polling is handled by a dedicated background thread; this function only processes
/// messages that have already been received. Cursor persistence is the caller's responsibility.
#[allow(clippy::too_many_arguments)]
pub fn poll_and_dispatch(
    messages: &[InboundMessage],
    messenger: &mut dyn CommandResponder,
    sender: &mut SmsSender,
    router: &ReplyRouter,
    registry: &CommandRegistry,
    store: &mut dyn Store,
    log: &LogRing,
    modem_status: &ModemStatus,
    uptime_ms: u32,
    free_heap_bytes: u32,
    min_free_heap_bytes: u32,
    wifi_info: &str,
) -> Result<DispatchOutcome, MessengerError> {
    let mut restart_requested = false;
    let mut restart_reply = None;
    let mut pause_mins: Option<u32> = None;
    let mut events = Vec::new();

    for msg in messages {
        if msg.document.is_some() {
            continue;
        }

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
                    min_free_heap_bytes,
                    wifi_info,
                };
                let page = log_cmd::render_log_page(&ctx, offset);
                let result = messenger.edit_reply(
                    callback.message_id,
                    &page.text,
                    page.keyboard.as_ref(),
                    page.format,
                );
                if let Err(e) = result {
                    log::error!("[poller] log page edit failed: {}", e);
                    events.push(LogEvent::network(
                        "telegram",
                        &format!("log page edit failed: {e}"),
                        false,
                    ));
                }
                if let Err(e) = messenger.answer_callback(&callback.id, None) {
                    log::warn!("[poller] callback answer failed: {}", e);
                    events.push(LogEvent::network(
                        "telegram",
                        &format!("callback answer failed: {e}"),
                        false,
                    ));
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
                    min_free_heap_bytes,
                    wifi_info,
                };
                let page = log_cmd::render_log_page(&ctx, log_cmd::parse_log_offset(args));
                let result = messenger.send_reply(&page.text, page.keyboard.as_ref(), page.format);
                if let Err(e) = result {
                    log::error!("[poller] log reply failed: {}", e);
                    events.push(LogEvent::network(
                        "telegram",
                        &format!("log reply enqueue failed: {e}"),
                        false,
                    ));
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
                min_free_heap_bytes,
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
                    if should_restart {
                        restart_reply = Some(display.to_string());
                    } else if let Err(e) = messenger.send_reply(display, None, MessageFormat::Plain)
                    {
                        log::error!("[poller] command reply failed: {}", e);
                        events.push(LogEvent::network(
                            "telegram",
                            &format!("command reply enqueue failed: {e}"),
                            false,
                        ));
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
        restart_reply,
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
    let mut display = String::new();
    let mut restart = false;
    let mut pause_mins: Option<u32> = None;
    let mut events = Vec::new();
    let mut suppress_success_line = false;

    for line in reply.lines() {
        if suppress_success_line {
            suppress_success_line = false;
            continue;
        }
        if let Some(rest) = line.strip_prefix(SEND_SENTINEL) {
            // Format: "+phone|body" — body may have \n/\r encoded as escape sequences.
            if let Some((phone, body_encoded)) = rest.split_once('|') {
                let body = decode_sentinel_body(body_encoded);
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
                        push_display_line(&mut display, crate::i18n::send_rate_limited());
                        events.push(LogEvent::user("/send", "rate limited", false));
                    }
                }
            }
        } else if let Some(phone) = line.strip_prefix(BLOCK_SENTINEL) {
            log::info!("[poller] sentinel: block {}", phone);
            match crate::bridge::forwarder::add_to_blocklist(phone, store) {
                Ok(()) => events.push(LogEvent::user(
                    "/block",
                    &format!("blocked {}", phone),
                    true,
                )),
                Err(error) => {
                    push_display_line(&mut display, crate::i18n::storage_write_failed());
                    events.push(LogEvent::user(
                        "/block",
                        &format!("block failed: {error}"),
                        false,
                    ));
                    suppress_success_line = true;
                }
            }
        } else if let Some(phone) = line.strip_prefix(UNBLOCK_SENTINEL) {
            log::info!("[poller] sentinel: unblock {}", phone);
            match crate::bridge::forwarder::remove_from_blocklist(phone, store) {
                Ok(true) => events.push(LogEvent::user(
                    "/unblock",
                    &format!("unblocked {}", phone),
                    true,
                )),
                Ok(false) => {
                    push_display_line(&mut display, &crate::i18n::unblock_not_found(phone));
                    events.push(LogEvent::user(
                        "/unblock",
                        &format!("unblock target {} not found", phone),
                        false,
                    ));
                    suppress_success_line = true;
                }
                Err(error) => {
                    push_display_line(&mut display, crate::i18n::storage_write_failed());
                    events.push(LogEvent::user(
                        "/unblock",
                        &format!("unblock failed: {error}"),
                        false,
                    ));
                    suppress_success_line = true;
                }
            }
        } else if let Some(rest) = line.strip_prefix(PAUSE_SENTINEL) {
            let mins: u32 = rest.trim().parse().unwrap_or(60);
            log::info!("[poller] sentinel: pause forwarding for {} min", mins);
            match save_bool(store, keys::FWD_ENABLED, false) {
                Ok(()) => {
                    pause_mins = Some(mins);
                    events.push(LogEvent::user(
                        "/pause",
                        &format!("paused forwarding for {} min", mins),
                        true,
                    ));
                }
                Err(error) => {
                    push_display_line(&mut display, crate::i18n::storage_write_failed());
                    events.push(LogEvent::user(
                        "/pause",
                        &format!("pause failed: {error}"),
                        false,
                    ));
                    suppress_success_line = true;
                }
            }
        } else if line.starts_with(RESUME_SENTINEL) {
            log::info!("[poller] sentinel: resume forwarding");
            match save_bool(store, keys::FWD_ENABLED, true) {
                Ok(()) => events.push(LogEvent::user("/resume", "resumed forwarding", true)),
                Err(error) => {
                    push_display_line(&mut display, crate::i18n::storage_write_failed());
                    events.push(LogEvent::user(
                        "/resume",
                        &format!("resume failed: {error}"),
                        false,
                    ));
                    suppress_success_line = true;
                }
            }
        } else if line.starts_with(RESTART_SENTINEL) {
            log::info!("[poller] sentinel: restart requested");
            restart = true;
            events.push(LogEvent::user("/restart", "restart requested", true));
        } else {
            push_display_line(&mut display, line);
        }
    }

    (display, restart, pause_mins, events)
}

fn push_display_line(display: &mut String, line: &str) {
    if !display.is_empty() {
        display.push('\n');
    }
    display.push_str(line);
}

fn preview(body: &str) -> String {
    crate::text::char_prefix(body, 50).0.to_string()
}
