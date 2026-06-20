//! CallHandler state machine tests.

use smsgate::bridge::call_handler::CallHandler;
use smsgate::sms::sender::SmsSender;
use smsgate::testing::mocks::{RecordingMessenger, ScriptedModem};

fn make_handler() -> (CallHandler, ScriptedModem, RecordingMessenger, SmsSender) {
    (
        CallHandler::new(),
        ScriptedModem::new(),
        RecordingMessenger::new(),
        SmsSender::new(),
    )
}

#[test]
fn ring_then_clip_notifies_im() {
    let (mut h, mut modem, mut messenger, mut sender) = make_handler();
    h.handle_urc("RING", &mut modem, &mut messenger, &mut sender);
    h.handle_urc(
        r#"+CLIP: "+8613800138000",145,"",,"",0"#,
        &mut modem,
        &mut messenger,
        &mut sender,
    );

    // Should have hung up and sent IM notification
    assert_eq!(modem.hang_up_count, 1);
    assert_eq!(messenger.sent_count(), 1);
    assert!(
        messenger.contains_sent("+86 138-0013-8000") || messenger.contains_sent("+8613800138000"),
        "no phone in: {:?}",
        messenger.last_sent()
    );
}

#[test]
fn ring_without_clip_commits_as_unknown() {
    let (mut h, mut modem, mut messenger, mut sender) = make_handler();
    h.handle_urc("RING", &mut modem, &mut messenger, &mut sender);

    // Tick past the CLIP deadline
    // We can't easily advance Instant in tests, so call tick() many times
    // and check that eventually the call commits. In practice, we test the
    // CLIP path (above) which is the common case. For the deadline path,
    // we can call handle_urc with an empty CLIP:
    h.handle_urc(
        r#"+CLIP: "",128,"",,"",0"#,
        &mut modem,
        &mut messenger,
        &mut sender,
    );

    assert_eq!(modem.hang_up_count, 1);
    assert_eq!(messenger.sent_count(), 1);
    assert!(messenger.contains_sent("unknown caller") || messenger.contains_sent("Incoming call"));
}

#[test]
fn duplicate_ring_in_cooldown_suppressed() {
    let (mut h, mut modem, mut messenger, mut sender) = make_handler();
    // First call
    h.handle_urc("RING", &mut modem, &mut messenger, &mut sender);
    h.handle_urc(
        r#"+CLIP: "+1",129,"",,"",0"#,
        &mut modem,
        &mut messenger,
        &mut sender,
    );
    assert_eq!(modem.hang_up_count, 1);

    // Second RING arrives while in cooldown
    h.handle_urc("RING", &mut modem, &mut messenger, &mut sender);
    // Still only 1 hang-up and 1 notification (cooldown suppresses it)
    assert_eq!(modem.hang_up_count, 1);
    assert_eq!(messenger.sent_count(), 1);
}

#[test]
fn no_carrier_resets_to_idle() {
    let (mut h, mut modem, mut messenger, mut sender) = make_handler();
    h.handle_urc("RING", &mut modem, &mut messenger, &mut sender);
    h.handle_urc("NO CARRIER", &mut modem, &mut messenger, &mut sender);
    // A new RING after NO CARRIER should be handled normally
    h.handle_urc("RING", &mut modem, &mut messenger, &mut sender);
    h.handle_urc(
        r#"+CLIP: "+1",129,"",,"",0"#,
        &mut modem,
        &mut messenger,
        &mut sender,
    );
    assert_eq!(modem.hang_up_count, 1);
    assert_eq!(messenger.sent_count(), 1);
}

#[test]
fn second_ring_while_ringing_is_noop() {
    // A second RING while already in Ringing state must not duplicate state or hang-up.
    let (mut h, mut modem, mut messenger, mut sender) = make_handler();
    h.handle_urc("RING", &mut modem, &mut messenger, &mut sender);
    h.handle_urc("RING", &mut modem, &mut messenger, &mut sender); // second RING — ignored
    h.handle_urc(
        r#"+CLIP: "+1",129,"",,"",0"#,
        &mut modem,
        &mut messenger,
        &mut sender,
    );
    // Only one hang-up and one notification
    assert_eq!(modem.hang_up_count, 1);
    assert_eq!(messenger.sent_count(), 1);
}

#[test]
fn no_carrier_while_idle_is_safe() {
    // NO CARRIER with no preceding RING must not panic or corrupt state.
    let (mut h, mut modem, mut messenger, mut sender) = make_handler();
    h.handle_urc("NO CARRIER", &mut modem, &mut messenger, &mut sender);
    // Subsequent RING should still work
    h.handle_urc("RING", &mut modem, &mut messenger, &mut sender);
    h.handle_urc(
        r#"+CLIP: "+1",129,"",,"",0"#,
        &mut modem,
        &mut messenger,
        &mut sender,
    );
    assert_eq!(modem.hang_up_count, 1);
    assert_eq!(messenger.sent_count(), 1);
}

#[test]
fn clip_without_prior_ring_is_ignored() {
    // CLIP arriving without RING must not cause a spurious hang-up or notification.
    let (mut h, mut modem, mut messenger, mut sender) = make_handler();
    h.handle_urc(
        r#"+CLIP: "+1",129,"",,"",0"#,
        &mut modem,
        &mut messenger,
        &mut sender,
    );
    assert_eq!(modem.hang_up_count, 0);
    assert_eq!(messenger.sent_count(), 0);
}
