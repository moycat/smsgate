//! SMS inbound processing: CMTI handling and boot-time sweep.
//!
//! Extracted from main.rs so it can be unit-tested against ScriptedModem.

use crate::bridge::{
    forwarder::{forward_sms, is_blocked},
    reply_router::ReplyRouter,
};
use crate::im::MessageSink;
use crate::log_ring::{LogEvent, LogKind, LogRing};
use crate::modem::{AtResponse, ModemError, ModemPort};
use crate::persist::{keys, load_bool, Store};
use crate::sms::{
    codec::parse_sms_pdu,
    concat::{ConcatReassembler, FeedOutcome, StorageSlot},
    SmsMessage,
};
use std::hash::{Hash, Hasher};
use std::time::Duration;

const MAX_SMS_INDEXES: usize = 1024;
const LIST_IDLE_TIMEOUT: Duration = Duration::from_secs(10);
const LIST_HARD_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, thiserror::Error)]
pub enum SmsReadError {
    #[error("invalid SMS storage name")]
    InvalidStorage,
    #[error("AT+CMGF=0 failed: {0}")]
    PduMode(String),
    #[error("AT+CPMS failed: {0}")]
    SelectStorage(String),
    #[error("AT+CMGR failed: {0}")]
    ReadSlot(String),
    #[error("AT+CMGR response did not contain a valid SMS")]
    MalformedSlot,
    #[error("AT+CMGL failed: {0}")]
    ListStorage(String),
    #[error("AT+CMGL response did not contain a valid SMS list")]
    MalformedList,
    #[error("AT+CMGL listed more SMS slots than the bounded index buffer")]
    ListTooLarge,
    #[error("stored SMS batch interrupted for incoming call")]
    InterruptedForCall,
    #[error("AT+CMGD failed: {0}")]
    DeleteSlot(String),
}

/// Raw SMS PDU read from a modem storage slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSms {
    pub mem: String,
    pub index: u16,
    pub pdu_hex: String,
    decoded: Option<SmsMessage>,
}

impl StoredSms {
    pub fn storage_slot(&self) -> StorageSlot {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.pdu_hex.hash(&mut hasher);
        if let Some(decoded) = &self.decoded {
            decoded.sender.hash(&mut hasher);
            decoded.timestamp.hash(&mut hasher);
            decoded.body.hash(&mut hasher);
        }
        StorageSlot {
            mem: self.mem.clone(),
            index: self.index,
            fingerprint: hasher.finish(),
        }
    }

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

/// Storage entries that may be removed only after their message was consumed.
#[derive(Debug, PartialEq, Eq)]
pub enum SmsDisposition {
    Retain,
    Delete(Vec<StorageSlot>),
}

#[derive(Debug, PartialEq, Eq)]
pub enum VerifiedDeletion {
    Deleted,
    SlotChanged,
}

/// A decoded SMS whose delivery result has not yet been confirmed.
#[derive(Debug)]
pub struct PreparedSms {
    pub sms: SmsMessage,
    pub slots: Vec<StorageSlot>,
}

/// Parsing and reassembly outcome, independent of IM delivery.
#[derive(Debug)]
pub enum SmsPreparation {
    Retain,
    Consumed(Vec<StorageSlot>),
    Ready(PreparedSms),
}

/// A storage sweep can recover valid entries while reporting malformed ones.
pub struct StoredSmsScan {
    pub messages: Vec<StoredSms>,
    pub malformed_entries: usize,
}

/// A complete storage listing retains only slot numbers, not every PDU body.
#[derive(Debug)]
pub struct StoredSmsIndexScan {
    pub indices: Vec<u16>,
    pub malformed_entries: usize,
}

/// A bounded batch of individually read storage slots.
#[derive(Debug)]
pub struct StoredSmsBatch {
    pub entries: Vec<(u16, Result<StoredSms, SmsReadError>)>,
}

/// Stream a full PDU-mode listing while keeping at most 1024 slot numbers.
/// A command timeout or an over-capacity listing is never reported as an
/// empty, successful scan.
pub fn scan_stored_sms_indices(
    mem: &str,
    modem: &mut dyn ModemPort,
) -> Result<StoredSmsIndexScan, SmsReadError> {
    ensure_pdu_mode(modem)?;
    select_storage(mem, modem)?;
    match stream_sms_index_list(modem, "+CMGL=4") {
        Err(SmsReadError::ListStorage(detail))
            if detail.contains("Invalid text mode parameter") =>
        {
            stream_sms_index_list(modem, "+CMGL=\"ALL\"")
        }
        result => result,
    }
}

fn stream_sms_index_list(
    modem: &mut dyn ModemPort,
    command: &str,
) -> Result<StoredSmsIndexScan, SmsReadError> {
    let mut indices = Vec::new();
    let mut malformed_entries = 0usize;
    let mut too_many = false;
    modem
        .send_at_streaming(command, LIST_IDLE_TIMEOUT, LIST_HARD_TIMEOUT, &mut |line| {
            let Some(rest) = line.trim().strip_prefix("+CMGL:") else {
                return;
            };
            let index = rest
                .split(',')
                .next()
                .and_then(|field| field.trim().parse::<u16>().ok());
            match index {
                Some(index) if indices.len() < MAX_SMS_INDEXES => indices.push(index),
                Some(_) => too_many = true,
                None => malformed_entries += 1,
            }
        })
        .map_err(|error| SmsReadError::ListStorage(format!("AT{command}: {error}")))?;
    if too_many {
        return Err(SmsReadError::ListTooLarge);
    }
    indices.sort_unstable();
    indices.dedup();
    Ok(StoredSmsIndexScan {
        indices,
        malformed_entries,
    })
}

/// Select PDU mode and one storage bank once, then read a small slot batch.
pub fn read_stored_sms_batch(
    mem: &str,
    indices: &[u16],
    modem: &mut dyn ModemPort,
) -> Result<StoredSmsBatch, SmsReadError> {
    ensure_pdu_mode(modem)?;
    select_storage(mem, modem)?;
    let mut entries = Vec::with_capacity(indices.len());
    for &index in indices {
        let response = modem.send_at(&format!("+CMGR={}", index));
        if matches!(response, Err(ModemError::InterruptedForCall)) {
            return Err(SmsReadError::InterruptedForCall);
        }
        let entry = checked_response(response)
            .map_err(SmsReadError::ReadSlot)
            .and_then(|response| {
                parse_cmgr_response(mem, index, &response.body).ok_or(SmsReadError::MalformedSlot)
            });
        entries.push((index, entry));
    }
    Ok(StoredSmsBatch { entries })
}

/// Delete a small same-bank batch after delivery, selecting storage once.
/// Individual CMGD failures stay visible to the caller.
pub fn delete_sms_slots(
    mem: &str,
    indices: &[u16],
    modem: &mut dyn ModemPort,
) -> Result<Vec<Result<(), SmsReadError>>, SmsReadError> {
    select_storage(mem, modem)?;
    Ok(indices
        .iter()
        .map(|index| {
            checked_at(modem, &format!("+CMGD={}", index))
                .map(|_| ())
                .map_err(SmsReadError::DeleteSlot)
        })
        .collect())
}

/// Read one SMS PDU from the modem slot reported by a +CMTI notification.
pub fn read_new_sms_pdu(
    mem: &str,
    index: u16,
    modem: &mut dyn ModemPort,
) -> Result<StoredSms, SmsReadError> {
    log::info!("[sms_handler] +CMTI: mem={} index={}", mem, index);

    ensure_pdu_mode(modem)?;
    select_storage(mem, modem)?;
    let resp = checked_at(modem, &format!("+CMGR={}", index)).map_err(SmsReadError::ReadSlot)?;
    log::info!("[sms_handler] AT+CMGR={} body: {:?}", index, resp.body);
    parse_cmgr_response(mem, index, &resp.body).ok_or(SmsReadError::MalformedSlot)
}

/// Delete an SMS storage slot after the message has been consumed.
pub fn delete_sms_slot(
    mem: &str,
    index: u16,
    modem: &mut dyn ModemPort,
) -> Result<(), SmsReadError> {
    select_storage(mem, modem)?;
    checked_at(modem, &format!("+CMGD={}", index)).map_err(SmsReadError::DeleteSlot)?;
    Ok(())
}

/// Delete only the exact message previously delivered to IM. A modem slot can
/// be reused while Telegram is slow, so its index alone is not sufficient.
pub fn delete_verified_sms_slot(
    expected: &StorageSlot,
    modem: &mut dyn ModemPort,
) -> Result<VerifiedDeletion, SmsReadError> {
    let current = read_new_sms_pdu(&expected.mem, expected.index, modem)?;
    if current.storage_slot() != *expected {
        return Ok(VerifiedDeletion::SlotChanged);
    }
    checked_at(modem, &format!("+CMGD={}", expected.index)).map_err(SmsReadError::DeleteSlot)?;
    Ok(VerifiedDeletion::Deleted)
}

/// Read all stored SMS PDUs from one memory bank.
pub fn read_stored_sms(
    mem: &str,
    modem: &mut dyn ModemPort,
) -> Result<Vec<StoredSms>, SmsReadError> {
    Ok(read_stored_sms_with_report(mem, modem)?.messages)
}

/// Read storage and report entries that could not be parsed without discarding
/// valid entries from the same modem response.
pub fn read_stored_sms_with_report(
    mem: &str,
    modem: &mut dyn ModemPort,
) -> Result<StoredSmsScan, SmsReadError> {
    ensure_pdu_mode(modem)?;
    select_storage(mem, modem)?;
    let (cmd, resp) = list_stored_sms(mem, modem)?;
    log::info!(
        "[sms_handler] sweep {} AT{} body: {:?}",
        mem,
        cmd,
        resp.body
    );

    let mut stored = Vec::new();
    let mut current: Option<PendingListEntry> = None;
    let mut header_count = 0;
    let mut malformed_entries = 0;
    for line in resp.body.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if line.starts_with("+CMGL:") {
            if !flush_list_entry(mem, &mut stored, current.take()) {
                malformed_entries += 1;
            }
            if let Some(header) = parse_cmgl_header(line) {
                header_count += 1;
                current = Some(PendingListEntry::new(header));
            } else {
                malformed_entries += 1;
            }
        } else if let Some(entry) = current.as_mut() {
            entry.push_body_line(line);
        }
    }
    if !flush_list_entry(mem, &mut stored, current) {
        malformed_entries += 1;
    }
    if !resp.body.trim().is_empty() && (header_count == 0 || stored.is_empty()) {
        return Err(SmsReadError::MalformedList);
    }
    Ok(StoredSmsScan {
        messages: stored,
        malformed_entries,
    })
}

fn list_stored_sms(
    mem: &str,
    modem: &mut dyn ModemPort,
) -> Result<(&'static str, AtResponse), SmsReadError> {
    const PDU_LIST: &str = "+CMGL=4";
    const TEXT_LIST: &str = "+CMGL=\"ALL\"";

    match checked_at(modem, PDU_LIST) {
        Ok(response) => Ok((PDU_LIST, response)),
        Err(error) if error.contains("Invalid text mode parameter") => {
            log::warn!(
                "[sms_handler] sweep {} AT{} failed: {}",
                mem,
                PDU_LIST,
                error
            );
            checked_at(modem, TEXT_LIST)
                .map(|response| (TEXT_LIST, response))
                .map_err(|fallback| {
                    SmsReadError::ListStorage(format!(
                        "AT{}: {}; AT{}: {}",
                        PDU_LIST, error, TEXT_LIST, fallback
                    ))
                })
        }
        Err(error) => Err(SmsReadError::ListStorage(format!(
            "AT{}: {}",
            PDU_LIST, error
        ))),
    }
}

fn ensure_pdu_mode(modem: &mut dyn ModemPort) -> Result<(), SmsReadError> {
    checked_at(modem, "+CMGF=0").map_err(SmsReadError::PduMode)?;
    Ok(())
}

fn select_storage(mem: &str, modem: &mut dyn ModemPort) -> Result<(), SmsReadError> {
    if mem.is_empty() || mem.len() > 4 || !mem.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return Err(SmsReadError::InvalidStorage);
    }
    checked_at(modem, &format!("+CPMS=\"{}\"", mem)).map_err(SmsReadError::SelectStorage)?;
    Ok(())
}

fn checked_at(modem: &mut dyn ModemPort, command: &str) -> Result<AtResponse, String> {
    checked_response(modem.send_at(command))
}

fn checked_response(response: Result<AtResponse, ModemError>) -> Result<AtResponse, String> {
    match response {
        Ok(response) if response.ok => Ok(response),
        Ok(response) => Err(safe_at_error(&response.body)),
        Err(ModemError::AtError(detail)) => Err(safe_at_error(&detail)),
        Err(error) => Err(error.to_string()),
    }
}

fn safe_at_error(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|line| {
            *line == "ERROR" || line.starts_with("+CME ERROR:") || line.starts_with("+CMS ERROR:")
        })
        .map(|line| line.chars().take(80).collect())
        .unwrap_or_else(|| "modem rejected command".to_string())
}

/// Parse a PDU hex string and forward the SMS.
///
/// Returns `true` if the direct delivery was consumed:
/// - single SMS forwarded successfully
/// - concat partial (held in RAM pending the other parts)
/// - intentionally ignored SMS (blocked sender or forwarding paused)
///
/// Stored deliveries use `process_stored_sms_with_slots` so incomplete parts
/// remain on the modem until the full message has been forwarded.
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
    matches!(
        process_pdu_hex_with_slot(
            hex,
            slot,
            None,
            router,
            log,
            concat,
            messenger,
            store,
            log_timestamp,
        ),
        SmsDisposition::Delete(_)
    )
}

#[allow(clippy::too_many_arguments)]
pub fn process_pdu_hex_with_slot(
    hex: &str,
    slot: u16,
    storage_slot: Option<StorageSlot>,
    router: &mut ReplyRouter,
    log: &mut LogRing,
    concat: &mut ConcatReassembler,
    messenger: &mut dyn MessageSink,
    store: &mut dyn Store,
    log_timestamp: &str,
) -> SmsDisposition {
    let preparation =
        prepare_pdu_hex_with_slot(hex, slot, storage_slot, log, concat, log_timestamp);
    finish_preparation(preparation, messenger, router, log, store, log_timestamp)
}

/// Decode one PDU without waiting for its IM delivery.
pub fn prepare_pdu_hex_with_slot(
    hex: &str,
    slot: u16,
    storage_slot: Option<StorageSlot>,
    log: &mut LogRing,
    concat: &mut ConcatReassembler,
    log_timestamp: &str,
) -> SmsPreparation {
    let pdu = match parse_sms_pdu(hex) {
        Ok(p) => p,
        Err(e) => {
            log::error!("[sms_handler] PDU parse error at slot {}: {}", slot, e);
            log.push(
                LogEvent::new(
                    LogKind::Sms,
                    "modem",
                    &format!("PDU parse failed at slot {}: {}", slot, e),
                    false,
                )
                .at(log_timestamp),
            );
            return SmsPreparation::Retain;
        }
    };

    let is_stored = storage_slot.is_some();
    let own_slots: Vec<StorageSlot> = storage_slot.iter().cloned().collect();
    let (sms, slots) = if let Some(notification) = crate::mms::parse_mms_notification_from_sms(&pdu)
    {
        (
            SmsMessage {
                sender: pdu.sender,
                body: crate::i18n::mms_notification(
                    &notification.content_location,
                    notification.message_size,
                    notification.expiry,
                ),
                timestamp: pdu.timestamp,
                slot,
            },
            own_slots,
        )
    } else if pdu.is_concatenated {
        match concat.feed_with_slot(&pdu, storage_slot) {
            FeedOutcome::Complete(complete) => (
                SmsMessage {
                    sender: complete.sender,
                    body: complete.content,
                    timestamp: complete.timestamp,
                    slot,
                },
                complete.slots,
            ),
            FeedOutcome::Incomplete => {
                return if is_stored {
                    SmsPreparation::Retain
                } else {
                    SmsPreparation::Consumed(Vec::new())
                };
            }
            FeedOutcome::Invalid => {
                log.push(
                    LogEvent::new(
                        LogKind::Sms,
                        "modem",
                        &format!("invalid concatenated SMS at slot {}", slot),
                        false,
                    )
                    .at(log_timestamp),
                );
                return SmsPreparation::Retain;
            }
        }
    } else {
        (
            SmsMessage {
                sender: pdu.sender,
                body: pdu.content,
                timestamp: pdu.timestamp,
                slot,
            },
            own_slots,
        )
    };

    SmsPreparation::Ready(PreparedSms { sms, slots })
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
    matches!(
        process_stored_sms_with_slots(stored, router, log, concat, messenger, store, log_timestamp,),
        SmsDisposition::Delete(_)
    )
}

#[allow(clippy::too_many_arguments)]
pub fn process_stored_sms_with_slots(
    stored: StoredSms,
    router: &mut ReplyRouter,
    log: &mut LogRing,
    concat: &mut ConcatReassembler,
    messenger: &mut dyn MessageSink,
    store: &mut dyn Store,
    log_timestamp: &str,
) -> SmsDisposition {
    let preparation = prepare_stored_sms(stored, log, concat, log_timestamp);
    finish_preparation(preparation, messenger, router, log, store, log_timestamp)
}

/// Decode one stored SMS without waiting for its IM delivery.
pub fn prepare_stored_sms(
    stored: StoredSms,
    log: &mut LogRing,
    concat: &mut ConcatReassembler,
    log_timestamp: &str,
) -> SmsPreparation {
    let storage_slot = stored.storage_slot();
    if let Some(sms) = stored.decoded {
        return SmsPreparation::Ready(PreparedSms {
            sms,
            slots: vec![storage_slot],
        });
    }

    prepare_pdu_hex_with_slot(
        &stored.pdu_hex,
        stored.index,
        Some(storage_slot),
        log,
        concat,
        log_timestamp,
    )
}

fn finish_preparation(
    preparation: SmsPreparation,
    messenger: &mut dyn MessageSink,
    router: &mut ReplyRouter,
    log: &mut LogRing,
    store: &mut dyn Store,
    log_timestamp: &str,
) -> SmsDisposition {
    match preparation {
        SmsPreparation::Retain => SmsDisposition::Retain,
        SmsPreparation::Consumed(slots) => SmsDisposition::Delete(slots),
        SmsPreparation::Ready(PreparedSms { sms, slots }) => {
            if forward_or_consume(&sms, messenger, router, log, store, log_timestamp) {
                SmsDisposition::Delete(slots)
            } else {
                SmsDisposition::Retain
            }
        }
    }
}

fn forward_or_consume(
    sms: &SmsMessage,
    messenger: &mut dyn MessageSink,
    router: &mut ReplyRouter,
    log: &mut LogRing,
    store: &mut dyn Store,
    log_timestamp: &str,
) -> bool {
    let intentionally_dropped =
        load_bool(store, keys::FWD_ENABLED) == Some(false) || is_blocked(&sms.sender, store);
    forward_sms(sms, messenger, router, log, store, log_timestamp).is_some()
        || intentionally_dropped
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
        let Some(rest) = line.strip_prefix("+CMGR:") else {
            continue;
        };
        let rest = rest.trim();
        if rest.starts_with('"') {
            let mut fields = AtCsvFields::new(rest);
            let _status = fields.next()?;
            let sender = decode_modem_text(fields.next()?);
            let _alpha = fields.next()?;
            let timestamp = fields.next()?.to_string();
            let text = collect_remaining_sms_body(&mut lines);
            return Some(StoredSms::decoded(
                mem,
                index,
                sender,
                timestamp,
                decode_modem_text(&text),
            ));
        }

        if let Some(hex) = lines.find(|line| is_pdu_hex_line(line)) {
            return Some(StoredSms::pdu(mem, index, hex.to_string()));
        }
    }
    None
}

fn parse_cmgl_header(line: &str) -> Option<ListHeader> {
    let rest = line.strip_prefix("+CMGL:")?.trim();
    let mut fields = AtCsvFields::new(rest);
    let index = fields.next()?.parse().ok()?;
    let status = fields.next()?;

    if !status.chars().all(|c| c.is_ascii_digit()) {
        let sender = fields.next()?;
        let _alpha = fields.next()?;
        let timestamp = fields.next()?;
        return Some(ListHeader::Text {
            index,
            sender: decode_modem_text(sender),
            timestamp: timestamp.to_string(),
        });
    }

    Some(ListHeader::Pdu { index })
}

struct PendingListEntry {
    header: ListHeader,
    body: String,
}

impl PendingListEntry {
    fn new(header: ListHeader) -> Self {
        Self {
            header,
            body: String::new(),
        }
    }

    fn push_body_line(&mut self, line: &str) {
        if matches!(&self.header, ListHeader::Pdu { .. }) && !is_pdu_hex_line(line) {
            return;
        }
        if matches!(&self.header, ListHeader::Pdu { .. }) && !self.body.is_empty() {
            return;
        }
        if !self.body.is_empty() {
            self.body.push('\n');
        }
        self.body.push_str(line);
    }
}

fn is_pdu_hex_line(line: &str) -> bool {
    let mut count = 0usize;
    for byte in line.bytes() {
        if byte.is_ascii_whitespace() {
            continue;
        }
        if !byte.is_ascii_hexdigit() {
            return false;
        }
        count += 1;
    }
    count > 0 && count.is_multiple_of(2)
}

fn flush_list_entry(
    mem: &str,
    stored: &mut Vec<StoredSms>,
    entry: Option<PendingListEntry>,
) -> bool {
    let Some(PendingListEntry { header, body }) = entry else {
        return true;
    };
    match header {
        ListHeader::Pdu { index } => {
            if !body.is_empty() {
                log::info!("[sms_handler] sweep found SMS in {} slot {}", mem, index);
                stored.push(StoredSms::pdu(mem, index, body));
                true
            } else {
                false
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
                decode_modem_text(&body),
            ));
            true
        }
    }
}

fn collect_remaining_sms_body<'a>(lines: &mut impl Iterator<Item = &'a str>) -> String {
    let mut body = String::new();
    for line in lines {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(line);
    }
    body
}

struct AtCsvFields<'a> {
    input: &'a str,
    finished: bool,
}

impl<'a> AtCsvFields<'a> {
    fn new(input: &'a str) -> Self {
        Self {
            input,
            finished: false,
        }
    }
}

impl<'a> Iterator for AtCsvFields<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }

        let mut in_quotes = false;
        for (idx, ch) in self.input.char_indices() {
            match ch {
                '"' => in_quotes = !in_quotes,
                ',' if !in_quotes => {
                    let field = trim_at_csv_field(&self.input[..idx]);
                    self.input = &self.input[idx + 1..];
                    return Some(field);
                }
                _ => {}
            }
        }

        self.finished = true;
        Some(trim_at_csv_field(self.input))
    }
}

fn trim_at_csv_field(field: &str) -> &str {
    let field = field.trim();
    field
        .strip_prefix('"')
        .and_then(|field| field.strip_suffix('"'))
        .unwrap_or(field)
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
    for chunk in hex.as_bytes().as_chunks::<4>().0 {
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
