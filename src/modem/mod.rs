//! Modem abstraction layer.
//!
//! Two-tier design:
//!
//! - `AtTransport` — the wire protocol seam.  Implement this for each new modem
//!   (four methods: `send_at`, `poll_urc`, `write_raw`, `wait_for_prompt`).
//!
//! - `ModemPort: AtTransport` — SMS and voice operations built on top.
//!   `send_pdu_sms` and `hang_up` have standard AT default implementations,
//!   so a new modem gets them for free; only `post_telegram_https` needs an
//!   override for modems with a built-in HTTP stack (e.g. the A7670G).
//!
//! Concrete implementations live under `a76xx/`.

pub mod urc;

#[cfg(any(feature = "esp32", feature = "testing"))]
pub mod a76xx;

use std::time::Duration;
use thiserror::Error;

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
    #[error("modem returned ERROR: {0}")]
    AtError(String),
    #[error("UART write failed")]
    Io,
    #[error("modem not ready")]
    NotReady,
    #[error("feature not supported on this modem")]
    NotSupported,
}

/// Signal strength snapshot.
#[derive(Debug, Clone)]
pub struct ModemStatus {
    /// CSQ value (0–31), 99 = unknown.
    pub csq: u8,
    /// Operator name.
    pub operator: String,
    /// Registration status: true = registered.
    pub registered: bool,
}

/// CSQ value representing "unknown signal".
pub const CSQ_UNKNOWN: u8 = 99;

impl Default for ModemStatus {
    fn default() -> Self {
        ModemStatus {
            csq: CSQ_UNKNOWN,
            operator: String::new(),
            registered: false,
        }
    }
}

/// Parse a +CREG? response body for registration status.
/// Returns true when stat is 1 (home) or 5 (roaming).
pub fn creg_registered(body: &str) -> bool {
    body.contains(",1") || body.contains(",5")
}

// ── Tier 1: wire protocol ─────────────────────────────────────────────────────

/// Raw AT transport seam.
///
/// Implement the four methods below for any new modem.
/// `ModemPort` is then blanket-available: `send_pdu_sms` (standard CMGS
/// handshake) and `hang_up` (ATH) both use only `AtTransport` primitives,
/// so they do not need to be reimplemented for each new modem.
pub trait AtTransport {
    /// Send `AT<cmd>\r` and collect the response lines until OK/ERROR/timeout.
    fn send_at(&mut self, cmd: &str) -> Result<AtResponse, ModemError>;

    /// Non-blocking poll: return a URC line if one is available.
    fn poll_urc(&mut self) -> Option<String>;

    /// Write raw bytes to the modem UART (used for PDU body in `AT+CMGS`).
    fn write_raw(&mut self, data: &[u8]) -> Result<(), ModemError>;

    /// Block until `prompt` byte is received or `timeout` elapses.
    /// Used for the `>` prompt in the `AT+CMGS` PDU send sequence.
    fn wait_for_prompt(&mut self, prompt: u8, timeout: Duration) -> bool;
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
    /// Default: standard `AT+CMGS` / `>` / Ctrl-Z handshake.
    #[cfg(feature = "esp32")]
    fn send_pdu_sms(&mut self, hex: &str, tpdu_len: u8) -> Result<u8, ModemError> {
        crate::modem::a76xx::sms::send_pdu(self, hex, tpdu_len)
    }
    #[cfg(not(feature = "esp32"))]
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

    /// HTTPS POST JSON via the modem's built-in HTTP stack (Quectel `AT+QHTTP*`).
    /// Used when the ESP32 has no WiFi and IM traffic goes over cellular PDP.
    ///
    /// Default: `NotSupported` — override for modems with a built-in HTTP stack.
    fn post_telegram_https(&mut self, path: &str, json: &str) -> Result<String, ModemError> {
        let _ = (path, json);
        Err(ModemError::NotSupported)
    }

    /// Query CSQ, operator name, and registration status from the modem.
    fn update_status(&mut self) -> ModemStatus {
        let mut s = ModemStatus::default();
        if let Ok(r) = self.send_at("+CSQ") {
            if let Some(v) = r.body.strip_prefix("+CSQ: ") {
                s.csq = v
                    .split(',')
                    .next()
                    .and_then(|x| x.trim().parse().ok())
                    .unwrap_or(CSQ_UNKNOWN);
            }
        }
        if let Ok(r) = self.send_at("+COPS?") {
            if let Some(start) = r.body.find('"') {
                if let Some(end) = r.body[start + 1..].find('"') {
                    s.operator = r.body[start + 1..start + 1 + end].to_string();
                }
            }
        }
        if let Ok(r) = self.send_at("+CREG?") {
            s.registered = creg_registered(&r.body);
        }
        s
    }
}
