//! Fixed-size event history ring buffer with optional flash persistence.

use std::cell::RefCell;
use thiserror::Error;

pub const FLASH_LOG_RECORD_SIZE: usize = 256;
pub const LOG_PAGE_SIZE: usize = 16;
const DEFAULT_VOLATILE_LOG_BYTES: usize = FLASH_LOG_RECORD_SIZE * 64;
const DEFAULT_VOLATILE_ERASE_BYTES: usize = 4096;
const FLASH_LOG_MAGIC: u32 = 0x534D_4C47; // SMLG
const FLASH_LOG_HEADER_SIZE: usize = 16;
const FLASH_LOG_PAYLOAD_SIZE: usize = FLASH_LOG_RECORD_SIZE - FLASH_LOG_HEADER_SIZE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogKind {
    Sms,
    Call,
    System,
    Network,
    User,
    Ota,
}

impl LogKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sms => "sms",
            Self::Call => "call",
            Self::System => "system",
            Self::Network => "network",
            Self::User => "user",
            Self::Ota => "ota",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Sms => "SMS",
            Self::Call => "CALL",
            Self::System => "SYS",
            Self::Network => "NET",
            Self::User => "USER",
            Self::Ota => "OTA",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "sms" => Some(Self::Sms),
            "call" => Some(Self::Call),
            "system" => Some(Self::System),
            "network" => Some(Self::Network),
            "user" => Some(Self::User),
            "ota" => Some(Self::Ota),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub kind: LogKind,
    pub sender: String,
    pub body_preview: String, // first 80 chars
    pub timestamp: String,
    pub forwarded: bool,
}

impl LogEntry {
    pub fn sms(sender: String, body_preview: String, timestamp: String, forwarded: bool) -> Self {
        Self {
            kind: LogKind::Sms,
            sender,
            body_preview,
            timestamp,
            forwarded,
        }
    }

    pub fn status_marker(&self) -> &'static str {
        if self.forwarded {
            "OK"
        } else {
            "FAIL"
        }
    }

    fn encode_payload(&self) -> Result<Vec<u8>, FlashLogError> {
        let kind = self.kind.as_str();
        let forwarded = if self.forwarded { "1" } else { "0" };
        let timestamp_len = escaped_field_len(&self.timestamp);
        let base_len = 1 + 1 + kind.len() + 1 + forwarded.len() + 1 + timestamp_len + 1 + 1;
        if base_len > FLASH_LOG_PAYLOAD_SIZE {
            return Err(FlashLogError::EntryTooLarge {
                len: base_len,
                max: FLASH_LOG_PAYLOAD_SIZE,
            });
        }

        let available = FLASH_LOG_PAYLOAD_SIZE - base_len;
        let sender_len = escaped_field_len(&self.sender);
        let (sender_budget, body_budget) = if sender_len <= available {
            (sender_len, available - sender_len)
        } else {
            (available, 0)
        };

        let mut payload = String::with_capacity(FLASH_LOG_PAYLOAD_SIZE.min(
            base_len + sender_budget + escaped_field_len(&self.body_preview).min(body_budget),
        ));
        payload.push('1');
        payload.push('\t');
        payload.push_str(kind);
        payload.push('\t');
        payload.push_str(forwarded);
        payload.push('\t');
        push_escaped_field(&mut payload, &self.timestamp);
        payload.push('\t');
        push_escaped_field_truncated(&mut payload, &self.sender, sender_budget);
        payload.push('\t');
        push_escaped_field_truncated(&mut payload, &self.body_preview, body_budget);
        Ok(payload.into_bytes())
    }

    fn decode_payload(bytes: &[u8]) -> Option<Self> {
        let payload = core::str::from_utf8(bytes).ok()?;
        let mut parts = payload.split('\t');
        if parts.next()? != "1" {
            return None;
        }
        let kind = parts.next()?;
        let forwarded = parts.next()?;
        let timestamp = parts.next()?;
        let sender = parts.next()?;
        let body_preview = parts.next()?;
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            kind: LogKind::parse(kind)?,
            forwarded: forwarded == "1",
            timestamp: unescape_field(timestamp),
            sender: unescape_field(sender),
            body_preview: unescape_field(body_preview),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEvent {
    pub kind: LogKind,
    pub sender: String,
    pub body_preview: String,
    pub forwarded: bool,
}

impl LogEvent {
    pub fn new(kind: LogKind, subject: &str, detail: &str, ok: bool) -> Self {
        Self {
            kind,
            sender: subject.to_string(),
            body_preview: detail.to_string(),
            forwarded: ok,
        }
    }

    pub fn system(subject: &str, detail: &str) -> Self {
        Self::new(LogKind::System, subject, detail, true)
    }

    pub fn network(subject: &str, detail: &str, ok: bool) -> Self {
        Self::new(LogKind::Network, subject, detail, ok)
    }

    pub fn user(subject: &str, detail: &str, ok: bool) -> Self {
        Self::new(LogKind::User, subject, detail, ok)
    }

    pub fn ota(subject: &str, detail: &str, ok: bool) -> Self {
        Self::new(LogKind::Ota, subject, detail, ok)
    }

    pub fn at(self, timestamp: &str) -> LogEntry {
        LogEntry {
            kind: self.kind,
            sender: self.sender,
            body_preview: self.body_preview,
            timestamp: timestamp.to_string(),
            forwarded: self.forwarded,
        }
    }
}

/// Event log backed by flash-like storage.
pub struct LogRing {
    flash: RefCell<FlashLogRing<Box<dyn LogFlashStorage>>>,
}

impl LogRing {
    pub fn new() -> Self {
        Self::with_flash(Box::new(MemFlashLogStorage::new(
            DEFAULT_VOLATILE_LOG_BYTES,
            DEFAULT_VOLATILE_ERASE_BYTES,
        )))
        .expect("default volatile log storage geometry is valid")
    }

    pub fn with_flash(storage: Box<dyn LogFlashStorage>) -> Result<Self, FlashLogError> {
        Ok(Self {
            flash: RefCell::new(FlashLogRing::mount(storage)?),
        })
    }

    /// Push a new entry, evicting the oldest sector if the storage wraps.
    pub fn push(&mut self, entry: LogEntry) {
        if let Err(e) = self.flash.borrow_mut().append(&entry) {
            log::warn!("[log] flash append failed: {}", e);
        }
    }

    /// Return the last `n` entries (most-recent last).
    pub fn last_n(&self, n: usize) -> Vec<LogEntry> {
        self.flash.borrow_mut().last_n(n).unwrap_or_else(|e| {
            log::warn!("[log] flash read failed: {}", e);
            Vec::new()
        })
    }

    /// Return the newest entry of one kind without materializing the full log.
    pub fn latest_of_kind(&self, kind: LogKind) -> Option<LogEntry> {
        self.flash
            .borrow_mut()
            .latest_of_kind(kind)
            .unwrap_or_else(|e| {
                log::warn!("[log] flash read failed: {}", e);
                None
            })
    }

    /// Return a page of entries, where `offset` skips newest entries first.
    /// Page entries are ordered newest-to-oldest for display.
    pub fn page(&self, offset: usize, limit: usize) -> Result<Vec<LogEntry>, FlashLogError> {
        self.flash.borrow_mut().page(offset, limit)
    }

    pub fn len(&self) -> usize {
        self.flash.borrow_mut().entry_count().unwrap_or_else(|e| {
            log::warn!("[log] flash count failed: {}", e);
            0
        })
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for LogRing {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Error)]
pub enum FlashLogError {
    #[error("flash log storage out of bounds")]
    OutOfBounds,
    #[error("invalid flash log geometry: size={size}, erase_size={erase_size}")]
    InvalidGeometry { size: usize, erase_size: usize },
    #[error("flash log entry too large: {len} bytes (max {max})")]
    EntryTooLarge { len: usize, max: usize },
    #[error("flash log partition not found: {0}")]
    PartitionNotFound(String),
    #[error("flash log storage error: {0}")]
    Storage(String),
}

pub trait LogFlashStorage {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn erase_size(&self) -> usize;
    fn read(&mut self, offset: usize, buf: &mut [u8]) -> Result<(), FlashLogError>;
    fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), FlashLogError>;
    fn erase(&mut self, offset: usize, len: usize) -> Result<(), FlashLogError>;
}

impl<T: LogFlashStorage + ?Sized> LogFlashStorage for Box<T> {
    fn len(&self) -> usize {
        (**self).len()
    }

    fn erase_size(&self) -> usize {
        (**self).erase_size()
    }

    fn read(&mut self, offset: usize, buf: &mut [u8]) -> Result<(), FlashLogError> {
        (**self).read(offset, buf)
    }

    fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), FlashLogError> {
        (**self).write(offset, data)
    }

    fn erase(&mut self, offset: usize, len: usize) -> Result<(), FlashLogError> {
        (**self).erase(offset, len)
    }
}

#[derive(Debug)]
struct DecodedRecord {
    seq: u32,
    entry: LogEntry,
}

#[derive(Debug, Clone, Copy)]
struct RecordHeader {
    seq: u32,
    slot: usize,
}

#[derive(Debug, Clone, Copy)]
struct RecordSummary {
    seq: u32,
    slot: usize,
    kind: LogKind,
}

pub struct FlashLogRing<S: LogFlashStorage> {
    storage: S,
    next_seq: u32,
    next_slot: usize,
    slots: usize,
    slots_per_erase: usize,
}

impl<S: LogFlashStorage> FlashLogRing<S> {
    pub fn mount(mut storage: S) -> Result<Self, FlashLogError> {
        let size = storage.len();
        let erase_size = storage.erase_size();
        if size < FLASH_LOG_RECORD_SIZE
            || !size.is_multiple_of(FLASH_LOG_RECORD_SIZE)
            || erase_size < FLASH_LOG_RECORD_SIZE
            || !erase_size.is_multiple_of(FLASH_LOG_RECORD_SIZE)
            || !size.is_multiple_of(erase_size)
        {
            return Err(FlashLogError::InvalidGeometry { size, erase_size });
        }

        let slots = size / FLASH_LOG_RECORD_SIZE;
        let slots_per_erase = erase_size / FLASH_LOG_RECORD_SIZE;
        let headers = scan_record_headers(&mut storage, slots)?;
        let (next_seq, next_slot) = headers
            .iter()
            .max_by_key(|header| header.seq)
            .map(|header| {
                let next_seq = if header.seq == u32::MAX {
                    1
                } else {
                    header.seq + 1
                };
                (next_seq, (header.slot + 1) % slots)
            })
            .unwrap_or((1, 0));

        Ok(Self {
            storage,
            next_seq,
            next_slot,
            slots,
            slots_per_erase,
        })
    }

    pub fn append(&mut self, entry: &LogEntry) -> Result<(), FlashLogError> {
        let payload = entry.encode_payload()?;
        if self.next_slot.is_multiple_of(self.slots_per_erase) {
            self.storage.erase(
                self.next_slot * FLASH_LOG_RECORD_SIZE,
                self.storage.erase_size(),
            )?;
        }

        let mut record = [0xFFu8; FLASH_LOG_RECORD_SIZE];
        record[0..4].copy_from_slice(&FLASH_LOG_MAGIC.to_le_bytes());
        record[4..8].copy_from_slice(&self.next_seq.to_le_bytes());
        record[8..10].copy_from_slice(&(payload.len() as u16).to_le_bytes());
        record[10..14].copy_from_slice(&checksum(&payload).to_le_bytes());
        record[FLASH_LOG_HEADER_SIZE..FLASH_LOG_HEADER_SIZE + payload.len()]
            .copy_from_slice(&payload);

        self.storage
            .write(self.next_slot * FLASH_LOG_RECORD_SIZE, &record)?;

        self.next_seq = if self.next_seq == u32::MAX {
            1
        } else {
            self.next_seq + 1
        };
        self.next_slot = (self.next_slot + 1) % self.slots;
        Ok(())
    }

    pub fn entries(&mut self) -> Result<Vec<LogEntry>, FlashLogError> {
        let mut records = scan_records(&mut self.storage, self.slots)?;
        records.sort_unstable_by_key(|record| record.seq);
        Ok(records.into_iter().map(|record| record.entry).collect())
    }

    pub fn last_n(&mut self, n: usize) -> Result<Vec<LogEntry>, FlashLogError> {
        if n == 0 {
            return Ok(Vec::new());
        }
        let headers = newest_record_headers(&mut self.storage, self.slots, n)?;
        let mut entries = Vec::with_capacity(headers.len());
        for header in &headers {
            if let Some(record) = read_record(&mut self.storage, header.slot)? {
                entries.push(record.entry);
            }
        }
        Ok(entries)
    }

    pub fn latest_of_kind(&mut self, kind: LogKind) -> Result<Option<LogEntry>, FlashLogError> {
        let mut latest: Option<RecordSummary> = None;
        for slot in 0..self.slots {
            let Some(summary) = read_record_summary(&mut self.storage, slot)? else {
                continue;
            };
            if summary.kind != kind {
                continue;
            }
            let replace = match latest.as_ref() {
                Some(current) => summary.seq > current.seq,
                None => true,
            };
            if replace {
                latest = Some(summary);
            }
        }
        let Some(summary) = latest else {
            return Ok(None);
        };
        Ok(read_record(&mut self.storage, summary.slot)?.map(|record| record.entry))
    }

    pub fn page(&mut self, offset: usize, limit: usize) -> Result<Vec<LogEntry>, FlashLogError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let keep = offset.saturating_add(limit);
        let headers = newest_record_headers(&mut self.storage, self.slots, keep)?;
        let mut page = Vec::with_capacity(limit.min(headers.len().saturating_sub(offset)));
        for header in headers.iter().rev().skip(offset).take(limit) {
            if let Some(record) = read_record(&mut self.storage, header.slot)? {
                page.push(record.entry);
            }
        }
        Ok(page)
    }

    pub fn entry_count(&mut self) -> Result<usize, FlashLogError> {
        let mut count = 0;
        for slot in 0..self.slots {
            if read_record_header(&mut self.storage, slot)?.is_some() {
                count += 1;
            }
        }
        Ok(count)
    }

    pub fn into_storage(self) -> S {
        self.storage
    }
}

fn scan_records<S: LogFlashStorage>(
    storage: &mut S,
    slots: usize,
) -> Result<Vec<DecodedRecord>, FlashLogError> {
    let mut records = Vec::new();
    for slot in 0..slots {
        if let Some(record) = read_record(storage, slot)? {
            records.push(record);
        }
    }
    Ok(records)
}

fn scan_record_headers<S: LogFlashStorage>(
    storage: &mut S,
    slots: usize,
) -> Result<Vec<RecordHeader>, FlashLogError> {
    let mut headers = Vec::new();
    for slot in 0..slots {
        if let Some(header) = read_record_header(storage, slot)? {
            headers.push(header);
        }
    }
    Ok(headers)
}

fn newest_record_headers<S: LogFlashStorage>(
    storage: &mut S,
    slots: usize,
    limit: usize,
) -> Result<Vec<RecordHeader>, FlashLogError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut headers = Vec::with_capacity(limit.min(slots));
    for slot in 0..slots {
        let Some(header) = read_record_header(storage, slot)? else {
            continue;
        };
        insert_newest_header(&mut headers, header, limit);
    }
    Ok(headers)
}

fn insert_newest_header(headers: &mut Vec<RecordHeader>, header: RecordHeader, limit: usize) {
    let insert_at = headers
        .binary_search_by_key(&header.seq, |existing| existing.seq)
        .unwrap_or_else(|index| index);
    if headers.len() < limit {
        headers.insert(insert_at, header);
    } else if insert_at > 0 {
        headers.remove(0);
        headers.insert(insert_at - 1, header);
    }
}

fn read_record_header<S: LogFlashStorage>(
    storage: &mut S,
    slot: usize,
) -> Result<Option<RecordHeader>, FlashLogError> {
    let mut record = [0u8; FLASH_LOG_RECORD_SIZE];
    Ok(read_record_meta(storage, slot, &mut record)?.map(|(seq, _len)| RecordHeader { seq, slot }))
}

fn read_record_summary<S: LogFlashStorage>(
    storage: &mut S,
    slot: usize,
) -> Result<Option<RecordSummary>, FlashLogError> {
    let mut record = [0u8; FLASH_LOG_RECORD_SIZE];
    let Some((seq, len)) = read_record_meta(storage, slot, &mut record)? else {
        return Ok(None);
    };
    let payload = &record[FLASH_LOG_HEADER_SIZE..FLASH_LOG_HEADER_SIZE + len];
    Ok(record_payload_kind(payload).map(|kind| RecordSummary { seq, slot, kind }))
}

fn read_record<S: LogFlashStorage>(
    storage: &mut S,
    slot: usize,
) -> Result<Option<DecodedRecord>, FlashLogError> {
    let mut record = [0u8; FLASH_LOG_RECORD_SIZE];
    let Some((seq, len)) = read_record_meta(storage, slot, &mut record)? else {
        return Ok(None);
    };
    let payload = &record[FLASH_LOG_HEADER_SIZE..FLASH_LOG_HEADER_SIZE + len];
    Ok(LogEntry::decode_payload(payload).map(|entry| DecodedRecord { seq, entry }))
}

fn read_record_meta<S: LogFlashStorage>(
    storage: &mut S,
    slot: usize,
    record: &mut [u8; FLASH_LOG_RECORD_SIZE],
) -> Result<Option<(u32, usize)>, FlashLogError> {
    storage.read(slot * FLASH_LOG_RECORD_SIZE, record)?;
    if record.iter().all(|b| *b == 0xFF) {
        return Ok(None);
    }

    let magic = u32::from_le_bytes(record[0..4].try_into().unwrap());
    if magic != FLASH_LOG_MAGIC {
        return Ok(None);
    }

    let seq = u32::from_le_bytes(record[4..8].try_into().unwrap());
    let len = u16::from_le_bytes(record[8..10].try_into().unwrap()) as usize;
    let expected = u32::from_le_bytes(record[10..14].try_into().unwrap());
    if seq == 0 || len > FLASH_LOG_PAYLOAD_SIZE {
        return Ok(None);
    }
    let payload = &record[FLASH_LOG_HEADER_SIZE..FLASH_LOG_HEADER_SIZE + len];
    if checksum(payload) != expected {
        return Ok(None);
    }

    Ok(Some((seq, len)))
}

fn record_payload_kind(payload: &[u8]) -> Option<LogKind> {
    let payload = core::str::from_utf8(payload).ok()?;
    let mut parts = payload.split('\t');
    if parts.next()? != "1" {
        return None;
    }
    LogKind::parse(parts.next()?)
}

fn checksum(bytes: &[u8]) -> u32 {
    let mut hash = 0x811C_9DC5u32;
    for b in bytes {
        hash ^= *b as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

fn escaped_field_len(input: &str) -> usize {
    input
        .chars()
        .map(|ch| match ch {
            '\\' | '\t' | '\n' | '\r' => 2,
            _ => ch.len_utf8(),
        })
        .sum()
}

fn push_escaped_field(out: &mut String, input: &str) {
    for ch in input.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(ch),
        }
    }
}

fn push_escaped_field_truncated(out: &mut String, input: &str, max_escaped_bytes: usize) {
    let mut used = 0;
    for ch in input.chars() {
        let encoded_len = match ch {
            '\\' | '\t' | '\n' | '\r' => 2,
            _ => ch.len_utf8(),
        };
        if used + encoded_len > max_escaped_bytes {
            break;
        }
        match ch {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(ch),
        }
        used += encoded_len;
    }
}

fn unescape_field(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[derive(Debug, Clone)]
pub struct MemFlashLogStorage {
    data: Vec<u8>,
    erase_size: usize,
}

impl MemFlashLogStorage {
    pub fn new(size: usize, erase_size: usize) -> Self {
        Self {
            data: vec![0xFF; size],
            erase_size,
        }
    }

    pub fn corrupt_byte(&mut self, offset: usize) {
        self.data[offset] ^= 0x55;
    }
}

impl LogFlashStorage for MemFlashLogStorage {
    fn len(&self) -> usize {
        self.data.len()
    }

    fn erase_size(&self) -> usize {
        self.erase_size
    }

    fn read(&mut self, offset: usize, buf: &mut [u8]) -> Result<(), FlashLogError> {
        let end = offset
            .checked_add(buf.len())
            .ok_or(FlashLogError::OutOfBounds)?;
        let src = self
            .data
            .get(offset..end)
            .ok_or(FlashLogError::OutOfBounds)?;
        buf.copy_from_slice(src);
        Ok(())
    }

    fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), FlashLogError> {
        let end = offset
            .checked_add(data.len())
            .ok_or(FlashLogError::OutOfBounds)?;
        let dst = self
            .data
            .get_mut(offset..end)
            .ok_or(FlashLogError::OutOfBounds)?;
        for (old, new) in dst.iter_mut().zip(data) {
            *old &= *new;
        }
        Ok(())
    }

    fn erase(&mut self, offset: usize, len: usize) -> Result<(), FlashLogError> {
        if !offset.is_multiple_of(self.erase_size) || !len.is_multiple_of(self.erase_size) {
            return Err(FlashLogError::InvalidGeometry {
                size: self.data.len(),
                erase_size: self.erase_size,
            });
        }
        let end = offset.checked_add(len).ok_or(FlashLogError::OutOfBounds)?;
        let dst = self
            .data
            .get_mut(offset..end)
            .ok_or(FlashLogError::OutOfBounds)?;
        dst.fill(0xFF);
        Ok(())
    }
}

#[cfg(feature = "esp32")]
pub struct EspFlashLogStorage {
    partition: esp_idf_svc::partition::EspPartition,
    size: usize,
    erase_size: usize,
}

#[cfg(feature = "esp32")]
impl EspFlashLogStorage {
    pub fn open(label: &str) -> Result<Self, FlashLogError> {
        let partition = unsafe {
            // SAFETY: the firmware creates exactly one LogRing for the `log_ring`
            // partition and keeps exclusive mutable access to it in main().
            esp_idf_svc::partition::EspPartition::new(label)
                .map_err(|e| FlashLogError::Storage(e.to_string()))?
        }
        .ok_or_else(|| FlashLogError::PartitionNotFound(label.to_string()))?;
        let size = partition.size();
        let erase_size = partition.erase_size();
        Ok(Self {
            partition,
            size,
            erase_size,
        })
    }
}

#[cfg(feature = "esp32")]
impl LogFlashStorage for EspFlashLogStorage {
    fn len(&self) -> usize {
        self.size
    }

    fn erase_size(&self) -> usize {
        self.erase_size
    }

    fn read(&mut self, offset: usize, buf: &mut [u8]) -> Result<(), FlashLogError> {
        self.partition
            .read(offset, buf)
            .map_err(|e| FlashLogError::Storage(e.to_string()))
    }

    fn write(&mut self, offset: usize, data: &[u8]) -> Result<(), FlashLogError> {
        self.partition
            .write(offset, data)
            .map_err(|e| FlashLogError::Storage(e.to_string()))
    }

    fn erase(&mut self, offset: usize, len: usize) -> Result<(), FlashLogError> {
        self.partition
            .erase(offset, len)
            .map_err(|e| FlashLogError::Storage(e.to_string()))
    }
}

#[cfg(feature = "esp32")]
pub fn open_flash_log_ring(label: &str) -> Result<LogRing, FlashLogError> {
    LogRing::with_flash(Box::new(EspFlashLogStorage::open(label)?))
}
