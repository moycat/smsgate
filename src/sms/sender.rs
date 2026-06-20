//! Outbound SMS queue with exponential-backoff retry.

use super::codec::build_sms_submit_pdus;
use crate::modem::ModemPort;
use std::time::{Duration, Instant};

/// Maximum queue depth.
const QUEUE_DEPTH: usize = 8;
/// Retry delays: 2 s, 8 s, 30 s (then give up).
const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(2),
    Duration::from_secs(8),
    Duration::from_secs(30),
];
/// Maximum delivery attempts per entry.
const MAX_ATTEMPTS: usize = 4; // 1 initial + 3 retries

/// Rate limit for /send bot commands: at most this many per window.
const CMD_SEND_LIMIT: u8 = 5;
/// Rate limit window duration in seconds.
const CMD_SEND_WINDOW_SECS: u64 = 60;

/// A pending outbound SMS entry.
#[derive(Debug)]
pub struct QueueEntry {
    pub id: u32,
    pub phone: String,
    pub body: String,
    pub attempts: usize,
    pub next_attempt: Option<Instant>,
    pub enqueued_at: Instant,
}

impl QueueEntry {
    fn is_ready(&self) -> bool {
        match self.next_attempt {
            None => true,
            Some(t) => Instant::now() >= t,
        }
    }
}

/// Snapshot of a queue entry for the /queue command.
#[derive(Debug, Clone)]
pub struct QueueSnapshot {
    pub id: u32,
    pub phone: String,
    pub body_preview: String,
    pub attempts: usize,
    pub age_secs: u64,
}

/// Result of `enqueue_command_send` (the rate-limited path for /send commands).
#[derive(Debug, PartialEq)]
pub enum CmdSendResult {
    Enqueued(u32),
    /// Queue full or exact duplicate already pending.
    QueueFull,
    /// Rate limit exceeded: too many /send commands in the current window.
    RateLimited,
}

/// Result of a single `drain_once` call.
#[derive(Debug)]
pub enum DrainOutcome {
    Idle,
    Sent { phone: String },
    Retrying,
    Dropped { phone: String },
    BadPdu,
}

impl DrainOutcome {
    /// Returns true if an attempt was made (i.e. not `Idle`).
    pub fn attempted(&self) -> bool {
        !matches!(self, DrainOutcome::Idle)
    }
}

/// Outbound SMS queue with retry.
pub struct SmsSender {
    entries: Vec<QueueEntry>,
    next_id: u32,
    /// Start of the current /send command rate-limit window (None = no sends yet).
    cmd_window_start: Option<Instant>,
    /// Number of /send commands issued in the current window.
    cmd_window_count: u8,
}

impl SmsSender {
    pub fn new() -> Self {
        SmsSender {
            entries: Vec::with_capacity(QUEUE_DEPTH),
            next_id: 1,
            cmd_window_start: None,
            cmd_window_count: 0,
        }
    }

    /// Enqueue a message. Returns the assigned queue ID, or None if queue is full.
    pub fn enqueue(&mut self, phone: String, body: String) -> Option<u32> {
        if self.entries.len() >= QUEUE_DEPTH {
            log::warn!("[sender] queue full, dropping message to {}", phone);
            return None;
        }
        // Dedup: if same phone+body already queued, skip
        if self
            .entries
            .iter()
            .any(|e| e.phone == phone && e.body == body)
        {
            log::info!("[sender] duplicate suppressed for {}", phone);
            return None;
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.entries.push(QueueEntry {
            id,
            phone,
            body,
            attempts: 0,
            next_attempt: None,
            enqueued_at: Instant::now(),
        });
        Some(id)
    }

    /// Enqueue an SMS from a /send bot command, subject to rate limiting.
    ///
    /// At most `CMD_SEND_LIMIT` sends are allowed per `CMD_SEND_WINDOW_SECS`-second
    /// window. Reply-to-SMS routing bypasses this limit; only explicit `/send`
    /// commands use this path.
    pub fn enqueue_command_send(&mut self, phone: String, body: String) -> CmdSendResult {
        let now = Instant::now();
        let window_expired = self
            .cmd_window_start
            .map(|s| now.duration_since(s) >= Duration::from_secs(CMD_SEND_WINDOW_SECS))
            .unwrap_or(true);
        if window_expired {
            self.cmd_window_start = Some(now);
            self.cmd_window_count = 0;
        }
        if self.cmd_window_count >= CMD_SEND_LIMIT {
            log::warn!(
                "[sender] /send rate limit: {}/{} in {}s window",
                self.cmd_window_count,
                CMD_SEND_LIMIT,
                CMD_SEND_WINDOW_SECS
            );
            return CmdSendResult::RateLimited;
        }
        match self.enqueue(phone, body) {
            Some(id) => {
                self.cmd_window_count += 1;
                CmdSendResult::Enqueued(id)
            }
            None => CmdSendResult::QueueFull,
        }
    }

    /// Process one ready entry against the modem. Called from the main loop.
    pub fn drain_once(&mut self, modem: &mut dyn ModemPort) -> DrainOutcome {
        let Some(idx) = self.entries.iter().position(|e| e.is_ready()) else {
            return DrainOutcome::Idle;
        };
        let entry = &mut self.entries[idx];
        entry.attempts += 1;
        let attempt = entry.attempts;
        let phone = entry.phone.clone();
        let body = entry.body.clone();

        log::info!(
            "[sender] attempt {} for {} ({}..)",
            attempt,
            phone,
            &body[..body.len().min(20)]
        );

        let pdus = build_sms_submit_pdus(&phone, &body, super::MAX_SMS_PARTS, false);
        if pdus.is_empty() {
            log::error!("[sender] PDU build failed for {} — dropping", phone);
            self.entries.remove(idx);
            return DrainOutcome::BadPdu;
        }

        let mut success = true;
        for pdu in &pdus {
            match modem.send_pdu_sms(&pdu.hex, pdu.tpdu_len) {
                Ok(_mr) => {
                    log::info!("[sender] part sent ok");
                }
                Err(e) => {
                    log::warn!("[sender] send failed: {}", e);
                    success = false;
                    break;
                }
            }
        }

        if success {
            self.entries.remove(idx);
            DrainOutcome::Sent { phone }
        } else if attempt >= MAX_ATTEMPTS {
            log::error!("[sender] max attempts reached for {}, dropping", phone);
            self.entries.remove(idx);
            DrainOutcome::Dropped { phone }
        } else {
            let delay = RETRY_DELAYS
                .get(attempt - 1)
                .copied()
                .unwrap_or(RETRY_DELAYS[2]);
            self.entries[idx].next_attempt = Some(Instant::now() + delay);
            DrainOutcome::Retrying
        }
    }

    /// Cancel all entries for a given phone number. Returns count cancelled.
    pub fn cancel_for_phone(&mut self, phone: &str) -> usize {
        let before = self.entries.len();
        self.entries.retain(|e| e.phone != phone);
        before - self.entries.len()
    }

    /// Cancel a specific entry by ID.
    pub fn cancel_by_id(&mut self, id: u32) -> bool {
        if let Some(pos) = self.entries.iter().position(|e| e.id == id) {
            self.entries.remove(pos);
            true
        } else {
            false
        }
    }

    /// Returns a snapshot of all queued entries.
    pub fn snapshot(&self) -> Vec<QueueSnapshot> {
        self.entries
            .iter()
            .map(|e| QueueSnapshot {
                id: e.id,
                phone: e.phone.clone(),
                body_preview: e.body.chars().take(30).collect(),
                attempts: e.attempts,
                age_secs: e.enqueued_at.elapsed().as_secs(),
            })
            .collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for SmsSender {
    fn default() -> Self {
        Self::new()
    }
}
