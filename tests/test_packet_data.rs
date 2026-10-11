//! Packet-data detachment and strict attachment readback tests.

use smsgate::modem::a76xx::data::{
    disable_packet_data, enforce_packet_data_disabled, PacketDataError, PacketDataOutcome,
};
use smsgate::modem::a76xx::A76xxModem;
use smsgate::modem::{AtTransport, ModemError};
use smsgate::testing::mocks::{MockUart, ScriptedModem};

#[test]
fn already_detached_requires_no_state_change() {
    let mut modem = ScriptedModem::new().expect("+CGATT?", "+CGATT: 0", true);

    assert_eq!(
        enforce_packet_data_disabled(&mut modem).unwrap(),
        PacketDataOutcome::AlreadyDetached
    );
    modem.check_consumed();
}

#[test]
fn attached_modem_is_detached_and_read_back() {
    let mut modem = ScriptedModem::new()
        .expect("+CGATT?", "+CGATT: 1", true)
        .expect("+CGATT=0", "", true)
        .expect("+CGATT?", "+CGATT: 0", true);

    assert_eq!(
        enforce_packet_data_disabled(&mut modem).unwrap(),
        PacketDataOutcome::Detached
    );
    modem.check_consumed();
}

#[test]
fn boot_disable_always_detaches_then_verifies() {
    let mut modem =
        ScriptedModem::new()
            .expect("+CGATT=0", "", true)
            .expect("+CGATT?", "+CGATT: 0", true);

    disable_packet_data(&mut modem).unwrap();
    modem.check_consumed();
}

#[test]
fn reattachment_is_recovered_on_the_next_enforcement() {
    let mut modem = ScriptedModem::new()
        .expect("+CGATT?", "+CGATT: 0", true)
        .expect("+CGATT?", "+CGATT: 1", true)
        .expect("+CGATT=0", "", true)
        .expect("+CGATT?", "+CGATT: 0", true);

    assert_eq!(
        enforce_packet_data_disabled(&mut modem).unwrap(),
        PacketDataOutcome::AlreadyDetached
    );
    assert_eq!(
        enforce_packet_data_disabled(&mut modem).unwrap(),
        PacketDataOutcome::Detached
    );
    modem.check_consumed();
}

#[test]
fn rejected_detach_is_not_reported_as_disabled() {
    let mut modem = ScriptedModem::new().expect("+CGATT=0", "+CME ERROR: 3", false);

    assert!(matches!(
        disable_packet_data(&mut modem),
        Err(PacketDataError::DetachRejected)
    ));
    modem.check_consumed();
}

#[test]
fn detach_transport_error_is_preserved() {
    let mut modem = ScriptedModem::new().expect_error("+CGATT=0", ModemError::Timeout);

    assert!(matches!(
        disable_packet_data(&mut modem),
        Err(PacketDataError::DetachTransport(ModemError::Timeout))
    ));
    modem.check_consumed();
}

#[test]
fn attached_confirmation_is_not_reported_as_disabled() {
    let mut modem =
        ScriptedModem::new()
            .expect("+CGATT=0", "", true)
            .expect("+CGATT?", "+CGATT: 1", true);

    assert!(matches!(
        disable_packet_data(&mut modem),
        Err(PacketDataError::StillAttached)
    ));
    modem.check_consumed();
}

#[test]
fn rejected_confirmation_is_not_reported_as_disabled() {
    let mut modem =
        ScriptedModem::new()
            .expect("+CGATT=0", "", true)
            .expect("+CGATT?", "+CGATT: 0", false);

    assert!(matches!(
        disable_packet_data(&mut modem),
        Err(PacketDataError::QueryRejected)
    ));
    modem.check_consumed();
}

#[test]
fn confirmation_transport_error_is_preserved() {
    let mut modem = ScriptedModem::new()
        .expect("+CGATT=0", "", true)
        .expect_error("+CGATT?", ModemError::Io);

    assert!(matches!(
        disable_packet_data(&mut modem),
        Err(PacketDataError::QueryTransport(ModemError::Io))
    ));
    modem.check_consumed();
}

#[test]
fn malformed_or_ambiguous_confirmation_never_counts_as_disabled() {
    for body in [
        "",
        "OK",
        "+CEREG: 0,1",
        "+CGATT: 2",
        "+CGATT: -1",
        "+CGATT: 00",
        "+CGATT: 0,1",
        "+CGATT: 0 extra",
        "+CGATT: 0\n+CGATT: 0",
        "+CGATT: 0\n+CGATT: 1",
        "+CGATT: 0\n+CGATT missing colon",
    ] {
        let mut modem = ScriptedModem::new()
            .expect("+CGATT=0", "", true)
            .expect("+CGATT?", body, true);

        assert!(
            matches!(
                disable_packet_data(&mut modem),
                Err(PacketDataError::InvalidAttachmentResponse)
            ),
            "body must not confirm detached: {body:?}"
        );
        modem.check_consumed();
    }
}

#[test]
fn malformed_initial_query_triggers_one_detach_and_verification() {
    for body in ["", "+CGATT: 2", "+CGATT: 0\n+CGATT: 1"] {
        let mut modem = ScriptedModem::new()
            .expect("+CGATT?", body, true)
            .expect("+CGATT=0", "", true)
            .expect("+CGATT?", "+CGATT: 0", true);

        assert_eq!(
            enforce_packet_data_disabled(&mut modem).unwrap(),
            PacketDataOutcome::Detached
        );
        modem.check_consumed();
    }
}

#[test]
fn rejected_initial_query_triggers_detach_and_verification() {
    let mut modem = ScriptedModem::new()
        .expect("+CGATT?", "+CME ERROR: 3", false)
        .expect("+CGATT=0", "", true)
        .expect("+CGATT?", "+CGATT: 0", true);

    assert_eq!(
        enforce_packet_data_disabled(&mut modem).unwrap(),
        PacketDataOutcome::Detached
    );
    modem.check_consumed();
}

#[test]
fn failed_initial_query_triggers_detach_and_verification() {
    let mut modem = ScriptedModem::new()
        .expect_error("+CGATT?", ModemError::Timeout)
        .expect("+CGATT=0", "", true)
        .expect("+CGATT?", "+CGATT: 0", true);

    assert_eq!(
        enforce_packet_data_disabled(&mut modem).unwrap(),
        PacketDataOutcome::Detached
    );
    modem.check_consumed();
}

#[test]
fn unrelated_lines_and_urcs_do_not_hide_the_attachment_state() {
    let mut modem = ScriptedModem::new()
        .expect(
            "+CGATT?",
            "AT+CGATT?\r\n+CGEV: ME DETACH\r\n  +CGATT: 0  \r\n+CREG: 0,2",
            true,
        )
        .push_urc("+CMTI: \"ME\",5");

    assert_eq!(
        enforce_packet_data_disabled(&mut modem).unwrap(),
        PacketDataOutcome::AlreadyDetached
    );
    assert_eq!(modem.poll_urc().as_deref(), Some("+CMTI: \"ME\",5"));
    modem.check_consumed();
}

#[test]
fn uart_detach_and_readback_preserve_stored_and_direct_sms_notifications() {
    let mut uart = MockUart::new();
    uart.feed_line("+CMTI: \"ME\",2");
    uart.queue_response_line("AT+CGATT=0");
    uart.queue_response_line("+CMTI: \"ME\",3");
    uart.queue_response_line("+CMT: ,3");
    uart.queue_response_line("+CGEV: ME PDN DEACT 1");
    uart.queue_response_line("OK");
    uart.finish_response();
    // The direct SMS body arrives after the detach command finishes. The AT
    // parser must retain the header across the attachment readback command.
    uart.queue_response_line("001122");
    uart.queue_response_line("+CGATT: 0");
    uart.queue_response_line("OK");
    let mut modem = A76xxModem::new_test(uart);

    disable_packet_data(&mut modem).unwrap();

    assert_eq!(modem.port().inner().sent_str(), "AT+CGATT=0\rAT+CGATT?\r");
    for expected in [
        "+CMTI: \"ME\",2",
        "+CMTI: \"ME\",3",
        "+CGEV: ME PDN DEACT 1",
        "+CMT: ,3",
        "001122",
    ] {
        assert_eq!(modem.poll_urc().as_deref(), Some(expected));
    }
    assert!(modem.poll_urc().is_none());
    assert!(modem.take_diagnostics().is_empty());
}

#[test]
fn uart_rejected_detach_preserves_sms_urcs_and_never_reports_disabled() {
    let mut uart = MockUart::new();
    uart.queue_response_line("+CMTI: \"ME\",4");
    uart.queue_response_line("+CGEV: ME PDN ACT 1");
    uart.queue_response_line("+CGATT: 1");
    uart.queue_response_line("OK");
    uart.finish_response();
    uart.queue_response_line("+CMT: ,3");
    uart.queue_response_line("001122");
    uart.queue_response_line("+CME ERROR: 3");
    let mut modem = A76xxModem::new_test(uart);

    assert!(matches!(
        enforce_packet_data_disabled(&mut modem),
        Err(PacketDataError::DetachRejected)
    ));

    assert_eq!(modem.port().inner().sent_str(), "AT+CGATT?\rAT+CGATT=0\r");
    for expected in [
        "+CMTI: \"ME\",4",
        "+CGEV: ME PDN ACT 1",
        "+CMT: ,3",
        "001122",
    ] {
        assert_eq!(modem.poll_urc().as_deref(), Some(expected));
    }
    assert!(modem.poll_urc().is_none());
    assert!(modem.take_diagnostics().is_empty());
}
