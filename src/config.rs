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
    pub const UART_BAUD: u32 = parse_u32_const(env!("CFG_MODEM_UART_BAUD"));
    pub const PWRKEY_PIN: u8 = parse_u8_const(env!("CFG_MODEM_PWRKEY"));
    /// Packet-switched (cellular data) attachment: `AT+CGATT=1` vs `AT+CGATT=0`.
    pub const MODEM_CELLULAR_DATA: bool = parse_bool_env_true(env!("CFG_MODEM_CELLULAR_DATA"));
    /// When WiFi fails, bring up PDP and send Telegram via modem `AT+QHTTP*` (requires `apn`).
    pub const CELLULAR_FALLBACK: bool = parse_bool_env_true(env!("CFG_CELLULAR_FALLBACK"));
    pub const MODEM_APN: &'static str = env!("CFG_MODEM_APN");
    pub const MODEM_APN_USER: &'static str = env!("CFG_MODEM_APN_USER");
    pub const MODEM_APN_PASS: &'static str = env!("CFG_MODEM_APN_PASS");
    pub const MODEM_SIM_PIN: &'static str = env!("CFG_MODEM_SIM_PIN");
    pub const MAX_FAILURES: u8 = parse_u8_const(env!("CFG_BRIDGE_MAX_FAILURES"));
    pub const POLL_INTERVAL_MS: u32 = parse_u32_const(env!("CFG_BRIDGE_POLL_INTERVAL_MS"));
    pub const GIT_COMMIT: &'static str = env!("CFG_GIT_COMMIT");
    pub const APPLY_COMPILED_CONFIG: bool = parse_bool_env_true(env!("CFG_APPLY_COMPILED_CONFIG"));
}

const fn parse_bool_env_true(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 4 && b[0] == b't' && b[1] == b'r' && b[2] == b'u' && b[3] == b'e'
}

const fn parse_u8_const(s: &str) -> u8 {
    parse_u64_const(s) as u8
}

const fn parse_u32_const(s: &str) -> u32 {
    parse_u64_const(s) as u32
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
    let mut i = start;
    let mut acc: i64 = 0;
    while i < bytes.len() {
        let d = bytes[i];
        if d >= b'0' && d <= b'9' {
            acc = acc * 10 + (d - b'0') as i64;
        }
        i += 1;
    }
    if neg {
        -acc
    } else {
        acc
    }
}

const fn parse_u64_const(s: &str) -> u64 {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut acc: u64 = 0;
    while i < bytes.len() {
        let d = bytes[i];
        if d >= b'0' && d <= b'9' {
            acc = acc * 10 + (d - b'0') as u64;
        }
        i += 1;
    }
    acc
}
