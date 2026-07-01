//! Persistence layer tests — helpers and MemStore.

use smsgate::persist::{keys, load_bool, load_i64, mem::MemStore, save_bool, save_i64, Store};

#[test]
fn save_and_load_i64_roundtrip() {
    let mut store = MemStore::new();
    save_i64(&mut store, "cursor", 12345678_i64).unwrap();
    assert_eq!(load_i64(&store, "cursor"), Some(12345678));
}

#[test]
fn load_i64_missing_key_returns_none() {
    let store = MemStore::new();
    assert_eq!(load_i64(&store, "nonexistent"), None);
}

#[test]
fn load_i64_short_payload_returns_none() {
    let mut store = MemStore::new();
    store.save("bad", &[1u8, 2, 3]).unwrap(); // only 3 bytes, need 8
    assert_eq!(load_i64(&store, "bad"), None);
}

#[test]
fn save_and_load_bool_true() {
    let mut store = MemStore::new();
    save_bool(&mut store, keys::FWD_ENABLED, true).unwrap();
    assert_eq!(load_bool(&store, keys::FWD_ENABLED), Some(true));
}

#[test]
fn save_and_load_bool_false() {
    let mut store = MemStore::new();
    save_bool(&mut store, keys::FWD_ENABLED, false).unwrap();
    assert_eq!(load_bool(&store, keys::FWD_ENABLED), Some(false));
}

#[test]
fn load_bool_missing_returns_none() {
    let store = MemStore::new();
    assert_eq!(load_bool(&store, "absent"), None);
}

#[test]
fn memstore_overwrite_key() {
    let mut store = MemStore::new();
    save_i64(&mut store, "x", 1).unwrap();
    save_i64(&mut store, "x", 2).unwrap();
    assert_eq!(load_i64(&store, "x"), Some(2));
}

#[test]
fn load_bool_empty_bytes_returns_false() {
    // load_bool calls bytes.first().copied().unwrap_or(0); empty slice → false.
    let mut store = MemStore::new();
    store.save("empty_bool", &[]).unwrap();
    assert_eq!(load_bool(&store, "empty_bool"), Some(false));
}

#[test]
fn load_i64_exact_eight_bytes() {
    // Boundary: exactly 8 bytes should parse correctly.
    let mut store = MemStore::new();
    store.save("eight", &42i64.to_le_bytes()).unwrap();
    assert_eq!(load_i64(&store, "eight"), Some(42));
}
