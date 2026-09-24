//! SMS → IM forwarding core logic.

use crate::bridge::reply_router::ReplyRouter;
use crate::im::{MessageFormat, MessageId, MessageSink, MessengerError};
use crate::log_ring::{LogEntry, LogEvent, LogKind, LogRing};
use crate::persist::{keys, load_bool, Store};
use crate::sms::{codec::human_readable_phone, SmsMessage};

const SMS_LOG_PREVIEW_CHARS: usize = 160;

/// Format an inbound SMS for Telegram without sending it.
pub fn sms_forward_text(sms: &SmsMessage) -> String {
    let display = human_readable_phone(&sms.sender);
    crate::i18n::sms_received(
        &display,
        &crate::sms::codec::timestamp_to_rfc3339(&sms.timestamp, 480),
        &sms.body,
    )
}

/// Finalize a confirmed IM delivery before its modem slots are deleted.
pub fn record_forward_success(
    sms: &SmsMessage,
    msg_id: MessageId,
    router: &mut ReplyRouter,
    log: &mut LogRing,
    store: &mut dyn Store,
    log_timestamp: &str,
) {
    if !router.put(msg_id, &sms.sender, store) {
        log.push(
            LogEvent::new(
                LogKind::System,
                "reply map",
                "failed to persist SMS reply mapping",
                false,
            )
            .at(log_timestamp),
        );
    }
    log.push(sms_log_entry(sms, log_timestamp, true));
    log::info!(
        "[forwarder] forwarded SMS from {} → msg_id={}",
        sms.sender,
        msg_id
    );
}

/// Record a failed IM delivery while retaining any modem storage slots.
pub fn record_forward_failure(
    sms: &SmsMessage,
    error: &MessengerError,
    log: &mut LogRing,
    log_timestamp: &str,
) {
    log::error!("[forwarder] send failed: {}", error);
    log.push(sms_log_entry(sms, log_timestamp, false));
    log.push(
        LogEvent::network("telegram", &format!("send failed: {}", error), false).at(log_timestamp),
    );
}

fn sms_log_entry(sms: &SmsMessage, log_timestamp: &str, forwarded: bool) -> LogEntry {
    LogEntry::sms(
        sms.sender.clone(),
        sms_log_preview(sms),
        log_timestamp.to_string(),
        forwarded,
    )
}

/// Process and forward one SMS. Returns the IM MessageId on success.
pub fn forward_sms(
    sms: &SmsMessage,
    messenger: &mut dyn MessageSink,
    router: &mut ReplyRouter,
    log: &mut LogRing,
    store: &mut dyn Store,
    log_timestamp: &str,
) -> Option<MessageId> {
    if load_bool(store, keys::FWD_ENABLED) == Some(false) {
        log::info!(
            "[forwarder] forwarding paused — dropping SMS from {}",
            sms.sender
        );
        log.push(sms_log_entry(sms, log_timestamp, false));
        return None;
    }

    if is_blocked(&sms.sender, store) {
        log::info!("[forwarder] {} is blocked — dropped", sms.sender);
        log.push(sms_log_entry(sms, log_timestamp, false));
        return None;
    }

    let text = sms_forward_text(sms);

    match messenger.send_message_with_format(&text, MessageFormat::Html) {
        Ok(msg_id) => {
            record_forward_success(sms, msg_id, router, log, store, log_timestamp);
            Some(msg_id)
        }
        Err(e) => {
            record_forward_failure(sms, &e, log, log_timestamp);
            None
        }
    }
}

fn sms_log_preview(sms: &SmsMessage) -> String {
    let (body, _) = crate::text::char_prefix(&sms.body, SMS_LOG_PREVIEW_CHARS);
    if sms.timestamp.is_empty() {
        body.to_string()
    } else {
        let sms_time_len = if sms.timestamp.len() < 17 {
            sms.timestamp.len()
        } else {
            "2026-04-10T12:00:00+08:00".len()
        };
        let mut preview = String::with_capacity("sms_time=".len() + sms_time_len + 1 + body.len());
        preview.push_str("sms_time=");
        if !crate::sms::codec::push_timestamp_rfc3339(&mut preview, &sms.timestamp, 480) {
            preview.push_str(&sms.timestamp);
        }
        preview.push(' ');
        preview.push_str(body);
        preview
    }
}

#[cfg(feature = "testing")]
pub fn sms_log_preview_for_test(sms: &SmsMessage) -> String {
    sms_log_preview(sms)
}

/// Check if `phone` is in the block list.
pub fn is_blocked(phone: &str, store: &dyn Store) -> bool {
    let Some(bytes) = store.load(keys::BLOCK_LIST) else {
        return false;
    };
    let Ok(list) = serde_json::from_slice::<Vec<String>>(&bytes) else {
        return false;
    };
    let normalized = crate::sms::codec::normalize_phone(phone);
    list.iter().any(|b| {
        let nb = crate::sms::codec::normalize_phone(b);
        nb == normalized || normalized.ends_with(&nb) || nb.ends_with(&normalized)
    })
}

/// Add a phone number to the block list.
pub fn add_to_blocklist(
    phone: &str,
    store: &mut dyn Store,
) -> Result<(), crate::persist::StoreError> {
    let mut list = load_blocklist(store);
    let n = crate::sms::codec::normalize_phone(phone);
    if !list.contains(&n) {
        list.push(n);
        save_blocklist(&list, store)?;
    }
    Ok(())
}

/// Remove a phone number from the block list.
pub fn remove_from_blocklist(
    phone: &str,
    store: &mut dyn Store,
) -> Result<bool, crate::persist::StoreError> {
    let mut list = load_blocklist(store);
    let n = crate::sms::codec::normalize_phone(phone);
    let before = list.len();
    list.retain(|b| *b != n);
    if list.len() < before {
        save_blocklist(&list, store)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

pub fn load_blocklist(store: &dyn Store) -> Vec<String> {
    store
        .load(keys::BLOCK_LIST)
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_blocklist(
    list: &[String],
    store: &mut dyn Store,
) -> Result<(), crate::persist::StoreError> {
    let bytes =
        serde_json::to_vec(list).map_err(|e| crate::persist::StoreError::Serde(e.to_string()))?;
    store.save(keys::BLOCK_LIST, &bytes)
}
