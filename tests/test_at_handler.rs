//! Host-side checks for the hidden AT diagnostic command.

use smsgate::bridge::at_handler::{execute_at_command, parse_hidden_at_command, AtRequestError};
use smsgate::testing::mocks::ScriptedModem;

#[test]
fn hidden_at_command_accepts_one_at_line() {
    assert_eq!(parse_hidden_at_command("/at AT+CSQ"), Some(Ok("+CSQ")));
    assert_eq!(
        parse_hidden_at_command("/at@MoySMSBot at+CREG?"),
        Some(Ok("+CREG?"))
    );
    assert_eq!(parse_hidden_at_command("/at AT"), Some(Ok("")));
    assert_eq!(
        parse_hidden_at_command("/at AT+SIMCOMATI"),
        Some(Ok("+SIMCOMATI"))
    );
    assert!(parse_hidden_at_command("/atom AT+CSQ").is_none());
    assert!(parse_hidden_at_command("/ota AT+CSQ").is_none());
}

#[test]
fn hidden_at_command_rejects_invalid_or_non_diagnostic_inputs() {
    assert_eq!(
        parse_hidden_at_command("/at"),
        Some(Err(AtRequestError::MissingCommand))
    );
    assert_eq!(
        parse_hidden_at_command("/at +CSQ"),
        Some(Err(AtRequestError::InvalidCommand))
    );
    assert_eq!(
        parse_hidden_at_command("/at AT+CSQ\nAT+CMGD=1"),
        Some(Err(AtRequestError::InvalidCommand))
    );
    assert_eq!(
        parse_hidden_at_command("/at AT+CSQ;AT+CMGD=1"),
        Some(Err(AtRequestError::InvalidCommand))
    );
    assert_eq!(
        parse_hidden_at_command("/at AT+CMGS=10"),
        Some(Err(AtRequestError::UnsupportedCommand))
    );
    assert_eq!(
        parse_hidden_at_command("/at AT+CGATT=1"),
        Some(Err(AtRequestError::UnsupportedCommand))
    );
    assert_eq!(
        parse_hidden_at_command("/at AT+HTTPINIT"),
        Some(Err(AtRequestError::UnsupportedCommand))
    );
    assert_eq!(
        parse_hidden_at_command(&format!("/at AT+{}", "A".repeat(128))),
        Some(Err(AtRequestError::CommandTooLong))
    );
}

#[test]
fn at_command_returns_body_and_terminal_result() {
    let mut modem = ScriptedModem::new().expect("+CREG?", "+CREG: 0,1", true);
    let reply = execute_at_command("+CREG?", &mut modem);
    modem.check_consumed();
    assert!(reply.succeeded);
    assert_eq!(reply.text, "+CREG: 0,1\nOK");
}

#[test]
fn at_command_returns_modem_error() {
    let mut modem = ScriptedModem::new().expect("+CREG?", "+CME ERROR: 10", false);
    let reply = execute_at_command("+CREG?", &mut modem);
    modem.check_consumed();
    assert!(!reply.succeeded);
    assert_eq!(reply.text, "+CME ERROR: 10");
}

#[test]
fn at_command_bounds_large_telegram_reply() {
    let mut modem = ScriptedModem::new().expect("+TEST", &"A".repeat(5000), true);
    let reply = execute_at_command("+TEST", &mut modem);
    modem.check_consumed();
    assert!(reply.succeeded);
    assert!(reply.text.len() < 4096);
    assert!(reply.text.contains("[response truncated]"));
    assert!(reply.text.ends_with("\nOK"));
}
