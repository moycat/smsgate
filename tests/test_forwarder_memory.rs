//! Heap allocation guardrails for SMS forwarding helpers.

#[path = "support/alloc_counter.rs"]
mod alloc_counter;

use smsgate::bridge::forwarder::sms_log_preview_for_test;
use smsgate::sms::SmsMessage;

#[test]
fn sms_log_preview_allocations_are_bounded() {
    let sms = SmsMessage {
        sender: "+15551234567".to_string(),
        body: "hello <memory> ".repeat(16),
        timestamp: "26/07/04,22:29:25-28".to_string(),
        slot: 0,
    };

    let (preview, allocations) =
        alloc_counter::count_allocations(|| sms_log_preview_for_test(&sms));

    let prefix = "sms_time=2026-07-04T22:29:25+08:00 ";
    assert!(preview.starts_with(prefix));
    assert_eq!(preview[prefix.len()..].chars().count(), 160);
    assert!(
        allocations <= 1,
        "SMS log preview allocated {allocations} times; expected one output allocation"
    );
}
