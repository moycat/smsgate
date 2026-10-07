//! Modem abstraction layer.
//!
//! Two-tier design:
//!
//! - `AtTransport` — the wire protocol seam.  Implement this for each new modem
//!   (`send_at`, streaming responses, `poll_urc`, `write_raw`, and `wait_for_prompt`).
//!
//! - `ModemPort: AtTransport` — SMS and voice operations built on top.
//!   `send_pdu_sms` and `hang_up` have standard AT default implementations,
//!   so a new modem gets them for free.
//!
//! Concrete implementations live under `a76xx/`.

mod status;
pub mod urc;
pub use status::{RegistrationDomain, RegistrationQuery};

#[cfg(any(feature = "esp32", feature = "testing"))]
pub mod a76xx;

use std::time::Duration;
use thiserror::Error;

use crate::log_clock::{parse_cclk_time, NetworkDateTime};

/// Raw AT command response.
#[derive(Debug, Clone)]
pub struct AtResponse {
    /// All lines before the final status line, joined.
    pub body: String,
    /// True if the response ended with OK; false for ERROR / CME ERROR.
    pub ok: bool,
}

/// Errors from the modem layer.
#[derive(Debug, Error)]
pub enum ModemError {
    #[error("timeout waiting for response")]
    Timeout,
    #[error("AT operation interrupted for incoming call")]
    InterruptedForCall,
    #[error("modem returned ERROR: {0}")]
    AtError(String),
    #[error("UART write failed")]
    Io,
    #[error("modem not ready")]
    NotReady,
    #[error("AT response exceeded the bounded body buffer")]
    ResponseTooLong,
}

/// Bounded AT receive faults collected between main-loop iterations.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ModemDiagnostics {
    pub dropped_urcs: u32,
    pub dropped_response_lines: u32,
    pub overlong_lines: u32,
}

impl ModemDiagnostics {
    pub fn is_empty(self) -> bool {
        self.dropped_urcs == 0 && self.dropped_response_lines == 0 && self.overlong_lines == 0
    }

    pub fn accumulate(&mut self, other: Self) {
        self.dropped_urcs = self.dropped_urcs.saturating_add(other.dropped_urcs);
        self.dropped_response_lines = self
            .dropped_response_lines
            .saturating_add(other.dropped_response_lines);
        self.overlong_lines = self.overlong_lines.saturating_add(other.overlong_lines);
    }
}

/// Signal strength snapshot.
#[derive(Debug, Clone)]
pub struct ModemStatus {
    /// CSQ value (0–31), 99 = unknown.
    pub csq: u8,
    /// Why CSQ is unavailable, if the query failed instead of returning 99.
    pub csq_error: Option<String>,
    pub csq_elapsed_ms: u32,
    /// Operator name.
    pub operator: String,
    /// Circuit-domain registration, retaining SMS-only and unknown states.
    pub registration: RegistrationQuery,
}

/// CSQ value representing "unknown signal".
pub const CSQ_UNKNOWN: u8 = 99;

impl Default for ModemStatus {
    fn default() -> Self {
        ModemStatus {
            csq: CSQ_UNKNOWN,
            csq_error: None,
            csq_elapsed_ms: 0,
            operator: String::new(),
            registration: RegistrationQuery::default(),
        }
    }
}

/// Parse a +CREG? response body for registration status.
/// Includes SMS-only registration (stat 6/7), without implying voice availability.
pub fn creg_registered(body: &str) -> bool {
    creg_registration_status(body).unwrap_or(false)
}

/// Return a registration state only when the modem supplied a numeric status.
pub fn creg_registration_status(body: &str) -> Option<bool> {
    status::parse_registration_stat(body, "+CREG:").and_then(status::registered_for_stat)
}

/// Whether the modem still routes stored SMS notifications to the UART.
pub fn cnmi_store_notifications_enabled(body: &str) -> bool {
    let Some(fields) = body
        .lines()
        .find_map(|line| line.trim().strip_prefix("+CNMI:"))
    else {
        return false;
    };
    fields
        .split(',')
        .map(str::trim)
        .take(5)
        .eq(["2", "1", "0", "0", "0"])
}

// ── Tier 1: wire protocol ─────────────────────────────────────────────────────

/// Raw AT transport seam.
///
/// Implement the four methods below for any new modem. The modem-specific
/// `ModemPort::send_pdu_sms` must also preserve unrelated URCs during CMGS.
pub trait AtTransport {
    /// Send `AT<cmd>\r` and collect the response lines until OK/ERROR/timeout.
    fn send_at(&mut self, cmd: &str) -> Result<AtResponse, ModemError>;

    /// Consume a potentially large response line by line without retaining it.
    /// Concrete UART implementations should override this bounded-memory
    /// fallback when the response can contain many SMS entries.
    fn send_at_streaming(
        &mut self,
        cmd: &str,
        _idle_timeout: Duration,
        _hard_timeout: Duration,
        on_line: &mut dyn FnMut(&str),
    ) -> Result<(), ModemError> {
        let response = self.send_at(cmd)?;
        if !response.ok {
            return Err(ModemError::AtError(response.body));
        }
        for line in response.body.lines() {
            on_line(line);
        }
        Ok(())
    }

    /// Non-blocking poll: return a URC line if one is available.
    fn poll_urc(&mut self) -> Option<String>;

    /// Write raw bytes to the modem UART (used for PDU body in `AT+CMGS`).
    fn write_raw(&mut self, data: &[u8]) -> Result<(), ModemError>;

    /// Block until `prompt` byte is received or `timeout` elapses.
    /// Used for the `>` prompt in the `AT+CMGS` PDU send sequence.
    fn wait_for_prompt(&mut self, prompt: u8, timeout: Duration) -> bool;

    /// Return and clear receive fault counters. Implementations without a
    /// diagnostic buffer can use the empty default.
    fn take_diagnostics(&mut self) -> ModemDiagnostics {
        ModemDiagnostics::default()
    }
}

// ── Tier 2: SMS + voice + optional HTTP ──────────────────────────────────────

/// High-level SMS, voice, and (optionally) HTTP operations.
///
/// `send_pdu_sms` and `hang_up` have default implementations that work for
/// any modem following standard AT command syntax.  Override them only if
/// your modem requires a non-standard sequence.
pub trait ModemPort: AtTransport {
    /// Send an SMS in PDU mode; return the message reference number.
    ///
    /// Implementations must preserve unrelated URCs during the CMGS exchange.
    fn send_pdu_sms(&mut self, hex: &str, tpdu_len: u8) -> Result<u8, ModemError>;

    /// Hang up the current call.
    ///
    /// Default: `ATH`.
    fn hang_up(&mut self) -> Result<(), ModemError> {
        let r = self.send_at("H")?;
        if r.ok {
            Ok(())
        } else {
            Err(ModemError::AtError("ATH failed".into()))
        }
    }

    /// Query CSQ, operator name, and registration status from the modem.
    fn update_status(&mut self) -> ModemStatus {
        let mut s = ModemStatus::default();
        let csq_started = std::time::Instant::now();
        match self.send_at("+CSQ") {
            Ok(r) if r.ok => {
                match r
                    .body
                    .lines()
                    .find_map(|line| line.trim().strip_prefix("+CSQ:"))
                    .and_then(|fields| fields.split(',').next())
                    .and_then(|value| value.trim().parse::<u8>().ok())
                {
                    Some(csq) if csq <= 31 || csq == CSQ_UNKNOWN => s.csq = csq,
                    _ => s.csq_error = Some("invalid +CSQ response".into()),
                }
            }
            Ok(r) => {
                s.csq_error = Some(format!("AT+CSQ rejected: {}", r.body.trim()));
            }
            Err(e) => s.csq_error = Some(e.to_string()),
        }
        s.csq_elapsed_ms = status::elapsed_ms(csq_started.elapsed());
        if let Ok(r) = self.send_at("+COPS?") {
            if let Some(line) = r
                .body
                .lines()
                .find(|line| line.trim().starts_with("+COPS:"))
            {
                if let Some(start) = line.find('"') {
                    if let Some(end) = line[start + 1..].find('"') {
                        s.operator = line[start + 1..start + 1 + end].to_string();
                    }
                }
            }
        }
        s.registration = self.query_registration(RegistrationDomain::Circuit);
        s
    }

    /// Read one registration domain without changing URC reporting or attachment.
    fn query_registration(&mut self, domain: RegistrationDomain) -> RegistrationQuery {
        let started = std::time::Instant::now();
        let response = self.send_at(domain.command());
        RegistrationQuery::from_response(domain, response, started.elapsed())
    }

    /// Query modem RTC time. A76xx exposes network-updated local time via CCLK
    /// once CTZU/NITZ has provided a valid time zone and clock.
    fn query_network_time(&mut self) -> Result<NetworkDateTime, ModemError> {
        let r = self.send_at("+CCLK?")?;
        if !r.ok {
            return Err(ModemError::AtError(r.body));
        }
        parse_cclk_time(&r.body)
            .ok_or_else(|| ModemError::AtError(format!("invalid CCLK response: {}", r.body)))
    }
}
