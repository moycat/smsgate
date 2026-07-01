//! Forwarder and block-list tests.

use smsgate::bridge::forwarder::*;
use smsgate::bridge::reply_router::ReplyRouter;
use smsgate::im::MessageFormat;
use smsgate::log_ring::LogRing;
use smsgate::persist::{keys, mem::MemStore, save_bool};
use smsgate::sms::SmsMessage;
use smsgate::testing::mocks::{FailingMessenger, RecordingMessenger};

fn make_sms(sender: &str, body: &str) -> SmsMessage {
    SmsMessage {
        sender: sender.to_string(),
        body: body.to_string(),
        timestamp: "26/04/10,12:00:00+00".to_string(),
        slot: 0,
    }
}

#[test]
fn forward_sms_sends_to_im() {
    let mut store = MemStore::new();
    let mut messenger = RecordingMessenger::new();
    let mut router = ReplyRouter::new();
    let mut log = LogRing::new();

    let sms = make_sms("+8613800138000", "Hello test");
    let result = forward_sms(&sms, &mut messenger, &mut router, &mut log, &mut store);

    assert!(result.is_some());
    assert_eq!(messenger.sent_count(), 1);
    assert!(
        messenger.contains_sent("+86 138-0013-8000"),
        "should include formatted phone"
    );
    assert!(messenger.contains_sent("Hello test"));
}

#[test]
fn forward_sms_sends_sender_as_html_code() {
    let mut store = MemStore::new();
    let mut messenger = RecordingMessenger::new();
    let mut router = ReplyRouter::new();
    let mut log = LogRing::new();

    let sms = make_sms("+8613800138000", "Hello test");
    forward_sms(&sms, &mut messenger, &mut router, &mut log, &mut store);

    let sent = messenger.sent.last().expect("SMS should be forwarded");
    assert_eq!(sent.format, MessageFormat::Html);
    assert!(sent.text.contains("<code>+86 138-0013-8000</code>"));
}

#[test]
fn forward_sms_escapes_sms_body_for_html() {
    let mut store = MemStore::new();
    let mut messenger = RecordingMessenger::new();
    let mut router = ReplyRouter::new();
    let mut log = LogRing::new();

    let sms = make_sms("ACME", "2 < 3 & 5 > 4");
    forward_sms(&sms, &mut messenger, &mut router, &mut log, &mut store);

    let sent = messenger.last_sent().expect("SMS should be forwarded");
    assert!(sent.contains("2 &lt; 3 &amp; 5 &gt; 4"));
}

#[test]
fn forward_updates_log_ring() {
    let mut store = MemStore::new();
    let mut messenger = RecordingMessenger::new();
    let mut router = ReplyRouter::new();
    let mut log = LogRing::new();

    forward_sms(
        &make_sms("123", "hi"),
        &mut messenger,
        &mut router,
        &mut log,
        &mut store,
    );

    assert_eq!(log.len(), 1);
    let entries = log.last_n(1);
    let entry = &entries[0];
    assert!(entry.forwarded);
    assert_eq!(entry.sender, "123");
}

#[test]
fn sms_log_preview_keeps_up_to_single_sms_text_length() {
    let mut store = MemStore::new();
    let mut messenger = RecordingMessenger::new();
    let mut router = ReplyRouter::new();
    let mut log = LogRing::new();
    let body = "A".repeat(120);

    forward_sms(
        &make_sms("123", &body),
        &mut messenger,
        &mut router,
        &mut log,
        &mut store,
    );

    assert_eq!(log.last_n(1)[0].body_preview.len(), 120);
}

#[test]
fn blocked_number_not_forwarded() {
    let mut store = MemStore::new();
    add_to_blocklist("10086", &mut store).unwrap();

    let mut messenger = RecordingMessenger::new();
    let mut router = ReplyRouter::new();
    let mut log = LogRing::new();

    let result = forward_sms(
        &make_sms("10086", "spam"),
        &mut messenger,
        &mut router,
        &mut log,
        &mut store,
    );

    assert!(result.is_none());
    assert_eq!(messenger.sent_count(), 0);
    // Log entry added but not forwarded
    assert_eq!(log.len(), 1);
    assert!(!log.last_n(1)[0].forwarded);
}

#[test]
fn forwarding_paused_drops_message() {
    let mut store = MemStore::new();
    save_bool(&mut store, keys::FWD_ENABLED, false).unwrap();

    let mut messenger = RecordingMessenger::new();
    let mut router = ReplyRouter::new();
    let mut log = LogRing::new();

    let result = forward_sms(
        &make_sms("+1234567890", "test"),
        &mut messenger,
        &mut router,
        &mut log,
        &mut store,
    );

    assert!(result.is_none());
    assert_eq!(messenger.sent_count(), 0);
}

#[test]
fn forwarding_enabled_by_default() {
    let store = MemStore::new();
    // No fwd_enabled key → default is enabled
    let enabled = smsgate::persist::load_bool(&store, keys::FWD_ENABLED);
    assert!(enabled.is_none()); // absent → treated as enabled in forwarder
}

#[test]
fn block_list_normalized_match() {
    let mut store = MemStore::new();
    // Add with formatting
    add_to_blocklist("+44 7911-123 456", &mut store).unwrap();
    // Should match without formatting
    assert!(is_blocked("+447911123456", &store));
    // Should match with different format
    assert!(is_blocked("+44 7911 123456", &store));
}

#[test]
fn unblock_removes_number() {
    let mut store = MemStore::new();
    add_to_blocklist("10086", &mut store).unwrap();
    assert!(is_blocked("10086", &store));
    let removed = remove_from_blocklist("10086", &mut store);
    assert!(removed);
    assert!(!is_blocked("10086", &store));
}

#[test]
fn unblock_nonexistent_returns_false() {
    let mut store = MemStore::new();
    assert!(!remove_from_blocklist("99999", &mut store));
}

#[test]
fn reply_router_stores_and_retrieves() {
    let mut store = MemStore::new();
    let mut router = ReplyRouter::new();

    router.put(1001, "+8613800138000", &mut store);
    assert_eq!(router.lookup(1001), Some("+8613800138000"));
    assert_eq!(router.lookup(1002), None);
}

#[test]
fn reply_router_persists_across_reload() {
    let mut store = MemStore::new();
    let mut router = ReplyRouter::new();
    router.put(42, "+447911123456", &mut store);

    let mut router2 = ReplyRouter::new();
    router2.load(&store);
    assert_eq!(router2.lookup(42), Some("+447911123456"));
}

#[test]
fn reply_router_overwrites_old_slot() {
    let mut store = MemStore::new();
    let mut router = ReplyRouter::new();
    // IDs 1 and 201 hash to same slot (% 200)
    router.put(1, "old_phone", &mut store);
    router.put(201, "new_phone", &mut store);
    assert_eq!(router.lookup(201), Some("new_phone"));
    assert_eq!(router.lookup(1), None); // overwritten
}

#[test]
fn forward_sms_stores_reply_mapping() {
    let mut store = MemStore::new();
    let mut messenger = RecordingMessenger::new();
    let mut router = ReplyRouter::new();
    let mut log = LogRing::new();

    let sms = make_sms("+8613800138000", "please reply");
    let msg_id = forward_sms(&sms, &mut messenger, &mut router, &mut log, &mut store);

    assert!(msg_id.is_some());
    let id = msg_id.unwrap();
    assert_eq!(router.lookup(id), Some("+8613800138000"));
}

#[test]
fn block_list_suffix_match_shorter_stored() {
    // Stored: "138000" — should match "+8613800138000" (ends_with)
    let mut store = MemStore::new();
    add_to_blocklist("138000", &mut store).unwrap();
    // "+8613800138000" normalized = "+8613800138000", stored "138000"
    // nb.ends_with(&normalized) is false, normalized.ends_with(&nb) = true ("...138000" ends with "138000")
    assert!(is_blocked("+8613800138000", &store));
    // non-matching number should NOT be blocked
    assert!(!is_blocked("+8613800138001", &store));
}

#[test]
fn block_list_duplicate_add_is_idempotent() {
    let mut store = MemStore::new();
    add_to_blocklist("10086", &mut store).unwrap();
    add_to_blocklist("10086", &mut store).unwrap(); // second add should be no-op
    let list = load_blocklist(&store);
    assert_eq!(list.len(), 1, "duplicate add should not grow list");
}

#[test]
fn forwarding_paused_log_entry_not_forwarded() {
    let mut store = MemStore::new();
    save_bool(&mut store, keys::FWD_ENABLED, false).unwrap();
    let mut messenger = RecordingMessenger::new();
    let mut router = ReplyRouter::new();
    let mut log = LogRing::new();

    forward_sms(
        &make_sms("+1", "test"),
        &mut messenger,
        &mut router,
        &mut log,
        &mut store,
    );

    assert_eq!(log.len(), 1);
    assert!(
        !log.last_n(1)[0].forwarded,
        "paused message should log as not-forwarded"
    );
}

#[test]
fn messenger_failure_returns_none_and_no_router_entry() {
    let mut store = MemStore::new();
    let mut messenger = FailingMessenger;
    let mut router = ReplyRouter::new();
    let mut log = LogRing::new();

    let sms = make_sms("+8613800138000", "test");
    let result = forward_sms(&sms, &mut messenger, &mut router, &mut log, &mut store);

    assert!(result.is_none(), "failure should return None");
    // No reply mapping stored
    assert_eq!(router.lookup(1000), None);
}
