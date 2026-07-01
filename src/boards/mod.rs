//! Board hardware abstraction layer.

#[cfg(feature = "esp32")]
pub mod ta7670x;

#[cfg(feature = "esp32")]
use crate::modem::ModemPort;
#[cfg(feature = "esp32")]
use std::sync::{Arc, Mutex};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum BoardError {
    #[error("GPIO init failed: {0}")]
    Gpio(String),
    #[error("UART init failed: {0}")]
    Uart(String),
}

/// Board-level abstraction: pin layout + power-on sequence.
///
/// Used only in `main.rs` during startup to build the modem driver.
/// Returns `Arc<Mutex<dyn ModemPort + Send>>` so the rest of the system is
/// insulated from the concrete modem type — adding a new board (SIM800,
/// A7608, EC21, …) only requires a new `Board` impl; no other file changes.
#[cfg(feature = "esp32")]
pub trait Board {
    /// Configure GPIO and run the board-specific power-on sequence.
    fn init(
        &self,
        peripherals: &mut esp_idf_hal::peripherals::Peripherals,
    ) -> Result<(), BoardError>;

    /// Build the UART driver + modem, shared across main and polling threads.
    /// Returns a type-erased handle so callers never depend on the concrete modem type.
    fn build_modem_port(
        &self,
        peripherals: &mut esp_idf_hal::peripherals::Peripherals,
    ) -> Result<Arc<Mutex<dyn ModemPort + Send>>, BoardError>;
}
