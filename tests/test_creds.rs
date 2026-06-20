//! Tests for RuntimeCreds (platform-independent logic only).

use smsgate::creds::RuntimeCreds;

fn creds(token: &str, chat_id: i64) -> RuntimeCreds {
    RuntimeCreds {
        wifi_ssid: String::new(),
        wifi_pass: String::new(),
        bot_token: token.to_string(),
        chat_id,
        apn: String::new(),
        apn_user: String::new(),
        apn_pass: String::new(),
    }
}

#[test]
fn provisioned_requires_both_token_and_chat_id() {
    assert!(creds("123:abc", 9999).is_provisioned());
}

#[test]
fn empty_token_not_provisioned() {
    assert!(!creds("", 9999).is_provisioned());
}

#[test]
fn zero_chat_id_not_provisioned() {
    assert!(!creds("123:abc", 0).is_provisioned());
}

#[test]
fn negative_chat_id_is_provisioned() {
    // Telegram channel IDs are negative; they must be accepted.
    assert!(creds("123:abc", -100123456789).is_provisioned());
}

#[test]
fn empty_token_and_zero_chat_id_not_provisioned() {
    assert!(!creds("", 0).is_provisioned());
}
