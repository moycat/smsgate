//! Tests for Telegram health logging helpers.

use smsgate::im::telegram::{poll_error_log_detail, should_log_poll_error};

#[test]
fn poll_errors_log_first_error_and_periodic_summaries() {
    let logged: Vec<u16> = (1..=25).filter(|n| should_log_poll_error(*n)).collect();

    assert_eq!(logged, vec![1, 12, 24]);
}

#[test]
fn poll_error_log_detail_includes_count_and_error() {
    let detail = poll_error_log_detail(12, "TLS timeout");

    assert_eq!(detail, "poll error x12: TLS timeout");
}
