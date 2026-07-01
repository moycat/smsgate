//! End-to-end Scenario tests.

use smsgate::bridge::forwarder::{add_to_blocklist, forward_sms};
use smsgate::bridge::reply_router::ReplyRouter;
use smsgate::log_ring::LogRing;
use smsgate::persist::{keys, mem::MemStore, save_bool};
use smsgate::sms::{codec::build_sms_submit_pdus, SmsMessage};
use smsgate::testing::mocks::RecordingMessenger;
use smsgate::testing::{pdu, Scenario};

const TEST_LOG_TS: &str = "2026-04-10T20:00:00Z";

// A known-good GSM-7 single-part DELIVER PDU for "+8613800138000", "Hello"
// We build it by hand-coding a minimal SMS-DELIVER:
// SCA=00, FO=04 (MTI=00 DELIVER, no flags), OA=0D918136001380F0 (+8613800138000)
// PID=00, DCS=00 (GSM7), SCTS=62400110000000 (some timestamp)
// UDL=05, UD=C8329BFD06 (packed "Hello")
const HELLO_PDU: &str = "00040D91683108108300F0000062400110000000 05C8329BFD06";

#[test]
fn scenario_sms_forward_basic() {
    Scenario::new("SMS forward")
        .with_pdu(&pdu(HELLO_PDU))
        .expect_im_sent_contains("Hello")
        .expect_im_sent_count(1)
        .run();
}

#[test]
fn scenario_blocked_number_not_forwarded() {
    let mut store = MemStore::new();
    add_to_blocklist("+8613800138000", &mut store).unwrap();

    let mut messenger = RecordingMessenger::new();
    let mut router = ReplyRouter::new();
    let mut log = LogRing::new();

    let sms = SmsMessage {
        sender: "+8613800138000".to_string(),
        body: "spam".to_string(),
        timestamp: "".to_string(),
        slot: 0,
    };
    forward_sms(
        &sms,
        &mut messenger,
        &mut router,
        &mut log,
        &mut store,
        TEST_LOG_TS,
    );

    assert_eq!(
        messenger.sent_count(),
        0,
        "blocked number should produce no IM messages"
    );
}

#[test]
fn scenario_paused_forwarding() {
    let mut store = MemStore::new();
    save_bool(&mut store, keys::FWD_ENABLED, false).unwrap();

    let mut messenger = RecordingMessenger::new();
    let mut router = ReplyRouter::new();
    let mut log = LogRing::new();

    forward_sms(
        &SmsMessage {
            sender: "+1".to_string(),
            body: "test".to_string(),
            timestamp: "".to_string(),
            slot: 0,
        },
        &mut messenger,
        &mut router,
        &mut log,
        &mut store,
        TEST_LOG_TS,
    );

    assert_eq!(
        messenger.sent_count(),
        0,
        "paused forwarding should produce no IM messages"
    );
}

#[test]
fn scenario_phone_number_in_forwarded_message() {
    Scenario::new("Phone number display")
        .with_pdu(&pdu(HELLO_PDU))
        .expect_im_sent_contains("+86") // formatted as +86 xxx-xxxx-xxxx or similar
        .run();
}

#[test]
fn outbound_sms_sends_correct_pdu() {
    // Verify that PDU encoding produces valid, non-empty hex
    let pdus = build_sms_submit_pdus("+8613800138000", "Test message", 10, false);
    assert_eq!(pdus.len(), 1);
    let hex = &pdus[0].hex;
    // PDU should start with 00 (empty SCA)
    assert!(
        hex.starts_with("00"),
        "PDU should start with 00 (empty SCA), got: {}",
        &hex[..4.min(hex.len())]
    );
    // TPDU length should match hex minus SCA byte
    let total_bytes = hex.len() / 2;
    assert_eq!(total_bytes as u8, pdus[0].tpdu_len + 1);
}
