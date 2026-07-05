//! Tests for Telegram request construction.
//!
//! Run with: cargo test --no-default-features --features testing --test test_telegram_request

use smsgate::im::telegram::{
    build_edit_message_text_body, build_edit_message_text_body_with_format, build_get_file_body,
    build_get_updates_body, build_send_message_body, build_send_message_body_with_format,
    build_set_my_commands_body,
};
use smsgate::im::{InlineKeyboard, InlineKeyboardButton, MessageFormat};

#[test]
fn get_updates_body_uses_embedded_update_limit() {
    let body = build_get_updates_body(42, 300);

    assert!(body.contains(r#""offset":42"#));
    assert!(body.contains(r#""timeout":30"#));
    assert!(body.contains(r#""limit":10"#));
    assert!(body.contains(r#""allowed_updates":["message","callback_query"]"#));
}

#[test]
fn get_updates_timeout_has_lower_bound() {
    let body = build_get_updates_body(1, 0);

    assert!(body.contains(r#""timeout":1"#));
}

#[test]
fn get_file_body_uses_file_id() {
    let body = build_get_file_body("BQACAgUAAxkBAAIB");

    assert_eq!(body, r#"{"file_id":"BQACAgUAAxkBAAIB"}"#);
}

#[test]
fn set_my_commands_body_escapes_command_descriptions() {
    let body =
        build_set_my_commands_body(&[("status", "device \"status\""), ("log", "tail \\ log")]);

    assert_eq!(
        body,
        r#"{"commands":[{"command":"status","description":"device \"status\""},{"command":"log","description":"tail \\ log"}]}"#
    );
}

#[test]
fn send_message_body_can_include_inline_keyboard() {
    let keyboard = InlineKeyboard::single_row(vec![InlineKeyboardButton::new("Older", "log:16")]);

    let body = build_send_message_body(123, "page", Some(&keyboard));

    assert!(body.contains(r#""chat_id":123"#));
    assert!(body.contains(r#""text":"page"#));
    assert!(body.contains(
        r#""reply_markup":{"inline_keyboard":[[{"text":"Older","callback_data":"log:16"}]]}"#
    ));
    assert!(!body.contains(r#""parse_mode""#));
}

#[test]
fn edit_message_body_can_include_inline_keyboard() {
    let keyboard = InlineKeyboard::single_row(vec![InlineKeyboardButton::new("Newer", "log:0")]);

    let body = build_edit_message_text_body(123, 456, "page", Some(&keyboard));

    assert!(body.contains(r#""chat_id":123"#));
    assert!(body.contains(r#""message_id":456"#));
    assert!(body.contains(
        r#""reply_markup":{"inline_keyboard":[[{"text":"Newer","callback_data":"log:0"}]]}"#
    ));
    assert!(!body.contains(r#""parse_mode""#));
}

#[test]
fn send_message_body_can_request_html_format() {
    let body = build_send_message_body_with_format(
        123,
        "header\n<pre>[INFO] log</pre>",
        None,
        MessageFormat::Html,
    );

    assert!(body.contains(r#""parse_mode":"HTML""#));
    assert!(body.contains(r#""text":"header\n<pre>[INFO] log</pre>""#));
}

#[test]
fn edit_message_body_can_request_html_format() {
    let body = build_edit_message_text_body_with_format(
        123,
        456,
        "<pre>page</pre>",
        None,
        MessageFormat::Html,
    );

    assert!(body.contains(r#""parse_mode":"HTML""#));
    assert!(body.contains(r#""message_id":456"#));
}
