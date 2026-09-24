//! Concatenated SMS reassembly.
//!
//! Maintains an in-flight table of partial multi-part messages.
//! When all parts arrive, the group is assembled and returned.

use super::codec::SmsPdu;
use std::time::{Duration, Instant};

/// Maximum in-flight concatenation groups at once.
const MAX_GROUPS: usize = 8;
/// Maximum distinct stored slots tracked for one concatenated message.
const MAX_SLOTS_PER_GROUP: usize = 64;
/// How long to keep an incomplete group before discarding it.
const GROUP_TTL: Duration = Duration::from_secs(24 * 3600);

/// An in-progress multi-part SMS.
#[derive(Debug)]
struct Group {
    sender: String,
    ref_num: u16,
    total: u8,
    parts: Vec<Option<String>>, // indexed by part_num - 1
    received: usize,
    first_seen: Instant,
    timestamp: String, // from first part
    slots: Vec<StorageSlot>,
}

impl Group {
    fn new(sender: &str, ref_num: u16, total: u8, timestamp: &str) -> Self {
        Group {
            sender: sender.to_string(),
            ref_num,
            total,
            parts: vec![None; total as usize],
            received: 0,
            first_seen: Instant::now(),
            timestamp: timestamp.to_string(),
            slots: Vec::new(),
        }
    }

    fn track_slot(&mut self, slot: StorageSlot) -> bool {
        if let Some(existing) = self
            .slots
            .iter()
            .find(|existing| existing.mem == slot.mem && existing.index == slot.index)
        {
            return existing.fingerprint == slot.fingerprint;
        }
        if self.slots.len() >= MAX_SLOTS_PER_GROUP {
            return false;
        }
        self.slots.push(slot);
        true
    }

    fn insert(&mut self, part_num: u8, content: String) -> bool {
        let idx = (part_num as usize).saturating_sub(1);
        if idx >= self.parts.len() {
            return false;
        }
        if self.parts[idx].is_none() {
            self.parts[idx] = Some(content);
            self.received += 1;
        }
        self.received == self.total as usize
    }

    fn assemble(&self) -> String {
        let len = self
            .parts
            .iter()
            .filter_map(|p| p.as_ref())
            .map(String::len)
            .sum();
        let mut content = String::with_capacity(len);
        for part in self.parts.iter().filter_map(|p| p.as_deref()) {
            content.push_str(part);
        }
        content
    }

    fn is_expired(&self) -> bool {
        self.first_seen.elapsed() > GROUP_TTL
    }
}

/// Completed message assembled from concatenated SMS parts.
#[derive(Debug)]
pub struct CompletedSms {
    pub sender: String,
    pub content: String,
    pub timestamp: String,
    /// Modem storage slots containing this message's parts.
    pub slots: Vec<StorageSlot>,
}

/// Modem storage location and in-memory content identity of an SMS part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageSlot {
    pub mem: String,
    pub index: u16,
    pub fingerprint: u64,
}

/// Result of feeding a concatenated SMS part to the reassembler.
#[derive(Debug)]
pub enum FeedOutcome {
    Incomplete,
    Complete(CompletedSms),
    Invalid,
}

/// Manages in-flight concatenated SMS groups.
pub struct ConcatReassembler {
    groups: Vec<Group>,
}

impl ConcatReassembler {
    pub fn new() -> Self {
        ConcatReassembler {
            groups: Vec::with_capacity(MAX_GROUPS),
        }
    }

    /// Feed a parsed PDU. Returns `Some(CompletedSms)` when all parts have arrived.
    pub fn feed(&mut self, pdu: &SmsPdu) -> Option<CompletedSms> {
        match self.feed_with_slot(pdu, None) {
            FeedOutcome::Complete(completed) => Some(completed),
            FeedOutcome::Incomplete | FeedOutcome::Invalid => None,
        }
    }

    /// Feed a parsed PDU and optionally retain its modem storage location.
    pub fn feed_with_slot(&mut self, pdu: &SmsPdu, slot: Option<StorageSlot>) -> FeedOutcome {
        if !pdu.is_concatenated {
            // Single-part messages are handled by the caller.
            return FeedOutcome::Invalid;
        }

        // Reject before touching the group table: a never-completing group
        // wastes one of the 8 slots for up to 24 h.
        if pdu.concat_total == 0 || pdu.concat_part == 0 || pdu.concat_part > pdu.concat_total {
            log::warn!(
                "[concat] malformed concat header from {}: part={}/{} — discarded",
                pdu.sender,
                pdu.concat_part,
                pdu.concat_total
            );
            return FeedOutcome::Invalid;
        }

        // Evict expired groups first
        self.groups.retain(|g| !g.is_expired());

        // Find or create matching group
        let key = (&pdu.sender[..], pdu.concat_ref);
        let group_idx = self
            .groups
            .iter()
            .position(|g| g.sender == key.0 && g.ref_num == key.1 && g.total == pdu.concat_total);

        let idx = match group_idx {
            Some(i) => i,
            None => {
                // Evict LRU if at capacity
                if self.groups.len() >= MAX_GROUPS {
                    let oldest = self
                        .groups
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, g)| g.first_seen)
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    log::warn!("[concat] evicting oldest group to make room");
                    self.groups.remove(oldest);
                }
                self.groups.push(Group::new(
                    &pdu.sender,
                    pdu.concat_ref,
                    pdu.concat_total,
                    &pdu.timestamp,
                ));
                self.groups.len() - 1
            }
        };

        let part_index = usize::from(pdu.concat_part - 1);
        if self.groups[idx].parts[part_index]
            .as_ref()
            .is_some_and(|existing| existing != &pdu.content)
        {
            log::warn!("[concat] conflicting content for the same part number");
            return FeedOutcome::Invalid;
        }

        if let Some(slot) = slot {
            if !self.groups[idx].track_slot(slot) {
                log::warn!("[concat] too many stored slots in one group");
                return FeedOutcome::Invalid;
            }
        }

        let complete = self.groups[idx].insert(pdu.concat_part, pdu.content.clone());
        if complete {
            let g = self.groups.remove(idx);
            let content = g.assemble();
            FeedOutcome::Complete(CompletedSms {
                sender: g.sender,
                content,
                timestamp: g.timestamp,
                slots: g.slots,
            })
        } else {
            FeedOutcome::Incomplete
        }
    }

    /// Number of in-progress groups.
    pub fn group_count(&self) -> usize {
        self.groups.len()
    }
}

impl Default for ConcatReassembler {
    fn default() -> Self {
        Self::new()
    }
}
