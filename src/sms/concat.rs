//! Concatenated SMS reassembly.
//!
//! Maintains an in-flight table of partial multi-part messages.
//! When all parts arrive, the group is assembled and returned.

use super::codec::SmsPdu;
use std::time::{Duration, Instant};

/// Maximum in-flight concatenation groups at once.
const MAX_GROUPS: usize = 8;
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
        }
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
        self.parts.iter().filter_map(|p| p.as_deref()).collect()
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
        if !pdu.is_concatenated {
            // Single-part — return immediately as-is (caller handles it).
            return None;
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
            return None;
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

        let complete = self.groups[idx].insert(pdu.concat_part, pdu.content.clone());
        if complete {
            let g = self.groups.remove(idx);
            let content = g.assemble();
            Some(CompletedSms {
                sender: g.sender,
                content,
                timestamp: g.timestamp,
            })
        } else {
            None
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
