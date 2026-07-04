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

#[test]
fn runtime_config_keeps_loaded_values_when_compiled_config_is_not_applied() {
    let loaded = RuntimeCreds {
        wifi_ssid: "nvs-wifi".to_string(),
        wifi_pass: "nvs-pass".to_string(),
        bot_token: "nvs-token".to_string(),
        chat_id: 42,
        apn: "nvs-apn".to_string(),
        apn_user: "nvs-user".to_string(),
        apn_pass: "nvs-apn-pass".to_string(),
    };

    let resolved = RuntimeCreds::resolve_compiled_config(loaded.clone(), false);

    assert_eq!(resolved.wifi_ssid, loaded.wifi_ssid);
    assert_eq!(resolved.wifi_pass, loaded.wifi_pass);
    assert_eq!(resolved.bot_token, loaded.bot_token);
    assert_eq!(resolved.chat_id, loaded.chat_id);
    assert_eq!(resolved.apn, loaded.apn);
    assert_eq!(resolved.apn_user, loaded.apn_user);
    assert_eq!(resolved.apn_pass, loaded.apn_pass);
}

#[test]
fn runtime_config_uses_compiled_defaults_when_compiled_config_is_applied() {
    let loaded = RuntimeCreds {
        wifi_ssid: "nvs-wifi".to_string(),
        wifi_pass: "nvs-pass".to_string(),
        bot_token: "nvs-token".to_string(),
        chat_id: 42,
        apn: "nvs-apn".to_string(),
        apn_user: "nvs-user".to_string(),
        apn_pass: "nvs-apn-pass".to_string(),
    };

    let resolved = RuntimeCreds::resolve_compiled_config(loaded, true);
    let compiled = RuntimeCreds::default();

    assert_eq!(resolved.wifi_ssid, compiled.wifi_ssid);
    assert_eq!(resolved.wifi_pass, compiled.wifi_pass);
    assert_eq!(resolved.bot_token, compiled.bot_token);
    assert_eq!(resolved.chat_id, compiled.chat_id);
    assert_eq!(resolved.apn, compiled.apn);
    assert_eq!(resolved.apn_user, compiled.apn_user);
    assert_eq!(resolved.apn_pass, compiled.apn_pass);
}
