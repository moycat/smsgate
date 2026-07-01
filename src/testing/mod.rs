//! Test utilities — mock implementations of all I/O boundaries.
//!
//! Enabled with `--features testing`. Not compiled into firmware.
//!
//! Provides:
//! - `mocks::ScriptedModem` — programmable AT response script
//! - `mocks::RecordingMessenger` — captures sent messages
//! - `mocks::FailingMessenger` — always errors on send
//! - `Scenario` — declarative end-to-end test DSL

pub mod mocks;
pub mod scenario;

pub use scenario::Scenario;

/// Strip all ASCII whitespace from a PDU hex literal (for readable test constants).
pub fn pdu(hex: &str) -> String {
    hex.chars().filter(|c| !c.is_ascii_whitespace()).collect()
}
