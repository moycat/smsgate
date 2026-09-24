//! Compile-time configuration injected by build.rs from config.toml.
//! This module contains *only* env!() references — no hardcoded values.

pub struct Config;

impl Config {
    pub const WIFI_SSID: &'static str = env!("CFG_WIFI_SSID");
    pub const WIFI_PASSWORD: &'static str = env!("CFG_WIFI_PASSWORD");
    pub const BOT_TOKEN: &'static str = env!("CFG_IM_BOT_TOKEN");
    pub const CHAT_ID: i64 = {
        // parse at compile time; default 0 if empty/missing
        let s = env!("CFG_IM_CHAT_ID");
        parse_i64_const(s)
    };
    pub const UART_TX: u8 = parse_u8_const(env!("CFG_MODEM_UART_TX"));
    pub const UART_RX: u8 = parse_u8_const(env!("CFG_MODEM_UART_RX"));
    pub const UART_BAUD: u32 = parse_nonzero_u32_const(env!("CFG_MODEM_UART_BAUD"));
    pub const PWRKEY_PIN: u8 = parse_u8_const(env!("CFG_MODEM_PWRKEY"));
    pub const MODEM_SIM_PIN: &'static str = env!("CFG_MODEM_SIM_PIN");
    pub const MAX_FAILURES: u8 = parse_nonzero_u8_const(env!("CFG_BRIDGE_MAX_FAILURES"));
    pub const POLL_INTERVAL_MS: u32 = parse_nonzero_u32_const(env!("CFG_BRIDGE_POLL_INTERVAL_MS"));
    pub const GIT_COMMIT: &'static str = env!("CFG_GIT_COMMIT");
    pub const APPLY_COMPILED_CONFIG: bool = parse_bool_env_true(env!("CFG_APPLY_COMPILED_CONFIG"));
}

const fn parse_bool_env_true(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 4 && b[0] == b't' && b[1] == b'r' && b[2] == b'u' && b[3] == b'e'
}

const fn parse_u8_const(s: &str) -> u8 {
    let value = parse_u64_const(s);
    if value > u8::MAX as u64 {
        panic!("numeric config exceeds u8 range");
    }
    value as u8
}

const fn parse_u32_const(s: &str) -> u32 {
    let value = parse_u64_const(s);
    if value > u32::MAX as u64 {
        panic!("numeric config exceeds u32 range");
    }
    value as u32
}

const fn parse_nonzero_u8_const(s: &str) -> u8 {
    let value = parse_u8_const(s);
    if value == 0 {
        panic!("numeric config must be nonzero");
    }
    value
}

const fn parse_nonzero_u32_const(s: &str) -> u32 {
    let value = parse_u32_const(s);
    if value == 0 {
        panic!("numeric config must be nonzero");
    }
    value
}

const fn parse_i64_const(s: &str) -> i64 {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return 0;
    }
    let (neg, start) = if bytes[0] == b'-' {
        (true, 1)
    } else {
        (false, 0)
    };
    if start == bytes.len() {
        panic!("invalid signed numeric config");
    }
    let mut i = start;
    let mut acc: u64 = 0;
    let limit = if neg {
        i64::MAX as u64 + 1
    } else {
        i64::MAX as u64
    };
    while i < bytes.len() {
        let d = bytes[i];
        if !d.is_ascii_digit() {
            panic!("invalid signed numeric config");
        }
        let digit = (d - b'0') as u64;
        if acc > (limit - digit) / 10 {
            panic!("signed numeric config out of range");
        }
        acc = acc * 10 + digit;
        i += 1;
    }
    if neg {
        if acc == i64::MAX as u64 + 1 {
            i64::MIN
        } else {
            -(acc as i64)
        }
    } else {
        acc as i64
    }
}

const fn parse_u64_const(s: &str) -> u64 {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        panic!("missing unsigned numeric config");
    }
    let mut i = 0;
    let mut acc: u64 = 0;
    while i < bytes.len() {
        let d = bytes[i];
        if !d.is_ascii_digit() {
            panic!("invalid unsigned numeric config");
        }
        let digit = (d - b'0') as u64;
        if acc > (u64::MAX - digit) / 10 {
            panic!("unsigned numeric config out of range");
        }
        acc = acc * 10 + digit;
        i += 1;
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_config_accepts_boundaries() {
        assert_eq!(parse_u8_const("255"), 255);
        assert_eq!(parse_u32_const("4294967295"), u32::MAX);
        assert_eq!(parse_i64_const("-9223372036854775808"), i64::MIN);
    }

    #[test]
    fn numeric_config_rejects_overflow_and_junk() {
        for invalid in ["", "256", "12x", "18446744073709551616"] {
            assert!(std::panic::catch_unwind(|| parse_u8_const(invalid)).is_err());
        }
        assert!(std::panic::catch_unwind(|| parse_u32_const("4294967296")).is_err());
        assert!(std::panic::catch_unwind(|| parse_i64_const("12x")).is_err());
        assert!(std::panic::catch_unwind(|| parse_nonzero_u8_const("0")).is_err());
        assert!(std::panic::catch_unwind(|| parse_nonzero_u32_const("0")).is_err());
    }
}
