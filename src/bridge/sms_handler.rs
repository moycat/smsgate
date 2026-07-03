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
    decoded: Option<SmsMessage>,
}

impl StoredSms {
    fn pdu(mem: &str, index: u16, pdu_hex: String) -> Self {
        StoredSms {
            mem: mem.to_string(),
            index,
            pdu_hex,
            decoded: None,
        }
    }

    fn decoded(mem: &str, index: u16, sender: String, timestamp: String, body: String) -> Self {
        StoredSms {
            mem: mem.to_string(),
            index,
            pdu_hex: String::new(),
            decoded: Some(SmsMessage {
                sender,
                body,
                timestamp,
                slot: index,
            }),
        }
    }
}

/// Read one SMS PDU from the modem slot reported by a +CMTI notification.
pub fn read_new_sms_pdu(mem: &str, index: u16, modem: &mut dyn ModemPort) -> Option<StoredSms> {
    log::info!("[sms_handler] +CMTI: mem={} index={}", mem, index);

    ensure_pdu_mode(modem);
    let _ = modem.send_at(&format!("+CPMS=\"{}\"", mem));
    let r = modem.send_at(&format!("+CMGR={}", index));
    let stored = match &r {
        Ok(resp) if resp.ok => {
            log::info!("[sms_handler] AT+CMGR={} body: {:?}", index, resp.body);
            parse_cmgr_response(mem, index, &resp.body)
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

    let Some(stored) = stored else {
        log::warn!(
            "[sms_handler] could not read SMS at mem={} slot={}",
            mem,
            index
        );
        return None;
    };

    Some(stored)
}

/// Delete an SMS storage slot after the message has been consumed.
pub fn delete_sms_slot(index: u16, modem: &mut dyn ModemPort) {
    let _ = modem.send_at(&format!("+CMGD={}", index));
}

/// Read all stored SMS PDUs from one memory bank.
pub fn read_stored_sms(mem: &str, modem: &mut dyn ModemPort) -> Vec<StoredSms> {
    ensure_pdu_mode(modem);
    let Some((cmd, resp)) = list_stored_sms(mem, modem) else {
        return Vec::new();
    };
    log::info!(
        "[sms_handler] sweep {} AT{} body: {:?}",
        mem,
        cmd,
        resp.body
    );

    let mut stored = Vec::new();
    let mut current: Option<(ListHeader, Vec<String>)> = None;
    for line in resp.body.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(header) = parse_cmgl_header(line) {
            flush_list_entry(mem, &mut stored, current.take());
            current = Some((header, Vec::new()));
        } else if let Some((_, body_lines)) = current.as_mut() {
            body_lines.push(line.to_string());
        }
    }
    flush_list_entry(mem, &mut stored, current);
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

fn ensure_pdu_mode(modem: &mut dyn ModemPort) {
    match modem.send_at("+CMGF=0") {
        Ok(resp) if resp.ok => {}
        Ok(resp) => log::warn!("[sms_handler] AT+CMGF=0 error: {}", resp.body.trim()),
        Err(e) => log::warn!("[sms_handler] AT+CMGF=0 failed: {:?}", e),
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
///
/// This is the SMS ingestion composition point; dependencies stay explicit so
/// host tests can inject each side effect independently.
#[allow(clippy::too_many_arguments)]
pub fn process_pdu_hex(
    hex: &str,
    slot: u16,
    router: &mut ReplyRouter,
    log: &mut LogRing,
    concat: &mut ConcatReassembler,
    messenger: &mut dyn MessageSink,
    store: &mut dyn Store,
    log_timestamp: &str,
) -> bool {
    let pdu = match parse_sms_pdu(hex) {
        Ok(p) => p,
        Err(e) => {
            log::error!("[sms_handler] PDU parse error at slot {}: {}", slot, e);
            return true; // unparseable — no point keeping it
        }
    };

    let sms = if let Some(notification) = crate::mms::parse_mms_notification_from_sms(&pdu) {
        SmsMessage {
            sender: pdu.sender,
            body: crate::i18n::mms_notification(
                &notification.content_location,
                notification.message_size,
                notification.expiry,
            ),
            timestamp: pdu.timestamp,
            slot,
        }
    } else if pdu.is_concatenated {
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

    forward_sms(&sms, messenger, router, log, store, log_timestamp).is_some()
}

/// Forward an SMS storage entry, whether it came from PDU mode or modem text mode.
pub fn process_stored_sms(
    stored: StoredSms,
    router: &mut ReplyRouter,
    log: &mut LogRing,
    concat: &mut ConcatReassembler,
    messenger: &mut dyn MessageSink,
    store: &mut dyn Store,
    log_timestamp: &str,
) -> bool {
    if let Some(sms) = stored.decoded {
        return forward_sms(&sms, messenger, router, log, store, log_timestamp).is_some();
    }

    process_pdu_hex(
        &stored.pdu_hex,
        stored.index,
        router,
        log,
        concat,
        messenger,
        store,
        log_timestamp,
    )
}

enum ListHeader {
    Pdu {
        index: u16,
    },
    Text {
        index: u16,
        sender: String,
        timestamp: String,
    },
}

fn parse_cmgr_response(mem: &str, index: u16, body: &str) -> Option<StoredSms> {
    let mut lines = body.lines().map(str::trim).filter(|l| !l.is_empty());
    while let Some(line) = lines.next() {
        let rest = line.strip_prefix("+CMGR:")?.trim();
        if rest.starts_with('"') {
            let fields = parse_at_csv(rest);
            if fields.len() < 4 {
                return None;
            }
            let sender = decode_modem_text(&fields[1]);
            let timestamp = fields[3].trim().to_string();
            let text = lines.collect::<Vec<_>>().join("\n");
            return Some(StoredSms::decoded(
                mem,
                index,
                sender,
                timestamp,
                decode_modem_text(&text),
            ));
        }

        if let Some(hex) = lines.find(|l| !l.starts_with("+CMGR:")) {
            return Some(StoredSms::pdu(mem, index, hex.to_string()));
        }
    }
    None
}

fn parse_cmgl_header(line: &str) -> Option<ListHeader> {
    let rest = line.strip_prefix("+CMGL:")?.trim();
    let fields = parse_at_csv(rest);
    let index = fields.first()?.trim().parse().ok()?;

    if fields.len() >= 5 && !fields[1].chars().all(|c| c.is_ascii_digit()) {
        return Some(ListHeader::Text {
            index,
            sender: decode_modem_text(&fields[2]),
            timestamp: fields[4].trim().to_string(),
        });
    }

    Some(ListHeader::Pdu { index })
}

fn flush_list_entry(
    mem: &str,
    stored: &mut Vec<StoredSms>,
    entry: Option<(ListHeader, Vec<String>)>,
) {
    let Some((header, body_lines)) = entry else {
        return;
    };
    match header {
        ListHeader::Pdu { index } => {
            if let Some(hex) = body_lines.first() {
                log::info!("[sms_handler] sweep found SMS in {} slot {}", mem, index);
                stored.push(StoredSms::pdu(mem, index, hex.trim().to_string()));
            }
        }
        ListHeader::Text {
            index,
            sender,
            timestamp,
        } => {
            log::info!("[sms_handler] sweep found SMS in {} slot {}", mem, index);
            stored.push(StoredSms::decoded(
                mem,
                index,
                sender,
                timestamp,
                decode_modem_text(&body_lines.join("\n")),
            ));
        }
    }
}

fn parse_at_csv(input: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in input.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                fields.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    fields.push(current.trim().to_string());
    fields
}

fn decode_modem_text(text: &str) -> String {
    decode_ucs2_hex(text).unwrap_or_else(|| text.to_string())
}

fn decode_ucs2_hex(text: &str) -> Option<String> {
    let hex: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if hex.len() < 4 || !hex.len().is_multiple_of(4) || !hex.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return None;
    }

    let mut units = Vec::with_capacity(hex.len() / 4);
    for chunk in hex.as_bytes().chunks_exact(4) {
        let raw = std::str::from_utf8(chunk).ok()?;
        units.push(u16::from_str_radix(raw, 16).ok()?);
    }

    let plausible = units
        .iter()
        .filter(|&&unit| is_text_code_unit(unit))
        .count();
    if plausible * 2 < units.len() {
        return None;
    }

    let decoded = std::char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .ok()?;
    (!decoded.is_empty()).then_some(decoded)
}

fn is_text_code_unit(unit: u16) -> bool {
    matches!(
        unit,
        0x0009 | 0x000A | 0x000D
            | 0x0020..=0x007E
            | 0x00A0..=0x00FF
            | 0x2000..=0x206F
            | 0x3000..=0x303F
            | 0x3400..=0x9FFF
            | 0xFF00..=0xFFEF
    )
}
