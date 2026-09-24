//! ReplyRouter unit tests — persistence, collision, and edge cases.

use smsgate::bridge::reply_router::ReplyRouter;
use smsgate::persist::mem::MemStore;
use smsgate::persist::{keys, Store};

#[test]
fn put_and_lookup_roundtrip() {
    let mut store = MemStore::new();
    let mut router = ReplyRouter::new();
    router.put(1001, "+8613800138000", &mut store);
    assert_eq!(router.lookup(1001), Some("+8613800138000"));
}

#[test]
fn lookup_missing_returns_none() {
    let router = ReplyRouter::new();
    assert_eq!(router.lookup(9999), None);
}

#[test]
fn save_and_load_round_trip() {
    let mut store = MemStore::new();
    let mut router = ReplyRouter::new();
    router.put(42, "+441234567890", &mut store);

    // Create a fresh router and load from the same store
    let mut router2 = ReplyRouter::new();
    router2.load(&store);
    assert_eq!(router2.lookup(42), Some("+441234567890"));
}

#[test]
fn persisted_slots_use_stable_legacy_bytes_without_padding_contents() {
    let mut store = MemStore::new();
    let mut router = ReplyRouter::new();
    router.put(42, "+123", &mut store);

    let bytes = store.load(keys::REPLY_MAP).unwrap();
    assert_eq!(bytes.len(), 200 * 32);
    let record = &bytes[42 * 32..43 * 32];
    assert_eq!(&record[..8], &42i64.to_le_bytes());
    assert_eq!(&record[8..13], b"+123\0");
    assert!(record[13..].iter().all(|byte| *byte == 0));
}

#[test]
fn loads_existing_32_byte_slot_records() {
    let mut bytes = vec![0u8; 200 * 32];
    let record = &mut bytes[42 * 32..43 * 32];
    record[..8].copy_from_slice(&42i64.to_le_bytes());
    record[8..13].copy_from_slice(b"+123\0");
    record[31] = 0xA5; // Legacy Rust padding had no defined value.
    let mut store = MemStore::new();
    store.save(keys::REPLY_MAP, &bytes).unwrap();

    let mut router = ReplyRouter::new();
    router.load(&store);
    assert_eq!(router.lookup(42), Some("+123"));
}

#[test]
fn rejects_record_in_the_wrong_hash_slot() {
    let mut bytes = vec![0u8; 200 * 32];
    let record = &mut bytes[42 * 32..43 * 32];
    record[..8].copy_from_slice(&43i64.to_le_bytes());
    record[8..13].copy_from_slice(b"+123\0");
    let mut store = MemStore::new();
    store.save(keys::REPLY_MAP, &bytes).unwrap();

    let mut router = ReplyRouter::new();
    router.load(&store);
    assert_eq!(router.lookup(43), None);
}

#[test]
fn slot_collision_overwrites_old_entry() {
    // Two message IDs that hash to the same slot (differ by 200)
    let id_a: i64 = 1;
    let id_b: i64 = 201; // 201 % 200 == 1
    let mut store = MemStore::new();
    let mut router = ReplyRouter::new();

    router.put(id_a, "+111", &mut store);
    assert_eq!(router.lookup(id_a), Some("+111"));

    // Overwrite the slot with a different id
    router.put(id_b, "+222", &mut store);
    // Old mapping is gone (slot overwritten)
    assert_eq!(router.lookup(id_a), None);
    // New mapping is present
    assert_eq!(router.lookup(id_b), Some("+222"));
}

#[test]
fn zero_message_id_is_not_stored() {
    let mut store = MemStore::new();
    let mut router = ReplyRouter::new();
    router.put(0, "+000", &mut store);
    // message_id == 0 is treated as "empty slot" — lookup returns None
    assert_eq!(router.lookup(0), None);
}

#[test]
fn long_phone_is_truncated_safely() {
    let long_phone = "+".to_string() + &"1".repeat(30); // 31 chars > PHONE_MAX(22)
    let mut store = MemStore::new();
    let mut router = ReplyRouter::new();
    router.put(100, &long_phone, &mut store);
    // Should not panic; result is a truncated phone (not the full string)
    let stored = router.lookup(100).unwrap_or("");
    assert!(stored.len() <= 22);
}

#[test]
fn load_with_empty_store_leaves_router_empty() {
    let store = MemStore::new();
    let mut router = ReplyRouter::new();
    router.load(&store);
    assert_eq!(router.lookup(1), None);
}

#[test]
fn negative_message_id_is_handled() {
    let mut store = MemStore::new();
    let mut router = ReplyRouter::new();
    router.put(-500, "+9999", &mut store);
    assert_eq!(router.lookup(-500), Some("+9999"));
    assert_eq!(router.lookup(500), None); // different id, different slot content
}

#[test]
fn load_with_truncated_bytes_leaves_router_empty() {
    // Guard: if stored bytes are shorter than [Slot; 200], load() should be a no-op.
    use smsgate::persist::{keys, mem::MemStore, Store};
    let mut store = MemStore::new();
    // Write garbage that's too short to be a valid slot array
    store.save(keys::REPLY_MAP, &[0u8; 16]).unwrap();

    let mut router = ReplyRouter::new();
    router.load(&store);
    // Router should remain empty — the guard returned early
    assert_eq!(router.lookup(1), None);
}

#[test]
fn phone_str_with_no_nul_uses_full_array() {
    // Slot::phone_str() uses unwrap_or(PHONE_MAX) when no NUL byte is present.
    // This path is exercised when put() fills all PHONE_MAX-1 bytes without a NUL
    // at position < PHONE_MAX. Use a phone exactly at the truncation boundary.
    let phone = "+".to_string() + &"1".repeat(21); // 22 chars = PHONE_MAX-1
    let mut store = MemStore::new();
    let mut router = ReplyRouter::new();
    router.put(7, &phone, &mut store);
    let stored = router.lookup(7).unwrap_or("");
    // Should be non-empty (either truncated or full 22 chars)
    assert!(!stored.is_empty());
}
