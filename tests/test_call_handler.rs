//! CallHandler state machine tests.

use smsgate::bridge::call_handler::CallHandler;
use smsgate::im::MessageSink;
use smsgate::log_ring::LogKind;
use smsgate::testing::mocks::{RecordingMessenger, ScriptedModem};

fn make_handler() -> (CallHandler, ScriptedModem, RecordingMessenger) {
    (
        CallHandler::new(),
        ScriptedModem::new(),
        RecordingMessenger::new(),
    )
}

fn handle_urc(
    handler: &mut CallHandler,
    line: &str,
    modem: &mut ScriptedModem,
    messenger: &mut RecordingMessenger,
) {
    if let Some(notification) = handler.handle_urc_deferred(line, modem) {
        messenger.send_message(&notification.text).unwrap();
    }
}

#[test]
fn ring_then_clip_notifies_im() {
    let (mut h, mut modem, mut messenger) = make_handler();
    handle_urc(&mut h, "RING", &mut modem, &mut messenger);
    handle_urc(
        &mut h,
        r#"+CLIP: "+8613800138000",145,"",,"",0"#,
        &mut modem,
        &mut messenger,
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
fn ring_then_clip_produces_call_log_event() {
    let (mut h, mut modem, _messenger) = make_handler();

    assert!(h.handle_urc_deferred("RING", &mut modem).is_none());
    let notification = h
        .handle_urc_deferred(r#"+CLIP: "+8613800138000",145,"",,"",0"#, &mut modem)
        .expect("call notification");
    let event = notification.log_event();

    assert_eq!(event.kind, LogKind::Call);
    assert!(
        event.sender.contains("+86 138-0013-8000") || event.sender.contains("+8613800138000"),
        "no phone in log sender: {}",
        event.sender
    );
    assert_eq!(event.body_preview, "incoming call; hung up");
    assert!(event.forwarded);
}

#[test]
fn ring_without_clip_commits_as_unknown() {
    let (mut h, mut modem, mut messenger) = make_handler();
    handle_urc(&mut h, "RING", &mut modem, &mut messenger);

    // We cannot advance Instant directly here, so an empty CLIP exercises the
    // same commit path with an unknown number.
    handle_urc(
        &mut h,
        r#"+CLIP: "",128,"",,"",0"#,
        &mut modem,
        &mut messenger,
    );

    assert_eq!(modem.hang_up_count, 1);
    assert_eq!(messenger.sent_count(), 1);
    assert!(messenger.contains_sent("unknown caller") || messenger.contains_sent("Incoming call"));
}

#[test]
fn duplicate_ring_in_cooldown_suppressed() {
    let (mut h, mut modem, mut messenger) = make_handler();
    // First call
    handle_urc(&mut h, "RING", &mut modem, &mut messenger);
    handle_urc(
        &mut h,
        r#"+CLIP: "+1",129,"",,"",0"#,
        &mut modem,
        &mut messenger,
    );
    assert_eq!(modem.hang_up_count, 1);

    // Second RING arrives while in cooldown
    handle_urc(&mut h, "RING", &mut modem, &mut messenger);
    // Still only 1 hang-up and 1 notification (cooldown suppresses it)
    assert_eq!(modem.hang_up_count, 1);
    assert_eq!(messenger.sent_count(), 1);
}

#[test]
fn no_carrier_after_hangup_keeps_cooldown() {
    let (mut h, mut modem, mut messenger) = make_handler();

    handle_urc(&mut h, "RING", &mut modem, &mut messenger);
    handle_urc(
        &mut h,
        r#"+CLIP: "+8613800138000",145,"",,"",0"#,
        &mut modem,
        &mut messenger,
    );
    handle_urc(&mut h, "NO CARRIER", &mut modem, &mut messenger);

    handle_urc(&mut h, "RING", &mut modem, &mut messenger);
    handle_urc(
        &mut h,
        r#"+CLIP: "+8613800138000",145,"",,"",0"#,
        &mut modem,
        &mut messenger,
    );

    assert_eq!(modem.hang_up_count, 1);
    assert_eq!(messenger.sent_count(), 1);
}

#[test]
fn no_carrier_resets_to_idle() {
    let (mut h, mut modem, mut messenger) = make_handler();
    handle_urc(&mut h, "RING", &mut modem, &mut messenger);
    handle_urc(&mut h, "NO CARRIER", &mut modem, &mut messenger);
    // A new RING after NO CARRIER should be handled normally
    handle_urc(&mut h, "RING", &mut modem, &mut messenger);
    handle_urc(
        &mut h,
        r#"+CLIP: "+1",129,"",,"",0"#,
        &mut modem,
        &mut messenger,
    );
    assert_eq!(modem.hang_up_count, 1);
    assert_eq!(messenger.sent_count(), 1);
}

#[test]
fn second_ring_while_ringing_is_noop() {
    // A second RING while already in Ringing state must not duplicate state or hang-up.
    let (mut h, mut modem, mut messenger) = make_handler();
    handle_urc(&mut h, "RING", &mut modem, &mut messenger);
    handle_urc(&mut h, "RING", &mut modem, &mut messenger); // second RING — ignored
    handle_urc(
        &mut h,
        r#"+CLIP: "+1",129,"",,"",0"#,
        &mut modem,
        &mut messenger,
    );
    // Only one hang-up and one notification
    assert_eq!(modem.hang_up_count, 1);
    assert_eq!(messenger.sent_count(), 1);
}

#[test]
fn no_carrier_while_idle_is_safe() {
    // NO CARRIER with no preceding RING must not panic or corrupt state.
    let (mut h, mut modem, mut messenger) = make_handler();
    handle_urc(&mut h, "NO CARRIER", &mut modem, &mut messenger);
    // Subsequent RING should still work
    handle_urc(&mut h, "RING", &mut modem, &mut messenger);
    handle_urc(
        &mut h,
        r#"+CLIP: "+1",129,"",,"",0"#,
        &mut modem,
        &mut messenger,
    );
    assert_eq!(modem.hang_up_count, 1);
    assert_eq!(messenger.sent_count(), 1);
}

#[test]
fn clip_without_prior_ring_is_ignored() {
    // CLIP arriving without RING must not cause a spurious hang-up or notification.
    let (mut h, mut modem, mut messenger) = make_handler();
    handle_urc(
        &mut h,
        r#"+CLIP: "+1",129,"",,"",0"#,
        &mut modem,
        &mut messenger,
    );
    assert_eq!(modem.hang_up_count, 0);
    assert_eq!(messenger.sent_count(), 0);
}
