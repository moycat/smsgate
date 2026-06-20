//! SMS inbound processing: CMTI handling and boot-time sweep.
//!
//! Extracted from main.rs so it can be unit-tested against ScriptedModem.

use crate::bridge::{forwarder::forward_sms, reply_router::ReplyRouter};
use crate::im::MessageSink;
use crate::log_ring::LogRing;
use crate::modem::ModemPort;
use crate::persist::Store;
use crate::sms::{codec::parse_sms_pdu, concat::ConcatReassembler, SmsMessage};

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
        return false;
    };

    let delete = process_pdu_hex(&hex, index, router, log, concat, messenger, store);
    if delete {
        let _ = modem.send_at(&format!("+CMGD={}", index));
    } else {
        log::warn!(
            "[sms_handler] forward failed — SMS stays at mem={} slot={}",
            mem,
            index
        );
    }
    delete
}

/// Sweep all stored SMS in one memory bank (AT+CMGL=4) at boot time.
pub fn sweep_one_storage(
    mem: &str,
    modem: &mut dyn ModemPort,
    router: &mut ReplyRouter,
    log: &mut LogRing,
    concat: &mut ConcatReassembler,
    messenger: &mut dyn MessageSink,
    store: &mut dyn Store,
) {
    let r = modem.send_at("+CMGL=4");
    let resp = match r {
        Ok(r) => r,
        Err(e) => {
            log::warn!("[sms_handler] sweep {} AT+CMGL=4 failed: {:?}", mem, e);
            return;
        }
    };
    if !resp.ok {
        log::warn!(
            "[sms_handler] sweep {} AT+CMGL=4 error: {}",
            mem,
            resp.body.trim()
        );
        return;
    }
    log::info!(
        "[sms_handler] sweep {} AT+CMGL=4 body: {:?}",
        mem,
        resp.body
    );

    let body = resp.body.clone();
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
                let delete = process_pdu_hex(hex, slot, router, log, concat, messenger, store);
                if delete {
                    let _ = modem.send_at(&format!("+CMGD={}", slot));
                } else {
                    log::warn!(
                        "[sms_handler] sweep forward failed — SMS stays at {} slot {}",
                        mem,
                        slot
                    );
                }
            }
        }
    }
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
