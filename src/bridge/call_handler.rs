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
        if line == "RING" || line.starts_with("RING") {
            self.on_ring(modem, messenger, sender);
            return;
        }
        if line.starts_with("+CLIP:") {
            if let Some(number) = crate::sms::codec::parse_clip_line(line) {
                self.on_clip(number, modem, messenger, sender);
            }
            return;
        }
        if line == "NO CARRIER" {
            self.state = State::Idle;
        }
    }

    /// Drive the state machine clock — call from main loop.
    pub fn tick(
        &mut self,
        modem: &mut dyn ModemPort,
        messenger: &mut dyn MessageSink,
        sender: &mut SmsSender,
    ) {
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
                    self.commit_call(num, modem, messenger, sender);
                }
            }
            State::Cooldown { until } => {
                if Instant::now() >= *until {
                    self.state = State::Idle;
                }
            }
            State::Idle => {}
        }
    }

    fn on_ring(
        &mut self,
        _modem: &mut dyn ModemPort,
        _messenger: &mut dyn MessageSink,
        _sender: &mut SmsSender,
    ) {
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
        messenger: &mut dyn MessageSink,
        sender: &mut SmsSender,
    ) {
        if let State::Ringing { .. } = &mut self.state {
            let n = if number.is_empty() {
                None
            } else {
                Some(number)
            };
            self.commit_call(n, modem, messenger, sender);
        }
    }

    fn commit_call(
        &mut self,
        number: Option<String>,
        modem: &mut dyn ModemPort,
        messenger: &mut dyn MessageSink,
        _sender: &mut SmsSender,
    ) {
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
        if let Err(e) = messenger.send_message(&text) {
            log::error!("[call] IM notify failed: {}", e);
        }

        log::info!("[call] call from {} — hung up and notified", display);
        self.state = State::Cooldown {
            until: Instant::now() + COOLDOWN,
        };
    }
}

impl Default for CallHandler {
    fn default() -> Self {
        Self::new()
    }
}
