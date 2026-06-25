//! Flash-backed log ring tests.

use smsgate::log_ring::{
    FlashLogRing, LogEntry, LogEvent, LogKind, MemFlashLogStorage, FLASH_LOG_RECORD_SIZE,
};

fn event(subject: &str, detail: &str) -> LogEntry {
    LogEvent::new(LogKind::System, subject, detail, true).at("2026-06-21 20:30:45+08:00")
}

#[test]
fn flash_log_survives_remount() {
    let storage = MemFlashLogStorage::new(FLASH_LOG_RECORD_SIZE * 8, FLASH_LOG_RECORD_SIZE * 2);
    let mut ring = FlashLogRing::mount(storage).unwrap();

    ring.append(&event("boot", "smsgate starting")).unwrap();
    ring.append(&event("wifi", "connected")).unwrap();

    let storage = ring.into_storage();
    let mut ring = FlashLogRing::mount(storage).unwrap();
    let entries = ring.last_n(4).unwrap();

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].sender, "boot");
    assert_eq!(entries[0].body_preview, "smsgate starting");
    assert_eq!(entries[1].sender, "wifi");
    assert_eq!(entries[1].kind, LogKind::System);
}

#[test]
fn flash_log_preserves_call_entries() {
    let storage = MemFlashLogStorage::new(FLASH_LOG_RECORD_SIZE * 8, FLASH_LOG_RECORD_SIZE * 2);
    let mut ring = FlashLogRing::mount(storage).unwrap();
    let entry = LogEvent::new(
        LogKind::Call,
        "+86 138-0013-8000",
        "incoming call; hung up",
        true,
    )
    .at("2026-06-21 20:30:45+08:00");

    ring.append(&entry).unwrap();

    let storage = ring.into_storage();
    let mut ring = FlashLogRing::mount(storage).unwrap();
    let entries = ring.last_n(1).unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, LogKind::Call);
    assert_eq!(entries[0].sender, "+86 138-0013-8000");
    assert_eq!(entries[0].body_preview, "incoming call; hung up");
    assert!(entries[0].forwarded);
}

#[test]
fn flash_log_wraps_by_erasing_oldest_sector() {
    let storage = MemFlashLogStorage::new(FLASH_LOG_RECORD_SIZE * 4, FLASH_LOG_RECORD_SIZE * 2);
    let mut ring = FlashLogRing::mount(storage).unwrap();

    for i in 0..6 {
        ring.append(&event("event", &format!("entry-{i}"))).unwrap();
    }

    let entries = ring.last_n(8).unwrap();
    let details: Vec<_> = entries.iter().map(|e| e.body_preview.as_str()).collect();

    assert_eq!(details, vec!["entry-2", "entry-3", "entry-4", "entry-5"]);
}

#[test]
fn flash_log_skips_corrupt_records() {
    let storage = MemFlashLogStorage::new(FLASH_LOG_RECORD_SIZE * 4, FLASH_LOG_RECORD_SIZE * 2);
    let mut ring = FlashLogRing::mount(storage).unwrap();

    ring.append(&event("good", "first")).unwrap();
    ring.append(&event("bad", "second")).unwrap();

    let mut storage = ring.into_storage();
    storage.corrupt_byte(FLASH_LOG_RECORD_SIZE + 20);

    let mut ring = FlashLogRing::mount(storage).unwrap();
    let entries = ring.last_n(4).unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].sender, "good");
}

#[test]
fn flash_log_truncates_oversized_entries_instead_of_dropping_them() {
    let storage = MemFlashLogStorage::new(FLASH_LOG_RECORD_SIZE * 4, FLASH_LOG_RECORD_SIZE * 2);
    let mut ring = FlashLogRing::mount(storage).unwrap();
    let long_detail = "network error ".repeat(40);

    ring.append(&event("ota", &long_detail)).unwrap();

    let storage = ring.into_storage();
    let mut ring = FlashLogRing::mount(storage).unwrap();
    let entries = ring.last_n(4).unwrap();

    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].sender, "ota");
    assert!(entries[0]
        .body_preview
        .starts_with("network error network error"));
    assert!(entries[0].body_preview.len() < long_detail.len());
}
