//! Maps IM message IDs → SMS phone numbers for reply routing.
//!
//! 200-slot ring buffer: slot = message_id % 200.
//! Exact-match check prevents stale-slot collisions.

use crate::im::MessageId;
use crate::persist::{keys, Store, StoreError};

const SLOT_COUNT: usize = 200;
const PHONE_MAX: usize = 23; // 22 chars + NUL
                             // Preserve the legacy NVS record size and field offsets without reading Rust
                             // struct padding or depending on compiler field layout.
const STORED_SLOT_BYTES: usize = 32;
const STORED_MAP_BYTES: usize = SLOT_COUNT * STORED_SLOT_BYTES;

#[derive(Debug, Clone, Copy, Default)]
struct Slot {
    message_id: i64, // 0 = empty
    phone: [u8; PHONE_MAX],
}

impl Slot {
    fn phone_str(&self) -> &str {
        let nul = self.phone.iter().position(|&b| b == 0).unwrap_or(PHONE_MAX);
        std::str::from_utf8(&self.phone[..nul]).unwrap_or("")
    }
}

/// Ring buffer mapping MessageId → phone number.
pub struct ReplyRouter {
    slots: Box<[Slot; SLOT_COUNT]>,
}

impl ReplyRouter {
    pub fn new() -> Self {
        ReplyRouter {
            slots: Box::new([Slot::default(); SLOT_COUNT]),
        }
    }

    /// Load from persistent store.
    pub fn load(&mut self, store: &dyn Store) {
        let Some(bytes) = store.load(keys::REPLY_MAP) else {
            return;
        };
        if bytes.len() < STORED_MAP_BYTES {
            return;
        }
        let (records, _) = bytes.as_chunks::<STORED_SLOT_BYTES>();
        for (index, (slot, record)) in self.slots.iter_mut().zip(records).enumerate() {
            let mut message_id = [0u8; 8];
            message_id.copy_from_slice(&record[..8]);
            slot.message_id = i64::from_le_bytes(message_id);
            slot.phone.copy_from_slice(&record[8..8 + PHONE_MAX]);
            if slot.message_id != 0
                && (slot.message_id.unsigned_abs() as usize) % SLOT_COUNT != index
            {
                *slot = Slot::default();
            }
        }
    }

    /// Save to persistent store.
    pub fn save(&self, store: &mut dyn Store) -> Result<(), StoreError> {
        let mut bytes = vec![0u8; STORED_MAP_BYTES];
        let (records, _) = bytes.as_chunks_mut::<STORED_SLOT_BYTES>();
        for (slot, record) in self.slots.iter().zip(records) {
            record[..8].copy_from_slice(&slot.message_id.to_le_bytes());
            record[8..8 + PHONE_MAX].copy_from_slice(&slot.phone);
        }
        store.save(keys::REPLY_MAP, &bytes)
    }

    /// Record a (message_id, phone) mapping. Persists immediately.
    pub fn put(&mut self, message_id: MessageId, phone: &str, store: &mut dyn Store) -> bool {
        let idx = (message_id.unsigned_abs() as usize) % SLOT_COUNT;
        let slot = &mut self.slots[idx];
        slot.message_id = message_id;
        slot.phone = [0u8; PHONE_MAX];
        let bytes = phone.as_bytes();
        let copy_len = bytes.len().min(PHONE_MAX - 1);
        slot.phone[..copy_len].copy_from_slice(&bytes[..copy_len]);
        match self.save(store) {
            Ok(()) => true,
            Err(error) => {
                log::error!("[reply_router] failed to persist reply map: {}", error);
                false
            }
        }
    }

    /// Look up a phone number by message_id. Returns None if not found or overwritten.
    pub fn lookup(&self, message_id: MessageId) -> Option<&str> {
        let idx = (message_id.unsigned_abs() as usize) % SLOT_COUNT;
        let slot = &self.slots[idx];
        if slot.message_id == message_id && slot.message_id != 0 {
            let s = slot.phone_str();
            if !s.is_empty() {
                Some(s)
            } else {
                None
            }
        } else {
            None
        }
    }
}

impl Default for ReplyRouter {
    fn default() -> Self {
        Self::new()
    }
}
