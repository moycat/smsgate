//! Heap allocation guardrails for outbound SMS sending.

#[path = "support/alloc_counter.rs"]
mod alloc_counter;

use smsgate::sms::sender::SmsSender;
use smsgate::testing::mocks::ScriptedModem;

#[test]
fn drain_once_avoids_cloning_message_body() {
    let mut sender = SmsSender::new();
    sender.enqueue("+15551234567".to_string(), "A".repeat(200));
    let mut modem = ScriptedModem::new();

    let (outcome, allocations) = alloc_counter::count_allocations(|| sender.drain_once(&mut modem));

    assert!(outcome.attempted());
    assert_eq!(modem.sent_pdus.len(), 2);
    assert!(
        allocations <= 32,
        "drain_once allocated {allocations} times; expected PDU construction without cloning the queued body"
    );
}
