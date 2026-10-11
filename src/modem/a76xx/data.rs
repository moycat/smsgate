//! Packet-data detachment for A76xx modems.
//!
//! Detachment is volatile modem state. Call the enforcement helper again after
//! modem resets and periodically while the firmware runs. It deliberately leaves
//! APN definitions and IMS settings unchanged.

use crate::modem::{AtTransport, ModemError};
use thiserror::Error;

/// Result of enforcing packet-data detachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PacketDataOutcome {
    /// The modem reported detached without requiring a state change.
    AlreadyDetached,
    /// A detach command succeeded and the modem confirmed detached afterward.
    Detached,
}

/// A failure to detach packet data or positively confirm the resulting state.
#[derive(Debug, Error)]
pub enum PacketDataError {
    #[error("packet-data detach transport failed: {0}")]
    DetachTransport(#[source] ModemError),
    #[error("AT+CGATT=0 rejected")]
    DetachRejected,
    #[error("packet-data attachment query transport failed: {0}")]
    QueryTransport(#[source] ModemError),
    #[error("AT+CGATT? rejected")]
    QueryRejected,
    #[error("missing, invalid, or ambiguous +CGATT attachment state")]
    InvalidAttachmentResponse,
    #[error("packet data remains attached after AT+CGATT=0")]
    StillAttached,
}

/// Detach packet data and require a successful readback of exactly state zero.
///
/// A successful write alone does not prove detachment. This helper never sends
/// an attach command or activates a PDP context, and performs no retries.
pub fn disable_packet_data<T: AtTransport + ?Sized>(modem: &mut T) -> Result<(), PacketDataError> {
    let response = modem
        .send_at("+CGATT=0")
        .map_err(PacketDataError::DetachTransport)?;
    if !response.ok {
        return Err(PacketDataError::DetachRejected);
    }

    if query_packet_data_attached(modem)? {
        Err(PacketDataError::StillAttached)
    } else {
        Ok(())
    }
}

/// Verify detachment, detaching once if attached or the query is inconclusive.
///
/// An unreadable initial state cannot establish that data is disabled, so it
/// triggers the same bounded detach-and-verify sequence as an attached state.
pub fn enforce_packet_data_disabled<T: AtTransport + ?Sized>(
    modem: &mut T,
) -> Result<PacketDataOutcome, PacketDataError> {
    if matches!(query_packet_data_attached(modem), Ok(false)) {
        return Ok(PacketDataOutcome::AlreadyDetached);
    }

    disable_packet_data(modem)?;
    Ok(PacketDataOutcome::Detached)
}

fn query_packet_data_attached<T: AtTransport + ?Sized>(
    modem: &mut T,
) -> Result<bool, PacketDataError> {
    let response = modem
        .send_at("+CGATT?")
        .map_err(PacketDataError::QueryTransport)?;
    if !response.ok {
        return Err(PacketDataError::QueryRejected);
    }

    let mut attached = None;
    for line in response.body.lines().map(str::trim) {
        if !line.starts_with("+CGATT") {
            continue;
        }
        let state = match line.strip_prefix("+CGATT:").map(str::trim) {
            Some("0") => false,
            Some("1") => true,
            _ => return Err(PacketDataError::InvalidAttachmentResponse),
        };
        if attached.replace(state).is_some() {
            return Err(PacketDataError::InvalidAttachmentResponse);
        }
    }

    attached.ok_or(PacketDataError::InvalidAttachmentResponse)
}
