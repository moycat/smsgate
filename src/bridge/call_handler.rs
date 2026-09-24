//! Incoming call state machine: Idle → Ringing → Cooldown.

use crate::{
    log_ring::{LogEvent, LogKind},
    modem::ModemPort,
};
use std::time::{Duration, Instant};

const CLIP_DEADLINE: Duration = Duration::from_millis(1500);
const COOLDOWN: Duration = Duration::from_secs(6);

#[derive(Debug, PartialEq)]
enum State {
    Idle,
    Ringing {
        clip_deadline: Instant,
        number: Option<String>,
    },
    Cooldown {
        until: Instant,
    },
}

/// Incoming call handler with auto-hangup and IM notification.
pub struct CallHandler {
    state: State,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallNotification {
    pub text: String,
    caller_display: String,
    detail: &'static str,
    hang_up_error: Option<String>,
}

impl CallNotification {
    pub fn log_event(&self) -> LogEvent {
        LogEvent::new(LogKind::Call, &self.caller_display, self.detail, true)
    }

    pub fn hang_up_error(&self) -> Option<&str> {
        self.hang_up_error.as_deref()
    }
}

impl CallHandler {
    pub fn new() -> Self {
        CallHandler { state: State::Idle }
    }

    /// Feed a URC line while deferring IM delivery until after the modem lock is released.
    pub fn handle_urc_deferred(
        &mut self,
        line: &str,
        modem: &mut dyn ModemPort,
    ) -> Option<CallNotification> {
        if line == "RING" || line.starts_with("RING") {
            self.on_ring();
            return None;
        }
        if line.starts_with("+CLIP:") {
            if let Some(number) = crate::sms::codec::parse_clip_line(line) {
                return self.on_clip(number, modem);
            }
            return None;
        }
        if line == "NO CARRIER" && matches!(self.state, State::Ringing { .. }) {
            // A short call can end before +CLIP or the 1.5 s deadline. It still
            // reached the modem and must produce a missed-call notification.
            let notification = self.commit_call(None, modem, false);
            self.state = State::Idle;
            return Some(notification);
        }
        None
    }

    /// Drive the state machine clock while deferring IM delivery.
    pub fn tick_deferred(&mut self, modem: &mut dyn ModemPort) -> Option<CallNotification> {
        match &self.state {
            State::Ringing {
                clip_deadline,
                number,
            } => {
                let deadline = *clip_deadline;
                let num = number.clone();
                if Instant::now() >= deadline {
                    // CLIP not received in time — commit with unknown number
                    return Some(self.commit_call(num, modem, true));
                }
            }
            State::Cooldown { until } => {
                if Instant::now() >= *until {
                    self.state = State::Idle;
                }
            }
            State::Idle => {}
        }
        None
    }

    fn on_ring(&mut self) {
        if matches!(self.state, State::Cooldown { .. }) {
            return; // suppress duplicate ring in cooldown window
        }
        if matches!(self.state, State::Idle) {
            self.state = State::Ringing {
                clip_deadline: Instant::now() + CLIP_DEADLINE,
                number: None,
            };
        }
    }

    fn on_clip(&mut self, number: String, modem: &mut dyn ModemPort) -> Option<CallNotification> {
        if let State::Ringing { .. } = &mut self.state {
            let n = if number.is_empty() {
                None
            } else {
                Some(number)
            };
            return Some(self.commit_call(n, modem, true));
        }
        None
    }

    fn commit_call(
        &mut self,
        number: Option<String>,
        modem: &mut dyn ModemPort,
        hang_up: bool,
    ) -> CallNotification {
        let hang_up_error = if hang_up {
            modem.hang_up().err().map(|e| {
                log::warn!("[call] hang_up failed: {}", e);
                e.to_string()
            })
        } else {
            None
        };

        // Notify via IM
        let display = match &number {
            Some(n) if !n.is_empty() => crate::sms::codec::human_readable_phone(n),
            _ => "unknown caller".to_string(),
        };
        let text = crate::i18n::incoming_call(&display);

        log::info!("[call] call detected from {}", display);
        self.state = State::Cooldown {
            until: Instant::now() + COOLDOWN,
        };
        CallNotification {
            text,
            caller_display: display,
            detail: if !hang_up {
                "incoming call; ended before caller ID"
            } else if hang_up_error.is_some() {
                "incoming call; hang-up failed"
            } else {
                "incoming call; hung up"
            },
            hang_up_error,
        }
    }
}

impl Default for CallHandler {
    fn default() -> Self {
        Self::new()
    }
}
