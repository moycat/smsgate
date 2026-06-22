//! LogRing tests.

use smsgate::log_ring::{LogEntry, LogRing};

fn entry(sender: &str) -> LogEntry {
    LogEntry::sms(
        sender.to_string(),
        "body".to_string(),
        "ts".to_string(),
        true,
    )
}

#[test]
fn empty_ring_returns_no_entries() {
    let r = LogRing::new();
    assert!(r.last_n(5).is_empty());
    assert!(r.page(0, 16).unwrap().is_empty());
    assert!(r.is_empty());
    assert_eq!(r.len(), 0);
}

#[test]
fn single_push_retrieval() {
    let mut r = LogRing::new();
    r.push(entry("A"));
    assert_eq!(r.len(), 1);
    assert_eq!(r.last_n(1)[0].sender, "A");
}

#[test]
fn last_n_capped_at_available() {
    let mut r = LogRing::new();
    r.push(entry("A"));
    r.push(entry("B"));
    let result = r.last_n(10);
    assert_eq!(result.len(), 2);
}

#[test]
fn last_n_returns_most_recent_last() {
    let mut r = LogRing::new();
    for c in ['A', 'B', 'C'] {
        r.push(entry(&c.to_string()));
    }
    let result = r.last_n(2);
    assert_eq!(result[0].sender, "B");
    assert_eq!(result[1].sender, "C");
}

#[test]
fn ring_reads_beyond_old_ram_cache_limit() {
    let mut r = LogRing::new();
    for i in 0..60 {
        r.push(entry(&i.to_string()));
    }
    assert_eq!(r.len(), 60);
    let entries = r.last_n(60);
    assert_eq!(entries[0].sender, "0");
    assert_eq!(entries[59].sender, "59");
}

#[test]
fn page_uses_offset_from_newest_entries() {
    let mut r = LogRing::new();
    for i in 0..20 {
        r.push(entry(&i.to_string()));
    }

    let latest = r.page(0, 16).unwrap();
    assert_eq!(latest.len(), 16);
    assert_eq!(latest[0].sender, "19");
    assert_eq!(latest[15].sender, "4");

    let previous = r.page(16, 16).unwrap();
    assert_eq!(previous.len(), 4);
    assert_eq!(previous[0].sender, "3");
    assert_eq!(previous[3].sender, "0");

    assert!(r.page(32, 16).unwrap().is_empty());
}
