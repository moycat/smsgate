//! IM message poll loop and command dispatcher.

use crate::bridge::reply_router::ReplyRouter;
use crate::commands::{
    CommandContext, CommandRegistry, BLOCK_SENTINEL, PAUSE_SENTINEL, RESTART_SENTINEL,
    RESUME_SENTINEL, SEND_SENTINEL, UNBLOCK_SENTINEL,
};
use crate::im::{InboundMessage, MessageSink, MessengerError};
use crate::log_ring::LogRing;
use crate::modem::ModemStatus;
use crate::persist::{keys, save_bool, Store};
use crate::sms::sender::{CmdSendResult, SmsSender};

/// Process a batch of inbound IM messages: dispatch commands and route replies to SMS.
/// Returns `(restart_requested, pause_mins)`.
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
) -> Result<(bool, Option<u32>), MessengerError> {
    let mut restart_requested = false;
    let mut pause_mins: Option<u32> = None;

    for msg in messages {
        let text = msg.text.trim();

        if text.starts_with('/') {
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
                let (clean, should_restart, maybe_pause) = apply_sentinels(&reply, sender, store);
                if should_restart {
                    restart_requested = true;
                }
                if maybe_pause.is_some() {
                    pause_mins = maybe_pause;
                }
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
                if sender.enqueue(phone, text.to_string()).is_none() {
                    log::warn!("[poller] queue full — reply dropped");
                }
            } else {
                log::warn!("[poller] reply_to={} not found in router", reply_to_id);
            }
        } else {
            log::debug!("[poller] non-command non-reply message ignored: {}", text);
        }
    }

    Ok((restart_requested, pause_mins))
}

/// Parse sentinel lines from a command reply, apply their side effects, and return
/// `(display_text, restart_requested, pause_mins)`.
fn apply_sentinels(
    reply: &str,
    sender: &mut SmsSender,
    store: &mut dyn Store,
) -> (String, bool, Option<u32>) {
    let mut display_lines = Vec::new();
    let mut restart = false;
    let mut pause_mins: Option<u32> = None;

    for line in reply.lines() {
        if let Some(rest) = line.strip_prefix(SEND_SENTINEL) {
            // Format: "+phone|body" — body may have \n/\r encoded as escape sequences.
            if let Some((phone, body_encoded)) = rest.split_once('|') {
                let body = body_encoded
                    .replace("\\n", "\n")
                    .replace("\\r", "\r")
                    .replace("\\\\", "\\");
                log::info!("[poller] sentinel: enqueue SMS to {}", phone);
                match sender.enqueue_command_send(phone.to_string(), body) {
                    CmdSendResult::Enqueued(_) => {}
                    CmdSendResult::QueueFull => {
                        log::warn!("[poller] queue full — /send dropped");
                    }
                    CmdSendResult::RateLimited => {
                        display_lines.push(crate::i18n::send_rate_limited());
                    }
                }
            }
        } else if let Some(phone) = line.strip_prefix(BLOCK_SENTINEL) {
            log::info!("[poller] sentinel: block {}", phone);
            let _ = crate::bridge::forwarder::add_to_blocklist(phone, store);
        } else if let Some(phone) = line.strip_prefix(UNBLOCK_SENTINEL) {
            log::info!("[poller] sentinel: unblock {}", phone);
            let _ = crate::bridge::forwarder::remove_from_blocklist(phone, store);
        } else if let Some(rest) = line.strip_prefix(PAUSE_SENTINEL) {
            let mins: u32 = rest.trim().parse().unwrap_or(60);
            log::info!("[poller] sentinel: pause forwarding for {} min", mins);
            let _ = save_bool(store, keys::FWD_ENABLED, false);
            pause_mins = Some(mins);
        } else if line.starts_with(RESUME_SENTINEL) {
            log::info!("[poller] sentinel: resume forwarding");
            let _ = save_bool(store, keys::FWD_ENABLED, true);
        } else if line.starts_with(RESTART_SENTINEL) {
            log::info!("[poller] sentinel: restart requested");
            restart = true;
        } else {
            display_lines.push(line);
        }
    }

    (display_lines.join("\n"), restart, pause_mins)
}
