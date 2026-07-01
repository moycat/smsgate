//! Tests for Telegram health logging helpers.

use smsgate::im::telegram::{poll_error_log_detail, should_log_poll_error};
use std::time::Duration;

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

#[test]
fn send_retry_delay_limits_attempts_to_one_per_30_seconds() {
    assert_eq!(
        smsgate::im::telegram::send_retry_delay_after(Duration::from_secs(5)),
        Duration::from_secs(25)
    );
    assert_eq!(
        smsgate::im::telegram::send_retry_delay_after(Duration::from_secs(30)),
        Duration::ZERO
    );
    assert_eq!(
        smsgate::im::telegram::send_retry_delay_after(Duration::from_secs(45)),
        Duration::ZERO
    );
}

#[test]
fn telegram_watchdogs_restart_after_five_minutes() {
    assert!(!smsgate::im::telegram::should_restart_after_stale_poll(
        Duration::from_secs(299)
    ));
    assert!(smsgate::im::telegram::should_restart_after_stale_poll(
        Duration::from_secs(300)
    ));
    assert!(!smsgate::im::telegram::should_restart_after_send_retry(
        Duration::from_secs(299)
    ));
    assert!(smsgate::im::telegram::should_restart_after_send_retry(
        Duration::from_secs(300)
    ));
}
