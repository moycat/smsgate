//! SMS types shared across the sms/* submodules.

pub mod codec;
pub mod concat;
pub mod sender;

use thiserror::Error;

/// Maximum number of concatenated SMS parts for a single outbound message.
pub const MAX_SMS_PARTS: usize = 10;

/// A fully-decoded inbound SMS message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmsMessage {
    /// Sender phone number (E.164 or national format).
    pub sender: String,
    /// Message body (UTF-8).
    pub body: String,
    /// Timestamp from PDU SCTS field ("yy/MM/dd,HH:mm:ss+zz").
    pub timestamp: String,
    /// Modem slot index where the PDU was stored (for deletion).
    pub slot: u16,
}

/// Errors that can occur during SMS encode/decode.
#[derive(Debug, Error)]
pub enum SmsError {
    #[error("malformed PDU: {0}")]
    MalformedPdu(&'static str),
    #[error("PDU too long")]
    PduTooLong,
    #[error("empty phone or body")]
    EmptyInput,
}
