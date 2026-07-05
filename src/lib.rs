//! smsgate — library root.

#[cfg(feature = "esp32")]
pub mod boards;
pub mod bridge;
pub mod commands;
pub mod config;
pub mod creds;
pub mod diagnostics;
pub mod i18n;
pub mod im;
pub mod log_clock;
pub mod log_ring;
pub mod mms;
pub mod modem;
pub mod ota;
pub mod persist;
pub mod sms;
pub mod text;
pub mod timer;

#[cfg(feature = "testing")]
pub mod testing;
