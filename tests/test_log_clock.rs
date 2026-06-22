//! Log clock and modem network time parsing tests.

use smsgate::log_clock::{parse_cclk_time, LogClock};

#[test]
fn unsynced_clock_starts_at_year_zero_boot_time() {
    let clock = LogClock::new();

    assert_eq!(clock.timestamp(0), "0000-01-01 00:00:00");
    assert_eq!(clock.timestamp(3_661_000), "0000-01-01 01:01:01");
}

#[test]
fn cclk_parser_treats_timezone_as_quarters_of_an_hour() {
    let dt = parse_cclk_time(r#"+CCLK: "26/06/21,20:30:45+32""#).unwrap();

    assert_eq!(dt.year, 2026);
    assert_eq!(dt.month, 6);
    assert_eq!(dt.day, 21);
    assert_eq!(dt.hour, 20);
    assert_eq!(dt.minute, 30);
    assert_eq!(dt.second, 45);
    assert_eq!(dt.offset_minutes, 8 * 60);
    assert_eq!(dt.format(), "2026-06-21 20:30:45+08:00");
}

#[test]
fn cclk_parser_handles_negative_timezone_offsets() {
    let dt = parse_cclk_time(r#"+CCLK: "26/06/21,07:30:45-20""#).unwrap();

    assert_eq!(dt.offset_minutes, -5 * 60);
    assert_eq!(dt.format(), "2026-06-21 07:30:45-05:00");
}

#[test]
fn cclk_parser_rejects_factory_default_rtc_dates() {
    assert!(parse_cclk_time(r#"+CCLK: "00/01/01,00:00:00+00""#).is_none());
    assert!(parse_cclk_time(r#"+CCLK: "23/12/31,23:59:59+00""#).is_none());
}

#[test]
fn synced_clock_advances_from_network_time() {
    let mut clock = LogClock::new();
    let dt = parse_cclk_time(r#"+CCLK: "26/12/31,23:59:58+00""#).unwrap();

    clock.sync_from_network(10_000, dt);

    assert_eq!(clock.timestamp(10_000), "2026-12-31 23:59:58+00:00");
    assert_eq!(clock.timestamp(13_000), "2027-01-01 00:00:01+00:00");
}

#[test]
fn synced_clock_uses_wraparound_safe_uptime_delta() {
    let mut clock = LogClock::new();
    let dt = parse_cclk_time(r#"+CCLK: "26/06/21,20:30:45+32""#).unwrap();

    clock.sync_from_network(u32::MAX - 999, dt);

    assert_eq!(clock.timestamp(2_000), "2026-06-21 20:30:48+08:00");
}
