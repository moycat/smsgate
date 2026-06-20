//! LogRing tests.

use smsgate::log_ring::{LogEntry, LogRing};

fn entry(sender: &str) -> LogEntry {
    LogEntry {
        sender: sender.to_string(),
        body_preview: "body".to_string(),
        timestamp: "ts".to_string(),
        forwarded: true,
    }
}

#[test]
fn empty_ring_returns_no_entries() {
    let r = LogRing::new();
    assert!(r.last_n(5).is_empty());
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
fn last_n_returns_most_recent_first() {
    let mut r = LogRing::new();
    for c in ['A', 'B', 'C'] {
        r.push(entry(&c.to_string()));
    }
    let result = r.last_n(2);
    assert_eq!(result[0].sender, "B");
    assert_eq!(result[1].sender, "C");
}

#[test]
fn ring_evicts_oldest_when_full() {
    let mut r = LogRing::new();
    for i in 0..51 {
        r.push(entry(&i.to_string()));
    }
    assert_eq!(r.len(), 50); // capacity is 50
                             // Oldest (0) evicted; newest (50) is present
    let entries = r.last_n(50);
    assert_eq!(entries[0].sender, "1");
    assert_eq!(entries[49].sender, "50");
}
