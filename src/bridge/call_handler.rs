//! Incoming call state machine: Idle → Ringing → Cooldown.

use crate::im::MessageSink;
use crate::modem::ModemPort;
use crate::sms::sender::SmsSender;
use std::time::{Duration, Instant};

const CLIP_DEADLINE: Duration = Duration::from_millis(1500);
const COOLDOWN: Duration = Duration::from_secs(6);

#[derive(Debug, PartialEq)]
enum State {
    Idle,
    Ringing {
        since: Instant,
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

impl CallHandler {
    pub fn new() -> Self {
        CallHandler { state: State::Idle }
    }

    /// Feed a URC line. Call this for every line from `modem.poll_urc()`.
    pub fn handle_urc(
        &mut self,
        line: &str,
        modem: &mut dyn ModemPort,
        messenger: &mut dyn MessageSink,
        sender: &mut SmsSender,
    ) {
        if let Some(text) = self.handle_urc_deferred(line, modem, sender) {
            if let Err(e) = messenger.send_message(&text) {
                log::error!("[call] IM notify failed: {}", e);
            }
        }
    }

    /// Feed a URC line while deferring IM delivery until after the modem lock is released.
    pub fn handle_urc_deferred(
        &mut self,
        line: &str,
        modem: &mut dyn ModemPort,
        sender: &mut SmsSender,
    ) -> Option<String> {
        if line == "RING" || line.starts_with("RING") {
            self.on_ring(modem, sender);
            return None;
        }
        if line.starts_with("+CLIP:") {
            if let Some(number) = crate::sms::codec::parse_clip_line(line) {
                return self.on_clip(number, modem, sender);
            }
            return None;
        }
        if line == "NO CARRIER" {
            self.state = State::Idle;
        }
        None
    }

    /// Drive the state machine clock — call from main loop.
    pub fn tick(
        &mut self,
        modem: &mut dyn ModemPort,
        messenger: &mut dyn MessageSink,
        sender: &mut SmsSender,
    ) {
        if let Some(text) = self.tick_deferred(modem, sender) {
            if let Err(e) = messenger.send_message(&text) {
                log::error!("[call] IM notify failed: {}", e);
            }
        }
    }

    /// Drive the state machine clock while deferring IM delivery.
    pub fn tick_deferred(
        &mut self,
        modem: &mut dyn ModemPort,
        sender: &mut SmsSender,
    ) -> Option<String> {
        match &self.state {
            State::Ringing {
                clip_deadline,
                number,
                since: _,
            } => {
                let deadline = *clip_deadline;
                let num = number.clone();
                if Instant::now() >= deadline {
                    // CLIP not received in time — commit with unknown number
                    return self.commit_call(num, modem, sender);
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

    fn on_ring(&mut self, _modem: &mut dyn ModemPort, _sender: &mut SmsSender) {
        if matches!(self.state, State::Cooldown { .. }) {
            return; // suppress duplicate ring in cooldown window
        }
        if matches!(self.state, State::Idle) {
            self.state = State::Ringing {
                since: Instant::now(),
                clip_deadline: Instant::now() + CLIP_DEADLINE,
                number: None,
            };
        }
    }

    fn on_clip(
        &mut self,
        number: String,
        modem: &mut dyn ModemPort,
        sender: &mut SmsSender,
    ) -> Option<String> {
        if let State::Ringing { .. } = &mut self.state {
            let n = if number.is_empty() {
                None
            } else {
                Some(number)
            };
            return self.commit_call(n, modem, sender);
        }
        None
    }

    fn commit_call(
        &mut self,
        number: Option<String>,
        modem: &mut dyn ModemPort,
        _sender: &mut SmsSender,
    ) -> Option<String> {
        // Auto-hang-up
        if let Err(e) = modem.hang_up() {
            log::warn!("[call] hang_up failed: {}", e);
        }

        // Notify via IM
        let display = match &number {
            Some(n) if !n.is_empty() => crate::sms::codec::human_readable_phone(n),
            _ => "unknown caller".to_string(),
        };
        let text = crate::i18n::incoming_call(&display);

        log::info!("[call] call from {} — hung up and notified", display);
        self.state = State::Cooldown {
            until: Instant::now() + COOLDOWN,
        };
        Some(text)
    }
}

impl Default for CallHandler {
    fn default() -> Self {
        Self::new()
    }
}
