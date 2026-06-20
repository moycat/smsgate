//! Tests for Telegram request construction.
//!
//! Run with: cargo test --no-default-features --features testing --test test_telegram_request

use smsgate::im::telegram::build_get_updates_body;

#[test]
fn get_updates_body_uses_embedded_update_limit() {
    let body = build_get_updates_body(42, 300);

    assert!(body.contains(r#""offset":42"#));
    assert!(body.contains(r#""timeout":30"#));
    assert!(body.contains(r#""limit":10"#));
    assert!(body.contains(r#""allowed_updates":["message"]"#));
}

#[test]
fn get_updates_timeout_has_lower_bound() {
    let body = build_get_updates_body(1, 0);

    assert!(body.contains(r#""timeout":1"#));
}
