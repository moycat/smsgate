//! SIM PIN unlock tests.
//!
//! Run with: cargo test --no-default-features --features testing --test test_sim_pin

use smsgate::modem::a76xx::sim::ensure_sim_unlocked;
use smsgate::testing::mocks::ScriptedModem;

#[test]
fn ready_sim_does_not_send_pin() {
    let mut modem = ScriptedModem::new().expect("+CPIN?", "+CPIN: READY", true);

    ensure_sim_unlocked(&mut modem, "").unwrap();

    modem.check_consumed();
}

#[test]
fn locked_sim_sends_configured_pin_and_waits_until_ready() {
    let mut modem = ScriptedModem::new()
        .expect("+CPIN?", "+CPIN: SIM PIN", true)
        .expect("+CPIN=\"1234\"", "", true)
        .expect("+CPIN?", "+CPIN: READY", true);

    ensure_sim_unlocked(&mut modem, "1234").unwrap();

    modem.check_consumed();
}

#[test]
fn locked_sim_without_configured_pin_fails() {
    let mut modem = ScriptedModem::new().expect("+CPIN?", "+CPIN: SIM PIN", true);

    let err = ensure_sim_unlocked(&mut modem, "").unwrap_err();

    assert!(err.to_string().contains("SIM requires PIN"));
    modem.check_consumed();
}

#[test]
fn invalid_pin_is_rejected_before_sending_at_command() {
    let mut modem = ScriptedModem::new();

    let err = ensure_sim_unlocked(&mut modem, "12ab").unwrap_err();

    assert!(err.to_string().contains("SIM PIN must be 4-8 digits"));
    modem.check_consumed();
}

#[test]
fn puk_locked_sim_fails_without_trying_pin() {
    let mut modem = ScriptedModem::new().expect("+CPIN?", "+CPIN: SIM PUK", true);

    let err = ensure_sim_unlocked(&mut modem, "1234").unwrap_err();

    assert!(err.to_string().contains("SIM requires PUK"));
    modem.check_consumed();
}
