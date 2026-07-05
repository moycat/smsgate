//! Heap allocation guardrails for flash-backed log reads.

#[path = "support/alloc_counter.rs"]
mod alloc_counter;

use smsgate::log_ring::{
    FlashLogRing, LogEntry, LogEvent, LogKind, MemFlashLogStorage, FLASH_LOG_RECORD_SIZE,
};

#[test]
fn latest_page_allocations_are_bounded_by_page_size() {
    let storage = MemFlashLogStorage::new(FLASH_LOG_RECORD_SIZE * 64, FLASH_LOG_RECORD_SIZE * 16);
    let mut ring = FlashLogRing::mount(storage).unwrap();
    for i in 0..64 {
        let entry = LogEvent::new(LogKind::System, "event", &format!("entry-{i}"), true).at("ts");
        ring.append(&entry).unwrap();
    }

    let (page, allocations) = alloc_counter::count_allocations(|| ring.page(0, 16).unwrap());

    assert_eq!(page.len(), 16);
    assert_eq!(page[0].body_preview, "entry-63");
    assert!(
        allocations <= 96,
        "latest page allocated {allocations} times; expected allocation count to scale with page size"
    );
}

#[test]
fn latest_page_allocated_bytes_are_bounded_by_page_size() {
    let storage = MemFlashLogStorage::new(FLASH_LOG_RECORD_SIZE * 512, FLASH_LOG_RECORD_SIZE * 16);
    let mut ring = FlashLogRing::mount(storage).unwrap();
    for i in 0..512 {
        let entry = LogEvent::new(LogKind::System, "event", &format!("entry-{i}"), true).at("ts");
        ring.append(&entry).unwrap();
    }

    let (page, bytes) = alloc_counter::count_allocated_bytes(|| ring.page(0, 16).unwrap());

    assert_eq!(page.len(), 16);
    assert_eq!(page[0].body_preview, "entry-511");
    assert!(
        bytes <= 4 * 1024,
        "latest page allocated {bytes} bytes; expected allocated bytes to scale with page size"
    );
}

#[test]
fn oversized_append_allocations_are_bounded() {
    let storage = MemFlashLogStorage::new(FLASH_LOG_RECORD_SIZE * 4, FLASH_LOG_RECORD_SIZE * 2);
    let mut ring = FlashLogRing::mount(storage).unwrap();
    let long_detail = "network error with <escaped> detail & retry ".repeat(40);
    let entry = LogEvent::new(LogKind::Network, "telegram", &long_detail, false).at("ts");

    let (result, allocations) = alloc_counter::count_allocations(|| ring.append(&entry));

    assert!(result.is_ok());
    assert!(
        allocations <= 32,
        "oversized append allocated {allocations} times; expected bounded one-pass encoding"
    );
}

#[test]
fn latest_of_kind_allocations_are_bounded() {
    let storage = MemFlashLogStorage::new(FLASH_LOG_RECORD_SIZE * 64, FLASH_LOG_RECORD_SIZE * 16);
    let mut ring = FlashLogRing::mount(storage).unwrap();
    ring.append(&LogEntry::sms(
        "+15551234567".to_string(),
        "hello".to_string(),
        "sms-ts".to_string(),
        true,
    ))
    .unwrap();
    for i in 0..63 {
        let entry = LogEvent::new(LogKind::System, "event", &format!("entry-{i}"), true).at("ts");
        ring.append(&entry).unwrap();
    }

    let (latest, allocations) =
        alloc_counter::count_allocations(|| ring.latest_of_kind(LogKind::Sms).unwrap());

    assert_eq!(latest.unwrap().sender, "+15551234567");
    assert!(
        allocations <= 32,
        "latest_of_kind allocated {allocations} times; expected allocation count independent of nonmatching log records"
    );
}
