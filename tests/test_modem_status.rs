use smsgate::modem::{
    cnmi_store_notifications_enabled, creg_registered, creg_registration_status, ModemPort,
    CSQ_UNKNOWN,
};
use smsgate::testing::mocks::ScriptedModem;

#[test]
fn registration_requires_creg_stat_field() {
    assert!(creg_registered("+CREG: 0,1"));
    assert!(creg_registered("+CREG: 2,5,\"abcd\""));
    assert!(!creg_registered("+CGEV: ME PDN ACT 8,1"));
    assert!(!creg_registered("+CREG: 0,0\n+CGEV: ME PDN ACT 8,1"));
    assert_eq!(creg_registration_status("+CREG: 0,0"), Some(false));
    assert_eq!(creg_registration_status("+CREG: 0,1"), Some(true));
    assert_eq!(creg_registration_status("+CREG: broken"), None);
}

#[test]
fn cnmi_health_requires_store_notification_configuration() {
    assert!(cnmi_store_notifications_enabled("+CNMI: 2,1,0,0,0"));
    assert!(cnmi_store_notifications_enabled(
        "+CGEV: ME PDN ACT 8,0\n+CNMI: 2, 1, 0, 0, 0"
    ));
    assert!(!cnmi_store_notifications_enabled("+CNMI: 2,2,0,0,0"));
    assert!(!cnmi_store_notifications_enabled("+CGEV: ME PDN ACT 8,0"));
}

#[test]
fn status_parses_expected_lines_after_unrelated_modem_events() {
    let mut modem = ScriptedModem::new()
        .expect("+CSQ", "+CGEV: ME PDN ACT 8,0\n+CSQ: 13,0", true)
        .expect(
            "+COPS?",
            "+CGEV: ME PDN DEACT 8\n+COPS: 0,2,\"310260\",7",
            true,
        )
        .expect("+CREG?", "+CREG: 0,1", true);

    let status = modem.update_status();
    assert_eq!(status.csq, 13);
    assert_eq!(status.csq_error, None);
    assert_eq!(status.operator, "310260");
    assert!(status.registered);
    assert_eq!(status.registration_error, None);
    modem.check_consumed();
}

#[test]
fn malformed_signal_remains_unknown() {
    let mut modem = ScriptedModem::new()
        .expect("+CSQ", "+CSQ: invalid,0", true)
        .expect("+COPS?", "", true)
        .expect("+CREG?", "+CREG: 0,0", true);
    let status = modem.update_status();
    assert_eq!(status.csq, CSQ_UNKNOWN);
    assert_eq!(status.csq_error.as_deref(), Some("invalid +CSQ response"));
    assert!(!status.registered);
    modem.check_consumed();
}

#[test]
fn malformed_registration_is_unknown_not_unregistered() {
    let mut modem = ScriptedModem::new()
        .expect("+CSQ", "+CSQ: 13,0", true)
        .expect("+COPS?", "", true)
        .expect("+CREG?", "garbled", true);
    let status = modem.update_status();
    assert!(!status.registered);
    assert_eq!(
        status.registration_error.as_deref(),
        Some("invalid +CREG response")
    );
    modem.check_consumed();
}

#[test]
fn explicit_unknown_signal_differs_from_query_failure() {
    let mut modem = ScriptedModem::new()
        .expect("+CSQ", "+CSQ: 99,99", true)
        .expect("+COPS?", "", true)
        .expect("+CREG?", "+CREG: 0,1", true);
    let status = modem.update_status();
    assert_eq!(status.csq, CSQ_UNKNOWN);
    assert_eq!(status.csq_error, None);
    modem.check_consumed();
}
