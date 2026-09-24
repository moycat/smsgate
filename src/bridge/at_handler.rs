//! Hidden AT diagnostic command handling for the configured Telegram chat.

use crate::modem::{ModemError, ModemPort};
use std::time::Duration;

const MAX_COMMAND_BYTES: usize = 128;
const MAX_RESPONSE_BYTES: usize = 3500;
const IDLE_TIMEOUT: Duration = Duration::from_secs(10);
const HARD_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtRequestError {
    MissingCommand,
    InvalidCommand,
    CommandTooLong,
    UnsupportedCommand,
}

impl AtRequestError {
    pub fn reply(self) -> &'static str {
        match self {
            Self::MissingCommand => crate::i18n::at_usage(),
            Self::InvalidCommand => crate::i18n::at_invalid_command(),
            Self::CommandTooLong => crate::i18n::at_command_too_long(),
            Self::UnsupportedCommand => crate::i18n::at_unsupported_command(),
        }
    }
}

/// Recognize `/at` without registering it as a public bot command.
/// The returned suffix can be passed directly to `ModemPort::send_at_streaming`.
pub fn parse_hidden_at_command(text: &str) -> Option<Result<&str, AtRequestError>> {
    let text = text.trim();
    let first = text.split_whitespace().next()?;
    let name = first.strip_prefix('/')?.split('@').next()?;
    if name != "at" {
        return None;
    }

    let command = text[first.len()..].trim();
    if command.is_empty() {
        return Some(Err(AtRequestError::MissingCommand));
    }
    if command.len() > MAX_COMMAND_BYTES {
        return Some(Err(AtRequestError::CommandTooLong));
    }
    if !command.is_ascii()
        || command.bytes().any(|byte| byte.is_ascii_control())
        || command.contains(';')
        || !command[..command.len().min(2)].eq_ignore_ascii_case("AT")
    {
        return Some(Err(AtRequestError::InvalidCommand));
    }
    let uppercase = command.to_ascii_uppercase();
    if !is_diagnostic_command(&uppercase) {
        return Some(Err(AtRequestError::UnsupportedCommand));
    }
    Some(Ok(&command[2..]))
}

fn is_diagnostic_command(command: &str) -> bool {
    matches!(
        command,
        "AT" | "ATI"
            | "AT+SIMCOMATI"
            | "AT+CSQ"
            | "AT+CREG?"
            | "AT+CGREG?"
            | "AT+CEREG?"
            | "AT+COPS?"
            | "AT+CPIN?"
            | "AT+CCLK?"
            | "AT+CNMI?"
            | "AT+CMGF?"
            | "AT+CPMS?"
            | "AT+CGATT?"
            | "AT+CGACT?"
            | "AT+CPSI?"
            | "AT+CEER"
            | "AT+CBC"
            | "AT+CCID"
            | "AT+CLCC"
            | "AT+CGMI"
            | "AT+CGMM"
            | "AT+CGMR"
            | "AT+CGSN"
            | "AT+GMI"
            | "AT+GMM"
            | "AT+GMR"
            | "AT+GSN"
    )
}

pub struct AtCommandReply {
    pub text: String,
    pub succeeded: bool,
}

/// Execute one already-validated AT command. The caller must release its
/// modem lock before sending `text` over Telegram.
pub fn execute_at_command(suffix: &str, modem: &mut dyn ModemPort) -> AtCommandReply {
    let mut text = String::new();
    let mut truncated = false;
    let result = modem.send_at_streaming(suffix, IDLE_TIMEOUT, HARD_TIMEOUT, &mut |line| {
        append_response_line(&mut text, line, &mut truncated)
    });

    let succeeded = result.is_ok();
    match result {
        Ok(()) => append_response_line(&mut text, "OK", &mut truncated),
        Err(ModemError::AtError(error)) => {
            append_response_line(&mut text, &error, &mut truncated);
        }
        Err(error) => {
            append_response_line(
                &mut text,
                &crate::i18n::at_transport_error(&error.to_string()),
                &mut truncated,
            );
        }
    }
    if truncated {
        text.push_str(crate::i18n::at_response_truncated());
        text.push_str(if succeeded { "\nOK" } else { "\nERROR" });
    }
    AtCommandReply { text, succeeded }
}

fn append_response_line(text: &mut String, line: &str, truncated: &mut bool) {
    if *truncated {
        return;
    }
    let separator = usize::from(!text.is_empty());
    let remaining = MAX_RESPONSE_BYTES.saturating_sub(text.len() + separator);
    if remaining == 0 {
        *truncated = true;
        return;
    }
    if separator != 0 {
        text.push('\n');
    }
    if line.len() <= remaining {
        text.push_str(line);
    } else {
        let cut = line.floor_char_boundary(remaining);
        text.push_str(&line[..cut]);
        *truncated = true;
    }
}
