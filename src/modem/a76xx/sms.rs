//! PDU send/read/delete AT command flows — generic over `AtTransport`.
//!
//! `send_pdu` is the standard AT+CMGS sequence (send command, wait for `>`,
//! write PDU hex, send Ctrl-Z, collect +CMGS: response).  It works for any
//! modem that implements `AtTransport`; the A76xx-specific `send_pdu_sms`
//! default in `ModemPort` delegates here.

use crate::modem::{AtTransport, ModemError};
use std::time::Duration;

/// Send a PDU SMS via the standard `AT+CMGS` handshake.
/// Returns the message reference number (MR) on success.
pub fn send_pdu<P: AtTransport + ?Sized>(
    port: &mut P,
    hex: &str,
    tpdu_len: u8,
) -> Result<u8, ModemError> {
    // Issue AT+CMGS=<tpduLen>
    let cmd = format!("+CMGS={}", tpdu_len);
    port.write_raw(format!("AT{}\r", cmd).as_bytes())?;

    // Wait for '>' prompt (10 s)
    if !port.wait_for_prompt(b'>', Duration::from_secs(10)) {
        return Err(ModemError::Timeout);
    }

    const CTRL_Z: u8 = 0x1A;
    port.write_raw(hex.as_bytes())?;
    port.write_raw(&[CTRL_Z])?;

    // Wait for +CMGS: or OK (60 s for network round-trip)
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let mut lines: Vec<String> = Vec::new();
    loop {
        if std::time::Instant::now() > deadline {
            return Err(ModemError::Timeout);
        }
        // Kick the Task Watchdog Timer — SMS send can block > 30 s on congested networks.
        unsafe {
            esp_idf_sys::esp_task_wdt_reset();
        }
        // Read using a generous timeout per line
        if let Some(line) = read_line_raw(port, Duration::from_secs(5)) {
            let line = line.trim().to_string();
            if line.is_empty() {
                continue;
            }
            if line == "OK" {
                break;
            }
            if line.starts_with("ERROR") || line.starts_with("+CMS ERROR") {
                return Err(ModemError::AtError(line));
            }
            lines.push(line);
        }
    }

    // Parse +CMGS: <mr>
    for line in &lines {
        if let Some(rest) = line.strip_prefix("+CMGS:") {
            let mr: u8 = rest.trim().parse().unwrap_or(0);
            return Ok(mr);
        }
    }

    // OK but no +CMGS: line — some firmware variants omit it.
    // Return 0 as a sentinel (caller treats 0 as "unknown MR").
    Ok(0)
}

fn read_line_raw<P: AtTransport + ?Sized>(port: &mut P, timeout: Duration) -> Option<String> {
    // We reuse poll_urc which has a short timeout — call in loop until we get a real line
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if std::time::Instant::now() > deadline {
            return None;
        }
        if let Some(l) = port.poll_urc() {
            return Some(l);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
