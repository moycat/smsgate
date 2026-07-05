//! Tests for Telegram health logging helpers.

use smsgate::im::{
    telegram::{poll_error_log_detail, poll_retry_after, should_log_poll_error},
    MessengerError,
};
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
fn poll_errors_request_recovery_after_first_summary() {
    let recovered: Vec<u16> = (1..=25)
        .filter(|n| smsgate::im::telegram::should_recover_after_poll_errors(*n))
        .collect();

    assert_eq!(recovered, vec![12, 24]);
}

#[test]
fn poll_retry_after_uses_structured_rate_limit_error() {
    let error = MessengerError::RateLimited {
        retry_after_secs: 7,
        description: "Too Many Requests".to_string(),
    };

    assert_eq!(poll_retry_after(&error), Some(Duration::from_secs(7)));
}

#[test]
fn poll_retry_after_parses_telegram_description_fallback() {
    let error = MessengerError::Api("Too Many Requests: retry after 5".to_string());

    assert_eq!(poll_retry_after(&error), Some(Duration::from_secs(5)));
}

#[test]
fn poll_retry_after_ignores_non_rate_limit_errors() {
    let error = MessengerError::Http("tls reconnect failed".to_string());

    assert_eq!(poll_retry_after(&error), None);
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
