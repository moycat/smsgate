//! SMS inbound processing: CMTI handling and boot-time sweep.
//!
//! Extracted from main.rs so it can be unit-tested against ScriptedModem.

use crate::bridge::{forwarder::forward_sms, reply_router::ReplyRouter};
use crate::im::MessageSink;
use crate::log_ring::LogRing;
use crate::modem::ModemPort;
use crate::persist::Store;
use crate::sms::{codec::parse_sms_pdu, concat::ConcatReassembler, SmsMessage};

/// Raw SMS PDU read from a modem storage slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSms {
    pub mem: String,
    pub index: u16,
    pub pdu_hex: String,
}

/// Process a +CMTI notification: read the PDU, forward it, delete on success.
///
/// Returns `true` if the modem slot was deleted (forwarded OK or unparseable),
/// `false` if forwarding failed and the slot should be retried on next boot.
#[allow(clippy::too_many_arguments)]
pub fn handle_new_sms(
    mem: &str,
    index: u16,
    modem: &mut dyn ModemPort,
    router: &mut ReplyRouter,
    log: &mut LogRing,
    concat: &mut ConcatReassembler,
    messenger: &mut dyn MessageSink,
    store: &mut dyn Store,
) -> bool {
    let Some(stored) = read_new_sms_pdu(mem, index, modem) else {
        return false;
    };

    let delete = process_pdu_hex(
        &stored.pdu_hex,
        stored.index,
        router,
        log,
        concat,
        messenger,
        store,
    );
    if delete {
        delete_sms_slot(stored.index, modem);
    } else {
        log::warn!(
            "[sms_handler] forward failed — SMS stays at mem={} slot={}",
            stored.mem,
            stored.index
        );
    }
    delete
}

/// Read one SMS PDU from the modem slot reported by a +CMTI notification.
pub fn read_new_sms_pdu(mem: &str, index: u16, modem: &mut dyn ModemPort) -> Option<StoredSms> {
    log::info!("[sms_handler] +CMTI: mem={} index={}", mem, index);

    let _ = modem.send_at(&format!("+CPMS=\"{}\"", mem));
    let r = modem.send_at(&format!("+CMGR={}", index));
    let pdu_hex = match &r {
        Ok(resp) if resp.ok => {
            log::info!("[sms_handler] AT+CMGR={} body: {:?}", index, resp.body);
            resp.body
                .lines()
                .find(|l| !l.starts_with("+CMGR:") && !l.is_empty())
                .map(|s| s.trim().to_string())
        }
        Ok(resp) => {
            log::warn!(
                "[sms_handler] AT+CMGR={} error: {}",
                index,
                resp.body.trim()
            );
            None
        }
        Err(e) => {
            log::warn!("[sms_handler] AT+CMGR={} failed: {:?}", index, e);
            None
        }
    };

    let Some(hex) = pdu_hex else {
        log::warn!(
            "[sms_handler] could not read SMS at mem={} slot={}",
            mem,
            index
        );
        return None;
    };

    Some(StoredSms {
        mem: mem.to_string(),
        index,
        pdu_hex: hex,
    })
}

/// Delete an SMS storage slot after the message has been consumed.
pub fn delete_sms_slot(index: u16, modem: &mut dyn ModemPort) {
    let _ = modem.send_at(&format!("+CMGD={}", index));
}

/// Sweep all stored SMS in one memory bank at boot time.
pub fn sweep_one_storage(
    mem: &str,
    modem: &mut dyn ModemPort,
    router: &mut ReplyRouter,
    log: &mut LogRing,
    concat: &mut ConcatReassembler,
    messenger: &mut dyn MessageSink,
    store: &mut dyn Store,
) {
    for stored in read_stored_sms(mem, modem) {
        let delete = process_pdu_hex(
            &stored.pdu_hex,
            stored.index,
            router,
            log,
            concat,
            messenger,
            store,
        );
        if delete {
            delete_sms_slot(stored.index, modem);
        } else {
            log::warn!(
                "[sms_handler] sweep forward failed — SMS stays at {} slot {}",
                stored.mem,
                stored.index
            );
        }
    }
}

/// Read all stored SMS PDUs from one memory bank.
pub fn read_stored_sms(mem: &str, modem: &mut dyn ModemPort) -> Vec<StoredSms> {
    let Some((cmd, resp)) = list_stored_sms(mem, modem) else {
        return Vec::new();
    };
    log::info!(
        "[sms_handler] sweep {} AT{} body: {:?}",
        mem,
        cmd,
        resp.body
    );

    let body = resp.body.clone();
    let mut stored = Vec::new();
    let mut lines = body.lines().peekable();
    while let Some(line) = lines.next() {
        if let Some(rest) = line.strip_prefix("+CMGL: ") {
            let slot: u16 = rest
                .split(',')
                .next()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);
            if let Some(hex) = lines.next() {
                let hex = hex.trim();
                log::info!("[sms_handler] sweep found SMS in {} slot {}", mem, slot);
                stored.push(StoredSms {
                    mem: mem.to_string(),
                    index: slot,
                    pdu_hex: hex.to_string(),
                });
            }
        }
    }
    stored
}

fn list_stored_sms(
    mem: &str,
    modem: &mut dyn ModemPort,
) -> Option<(&'static str, crate::modem::AtResponse)> {
    const LIST_COMMANDS: [&str; 2] = ["+CMGL=4", "+CMGL=\"ALL\""];

    for cmd in LIST_COMMANDS {
        match modem.send_at(cmd) {
            Ok(resp) if resp.ok => return Some((cmd, resp)),
            Ok(resp) => {
                log::warn!(
                    "[sms_handler] sweep {} AT{} error: {}",
                    mem,
                    cmd,
                    resp.body.trim()
                );
            }
            Err(e) => {
                log::warn!("[sms_handler] sweep {} AT{} failed: {:?}", mem, cmd, e);
            }
        }
    }

    None
}

/// Parse a PDU hex string and forward the SMS.
///
/// Returns `true` if the modem slot should be deleted:
/// - single SMS forwarded successfully
/// - concat partial (slot consumed; delete to free storage)
/// - unparseable PDU (no point retaining)
///
/// Returns `false` if forwarding failed (keep for retry on next boot).
pub fn process_pdu_hex(
    hex: &str,
    slot: u16,
    router: &mut ReplyRouter,
    log: &mut LogRing,
    concat: &mut ConcatReassembler,
    messenger: &mut dyn MessageSink,
    store: &mut dyn Store,
) -> bool {
    let pdu = match parse_sms_pdu(hex) {
        Ok(p) => p,
        Err(e) => {
            log::error!("[sms_handler] PDU parse error at slot {}: {}", slot, e);
            return true; // unparseable — no point keeping it
        }
    };

    let sms = if pdu.is_concatenated {
        match concat.feed(&pdu) {
            Some(complete) => SmsMessage {
                sender: complete.sender,
                body: complete.content,
                timestamp: complete.timestamp,
                slot,
            },
            None => return true, // concat partial consumed; delete to free modem slot
        }
    } else {
        SmsMessage {
            sender: pdu.sender,
            body: pdu.content,
            timestamp: pdu.timestamp,
            slot,
        }
    };

    forward_sms(&sms, messenger, router, log, store).is_some()
}
