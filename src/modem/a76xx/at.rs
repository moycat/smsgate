//! Raw AT send/receive over a UART-like byte port.

use crate::modem::{AtResponse, ModemDiagnostics, ModemError};
use std::time::{Duration, Instant};

/// FreeRTOS ticks to block waiting for a byte.
/// 10 ticks ≈ 10 ms at the default 1 kHz tick rate.
/// Blocking yields the CPU to the IDLE task and prevents the Task WDT from firing.
const UART_READ_TICKS: u32 = 10;

const CMD_TIMEOUT: Duration = Duration::from_secs(5);
// A76XX status queries can take up to 9 s according to the AT command manual.
const STATUS_QUERY_TIMEOUT: Duration = Duration::from_secs(10);
const SMS_STORAGE_TIMEOUT: Duration = Duration::from_secs(20);
const SMS_SETUP_TIMEOUT: Duration = Duration::from_secs(10);
const READLINE_TIMEOUT: Duration = Duration::from_millis(500);
const CMGS_PROMPT_TIMEOUT: Duration = Duration::from_secs(10);
const CMGS_RESULT_TIMEOUT: Duration = Duration::from_secs(60);
const RESYNC_TERMINAL_WAIT: Duration = Duration::from_secs(2);
const RESYNC_PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const URC_DRAIN_BUDGET: Duration = Duration::from_millis(100);
const MAX_URC_DRAIN_LINES: usize = 32;
/// Maximum buffered URC lines. Prevents unbounded queue growth on UART noise flood.
const MAX_URC_BUF: usize = 32;
/// Maximum response body lines collected per AT command.
/// A well-formed modem never sends more; caps UART garbage.
const MAX_BODY_LINES: usize = 64;
const MAX_BODY_BYTES: usize = 16 * 1024;
const INITIAL_LINE_CAPACITY: usize = 64;
/// Bound an unterminated UART line before it can exhaust the ESP32 heap.
const MAX_LINE_LEN: usize = 1024;
const CMT_BODY_TIMEOUT: Duration = Duration::from_secs(5);

fn command_timeout(cmd: &str) -> Duration {
    if ["+CPMS", "+CMGR=", "+CMGD="]
        .iter()
        .any(|prefix| cmd.starts_with(prefix))
    {
        SMS_STORAGE_TIMEOUT
    } else if cmd == "+CMGF=0" || cmd.starts_with("+CNMI") {
        SMS_SETUP_TIMEOUT
    } else if matches!(
        cmd,
        "+CSQ"
            | "+CREG?"
            | "+CGREG?"
            | "+CEREG?"
            | "+COPS?"
            | "+CPSI?"
            | "+CGATT?"
            | "+CGACT?"
            | "+CGDCONT?"
            | "+CIREG?"
    ) {
        STATUS_QUERY_TIMEOUT
    } else {
        CMD_TIMEOUT
    }
}

/// Byte-level UART abstraction — `UartDriver` on hardware, `MockUart` in tests.
pub trait UartPort {
    /// Read one byte. `ticks` is a FreeRTOS tick hint (ignored outside RTOS).
    /// Returns `None` if no byte is available within the tick window.
    fn read_byte(&mut self, ticks: u32) -> Option<u8>;
    fn write_all(&mut self, data: &[u8]) -> Result<(), ModemError>;
}

/// Low-level AT command port.
pub struct AtPort<U: UartPort> {
    uart: U,
    urc_buf: std::collections::VecDeque<String>,
    partial_line: String,
    discarding_overlong_line: bool,
    pending_cmt_header: Option<(String, Instant)>,
    diagnostics: ModemDiagnostics,
    response_pending: bool,
    timed_out_cmgf_query: bool,
}

impl<U: UartPort> AtPort<U> {
    pub fn new(uart: U) -> Self {
        AtPort {
            uart,
            urc_buf: std::collections::VecDeque::new(),
            partial_line: String::with_capacity(INITIAL_LINE_CAPACITY),
            discarding_overlong_line: false,
            pending_cmt_header: None,
            diagnostics: ModemDiagnostics::default(),
            response_pending: false,
            timed_out_cmgf_query: false,
        }
    }

    /// Return and clear receive-fault counts since the previous read.
    pub fn take_diagnostics(&mut self) -> ModemDiagnostics {
        std::mem::take(&mut self.diagnostics)
    }

    /// Access the underlying UART (useful in tests to inspect sent bytes).
    #[cfg(feature = "testing")]
    pub fn inner(&self) -> &U {
        &self.uart
    }

    /// Send "AT<cmd>\r" and collect lines until OK/ERROR/timeout.
    pub fn send_at(&mut self, cmd: &str) -> Result<AtResponse, ModemError> {
        self.send_at_with_timeout(cmd, command_timeout(cmd))
    }

    /// Send "AT<cmd>\r" with a caller-provided timeout.
    pub fn send_at_with_timeout(
        &mut self,
        cmd: &str,
        timeout: Duration,
    ) -> Result<AtResponse, ModemError> {
        self.prepare_command()?;

        let command = format!("AT{}\r", cmd);
        self.uart.write_all(command.as_bytes())?;

        let deadline = Instant::now() + timeout;
        let mut body = ResponseBody::new();
        let interruptible = ["+CPMS", "+CMGR=", "+CMGD="]
            .iter()
            .any(|prefix| cmd.starts_with(prefix));
        let terminal = self.collect_until_ok(deadline, &mut body, true, interruptible);
        if matches!(
            terminal,
            Err(ModemError::Timeout | ModemError::InterruptedForCall)
        ) {
            self.mark_timed_out(cmd);
        }
        if let Some(err) = terminal? {
            return Ok(err);
        }
        if body.truncated {
            return Err(ModemError::ResponseTooLong);
        }
        Ok(AtResponse {
            body: body.into_string(),
            ok: true,
        })
    }

    /// Stream a large AT response without accumulating its body in RAM.
    /// The idle timer resets only on command response lines, not on URC noise.
    pub fn send_at_streaming(
        &mut self,
        cmd: &str,
        idle_timeout: Duration,
        hard_timeout: Duration,
        on_line: &mut dyn FnMut(&str),
    ) -> Result<(), ModemError> {
        self.prepare_command()?;
        self.uart.write_all(format!("AT{}\r", cmd).as_bytes())?;

        let hard_deadline = Instant::now() + hard_timeout;
        let mut idle_deadline = Instant::now() + idle_timeout;
        loop {
            reset_task_watchdog();
            let now = Instant::now();
            let Some(remaining) = hard_deadline
                .checked_duration_since(now)
                .zip(idle_deadline.checked_duration_since(now))
                .map(|(hard, idle)| hard.min(idle).min(READLINE_TIMEOUT))
            else {
                self.mark_timed_out(cmd);
                return Err(ModemError::Timeout);
            };
            let Some(line) = self.read_line(remaining).and_then(normalize_line) else {
                continue;
            };
            if self.route_cmt_line(&line) {
                continue;
            }
            if line == "OK" {
                return Ok(());
            }
            if is_at_error(&line) {
                return Err(ModemError::AtError(line));
            }
            if urc::is_urc(&line) {
                let incoming_call = cmd.starts_with("+CMGL=")
                    && (line.starts_with("RING") || line.starts_with("+CLIP:"));
                self.queue_urc(line);
                if incoming_call {
                    // A long storage listing must not hide a call for up to
                    // its two-minute hard timeout. Resync before the next AT
                    // command because the listing may still finish later.
                    self.mark_timed_out(cmd);
                    return Err(ModemError::InterruptedForCall);
                }
                continue;
            }
            on_line(&line);
            idle_deadline = Instant::now() + idle_timeout;
        }
    }

    /// Non-blocking: drain one URC line if available.
    pub fn poll_urc(&mut self) -> Option<String> {
        self.expire_pending_cmt_header();
        if let Some(urc) = self.urc_buf.pop_front() {
            return Some(urc);
        }
        if self.response_pending {
            if let Some(line) = self
                .read_line(Duration::from_millis(10))
                .and_then(normalize_line)
            {
                self.discard_stale_response_line(line);
            }
            return self.urc_buf.pop_front();
        }
        let deadline = Instant::now() + Duration::from_millis(10);
        loop {
            let remaining = deadline.checked_duration_since(Instant::now())?;
            let line = normalize_line(self.read_line(remaining)?)?;
            if self.route_cmt_line(&line) {
                if let Some(urc) = self.urc_buf.pop_front() {
                    return Some(urc);
                }
                continue;
            }
            return Some(line);
        }
    }

    // ---- private ----

    fn mark_timed_out(&mut self, cmd: &str) {
        self.response_pending = true;
        self.timed_out_cmgf_query = cmd == "+CMGF?";
    }

    fn prepare_command(&mut self) -> Result<(), ModemError> {
        if self.response_pending {
            self.resynchronize()?;
        }
        self.drain_urcs();
        Ok(())
    }

    fn discard_stale_response_line(&mut self, line: String) {
        if self.route_cmt_line(&line) {
            return;
        }
        if line == "OK" || is_at_error(&line) {
            self.response_pending = false;
        } else if urc::is_urc(&line) {
            self.queue_urc(line);
        }
    }

    /// A timed-out command can still emit a late OK. Consume its terminal
    /// before another command, or require a distinctive probe response so a
    /// stale OK cannot be mistaken for the next command's completion.
    fn resynchronize(&mut self) -> Result<(), ModemError> {
        let terminal_deadline = Instant::now() + RESYNC_TERMINAL_WAIT;
        while self.response_pending {
            reset_task_watchdog();
            let Some(remaining) = terminal_deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            if let Some(line) = self
                .read_line(remaining.min(READLINE_TIMEOUT))
                .and_then(normalize_line)
            {
                self.discard_stale_response_line(line);
            }
        }
        if !self.response_pending {
            return Ok(());
        }

        let (probe, marker) = if self.timed_out_cmgf_query {
            ("+CSQ", "+CSQ:")
        } else {
            ("+CMGF?", "+CMGF:")
        };
        self.uart.write_all(format!("AT{}\r", probe).as_bytes())?;
        self.timed_out_cmgf_query = probe == "+CMGF?";
        let deadline = Instant::now() + RESYNC_PROBE_TIMEOUT;
        let mut saw_marker = false;
        loop {
            reset_task_watchdog();
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(ModemError::NotReady);
            };
            let Some(line) = self
                .read_line(remaining.min(READLINE_TIMEOUT))
                .and_then(normalize_line)
            else {
                continue;
            };
            if self.route_cmt_line(&line) {
                continue;
            }
            if line.starts_with(marker) {
                saw_marker = true;
            } else if line == "OK" && saw_marker {
                self.response_pending = false;
                return Ok(());
            } else if urc::is_urc(&line) {
                self.queue_urc(line);
            }
        }
    }

    fn drain_urcs(&mut self) {
        // Bound both elapsed time and line count: a noisy modem must not keep
        // the modem owner inside prepare_command indefinitely.
        let deadline = Instant::now() + URC_DRAIN_BUDGET;
        for _ in 0..MAX_URC_DRAIN_LINES {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            let Some(line) = self.read_line(remaining.min(Duration::from_millis(20))) else {
                break;
            };
            if let Some(line) = normalize_line(line) {
                if !self.route_cmt_line(&line) {
                    self.queue_urc(line);
                }
            }
        }
    }

    /// Read response lines until `OK` or an error terminal, respecting `deadline`.
    /// Returns `Ok(response)` on `OK`, `Ok(error_response)` on ERROR, `Err(Timeout)` on deadline.
    fn collect_until_ok(
        &mut self,
        deadline: Instant,
        body: &mut ResponseBody,
        kick_wdt: bool,
        interruptible: bool,
    ) -> Result<Option<AtResponse>, ModemError> {
        loop {
            if kick_wdt {
                reset_task_watchdog();
            }
            let Some(read_timeout) = deadline
                .checked_duration_since(Instant::now())
                .map(|remaining| remaining.min(READLINE_TIMEOUT))
            else {
                return Err(ModemError::Timeout);
            };
            if let Some(line) = self.read_line(read_timeout) {
                let Some(line) = normalize_line(line) else {
                    continue;
                };
                if line.is_empty() {
                    continue;
                }
                if self.route_cmt_line(&line) {
                    continue;
                }
                if line == "OK" {
                    return Ok(None);
                }
                if is_at_error(&line) {
                    return Ok(Some(AtResponse {
                        body: line,
                        ok: false,
                    }));
                }
                if interruptible && (line.starts_with("RING") || line.starts_with("+CLIP:")) {
                    self.queue_urc(line);
                    return Err(ModemError::InterruptedForCall);
                }
                self.buffer_line(line, body);
            }
        }
    }

    fn read_line(&mut self, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        loop {
            if Instant::now() >= deadline {
                return None;
            }
            let Some(c) = self.uart.read_byte(UART_READ_TICKS) else {
                continue;
            };
            if let Some(line) = self.consume_line_byte(c) {
                return Some(line);
            }
        }
    }

    fn consume_line_byte(&mut self, c: u8) -> Option<String> {
        if c == b'\n' {
            if self.discarding_overlong_line {
                self.discarding_overlong_line = false;
                return None;
            }
            return Some(std::mem::replace(
                &mut self.partial_line,
                String::with_capacity(INITIAL_LINE_CAPACITY),
            ));
        }
        if c == b'\r' || self.discarding_overlong_line {
            return None;
        }
        if self.partial_line.len() + (c as char).len_utf8() > MAX_LINE_LEN {
            self.partial_line = String::with_capacity(INITIAL_LINE_CAPACITY);
            self.discarding_overlong_line = true;
            self.diagnostics.overlong_lines = self.diagnostics.overlong_lines.saturating_add(1);
            self.drop_pending_cmt_header();
            log::warn!("[at] overlong UART line discarded");
        } else {
            self.partial_line.push(c as char);
        }
        None
    }

    /// Route a non-terminal response line into either the URC buffer or the
    /// command body accumulator, respecting both caps.
    fn buffer_line(&mut self, line: String, body: &mut ResponseBody) {
        if urc::is_urc(&line) {
            self.queue_urc(line);
        } else if body.line_count() < MAX_BODY_LINES
            && body.text.len() + line.len() + usize::from(!body.text.is_empty()) <= MAX_BODY_BYTES
        {
            body.push(line);
        } else {
            self.diagnostics.dropped_response_lines =
                self.diagnostics.dropped_response_lines.saturating_add(1);
            body.truncated = true;
            log::warn!("[at] body cap exceeded — discarding: {}", line);
        }
    }

    /// Hold a +CMT header until its PDU arrives so the pair cannot be split by
    /// a command response or by the bounded URC queue.
    fn route_cmt_line(&mut self, line: &str) -> bool {
        self.expire_pending_cmt_header();

        if line.starts_with("+CMT:") {
            self.drop_pending_cmt_header();
            self.pending_cmt_header = Some((line.to_string(), Instant::now()));
            return true;
        }

        if self.pending_cmt_header.is_some() && is_pdu_hex_line(line) {
            let (header, _) = self.pending_cmt_header.take().unwrap();
            if self.reserve_urc_slots(2) {
                self.urc_buf.push_back(header);
                self.urc_buf.push_back(line.to_string());
            } else {
                self.diagnostics.dropped_urcs = self.diagnostics.dropped_urcs.saturating_add(1);
                log::warn!("[at] URC queue full — discarding +CMT delivery");
            }
            return true;
        }

        false
    }

    fn expire_pending_cmt_header(&mut self) {
        if self
            .pending_cmt_header
            .as_ref()
            .is_some_and(|(_, since)| since.elapsed() > CMT_BODY_TIMEOUT)
        {
            self.drop_pending_cmt_header();
        }
    }

    fn drop_pending_cmt_header(&mut self) {
        if self.pending_cmt_header.take().is_some() {
            self.diagnostics.dropped_urcs = self.diagnostics.dropped_urcs.saturating_add(1);
            log::warn!("[at] incomplete +CMT delivery discarded");
        }
    }

    fn queue_urc(&mut self, line: String) {
        // +CGEV can flap rapidly. Keep only its newest value and reserve queue
        // space for SMS and call notifications, which cannot be reconstructed.
        if line.starts_with("+CGEV:") {
            if let Some(existing) = self.urc_buf.iter_mut().find(|s| s.starts_with("+CGEV:")) {
                *existing = line;
            } else if self.urc_buf.len() < MAX_URC_BUF - 1 {
                self.urc_buf.push_back(line);
            }
            return;
        }

        if self.reserve_urc_slots(1) {
            self.urc_buf.push_back(line);
        } else {
            self.diagnostics.dropped_urcs = self.diagnostics.dropped_urcs.saturating_add(1);
            log::warn!("[at] URC queue full — discarding: {}", line);
        }
    }

    fn reserve_urc_slots(&mut self, slots: usize) -> bool {
        while self.urc_buf.len() + slots > MAX_URC_BUF {
            if let Some(index) = self.urc_buf.iter().position(|s| s.starts_with("+CGEV:")) {
                self.urc_buf.remove(index);
                continue;
            }
            // Stored SMS can be recovered by the periodic storage sweep;
            // ringing calls and direct deliveries cannot. Prefer the latter.
            if let Some(index) = self.urc_buf.iter().position(|s| s.starts_with("+CMTI:")) {
                self.urc_buf.remove(index);
                self.diagnostics.dropped_urcs = self.diagnostics.dropped_urcs.saturating_add(1);
                log::warn!("[at] URC queue full — evicting recoverable +CMTI");
                continue;
            }
            return false;
        }
        true
    }

    /// Write raw bytes to UART (used for AT+CMGS PDU send).
    pub fn write_raw(&mut self, data: &[u8]) -> Result<(), ModemError> {
        self.uart.write_all(data)
    }

    /// Send an SMS PDU without consuming concurrent SMS/call URCs as CMGS output.
    pub fn send_cmgs_pdu(&mut self, hex: &str, tpdu_len: u8) -> Result<u8, ModemError> {
        self.prepare_command()?;
        self.uart
            .write_all(format!("AT+CMGS={}\r", tpdu_len).as_bytes())?;

        let prompt_deadline = Instant::now() + CMGS_PROMPT_TIMEOUT;
        loop {
            reset_task_watchdog();
            if Instant::now() >= prompt_deadline {
                self.abort_cmgs_input();
                self.mark_timed_out("+CMGS");
                return Err(ModemError::Timeout);
            }
            let Some(c) = self.uart.read_byte(UART_READ_TICKS) else {
                continue;
            };
            if c == b'>' {
                break;
            }
            if let Some(line) = self.consume_line_byte(c).and_then(normalize_line) {
                if is_at_error(&line) {
                    self.abort_cmgs_input();
                    return Err(ModemError::AtError(line));
                }
                if line == "OK" {
                    self.abort_cmgs_input();
                    return Err(ModemError::AtError("CMGS prompt not received".into()));
                }
                if !self.route_cmt_line(&line) {
                    self.queue_urc(line);
                }
            }
        }

        let mut payload = Vec::with_capacity(hex.len() + 1);
        payload.extend_from_slice(hex.as_bytes());
        payload.push(0x1A);
        if let Err(e) = self.uart.write_all(&payload) {
            self.abort_cmgs_input();
            return Err(e);
        }

        let mut body = ResponseBody::new();
        let result_deadline = Instant::now() + CMGS_RESULT_TIMEOUT;
        let terminal = match self.collect_until_ok(result_deadline, &mut body, true, false) {
            Err(ModemError::Timeout) => {
                log::warn!("[at] CMGS final result timed out");
                self.mark_timed_out("+CMGS");
                return Err(ModemError::Timeout);
            }
            other => other?,
        };
        if let Some(error) = terminal {
            return Err(ModemError::AtError(error.body));
        }
        if body.truncated {
            return Err(ModemError::ResponseTooLong);
        }
        let mr = body
            .into_string()
            .lines()
            .find_map(|line| line.strip_prefix("+CMGS:"))
            .and_then(|value| value.trim().parse().ok())
            .unwrap_or(0);
        Ok(mr)
    }

    fn abort_cmgs_input(&mut self) {
        if let Err(e) = self.uart.write_all(&[0x1B]) {
            log::warn!("[at] failed to abort CMGS input: {}", e);
        }
    }

    /// Read until `prompt` byte or timeout (used for AT+CMGS '>' prompt).
    pub fn wait_for_prompt(&mut self, prompt: u8, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if Instant::now() > deadline {
                return false;
            }
            if let Some(b) = self.uart.read_byte(UART_READ_TICKS) {
                if b == prompt {
                    return true;
                }
            }
        }
    }
}

struct ResponseBody {
    text: String,
    lines: usize,
    truncated: bool,
}

impl ResponseBody {
    fn new() -> Self {
        Self {
            text: String::new(),
            lines: 0,
            truncated: false,
        }
    }

    fn push(&mut self, line: String) {
        if !self.text.is_empty() {
            self.text.push('\n');
        }
        self.text.push_str(&line);
        self.lines += 1;
    }

    fn line_count(&self) -> usize {
        self.lines
    }

    fn into_string(self) -> String {
        self.text
    }
}

fn normalize_line(mut line: String) -> Option<String> {
    trim_ascii_in_place(&mut line);
    (!line.is_empty()).then_some(line)
}

fn is_at_error(line: &str) -> bool {
    line.starts_with("ERROR") || line.starts_with("+CME ERROR") || line.starts_with("+CMS ERROR")
}

fn reset_task_watchdog() {
    #[cfg(feature = "esp32")]
    unsafe {
        // SAFETY: This only resets the watchdog timer for the current task.
        // An unregistered task receives an ESP-IDF error without changing state.
        esp_idf_sys::esp_task_wdt_reset();
    }
}

fn is_pdu_hex_line(line: &str) -> bool {
    !line.is_empty() && line.len().is_multiple_of(2) && line.bytes().all(|b| b.is_ascii_hexdigit())
}

fn trim_ascii_in_place(value: &mut String) {
    let bytes = value.as_bytes();
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map(|idx| idx + 1)
        .unwrap_or(start);

    if end < value.len() {
        value.truncate(end);
    }
    if start > 0 {
        value.drain(..start);
    }
}

/// `UartDriver` implementation for real hardware.
#[cfg(feature = "esp32")]
impl UartPort for esp_idf_hal::uart::UartDriver<'static> {
    fn read_byte(&mut self, ticks: u32) -> Option<u8> {
        let mut buf = [0u8; 1];
        match self.read(&mut buf, ticks) {
            Ok(1) => Some(buf[0]),
            _ => None,
        }
    }

    fn write_all(&mut self, data: &[u8]) -> Result<(), ModemError> {
        use esp_idf_hal::io::Write;
        Write::write_all(self, data).map_err(|_| ModemError::Io)
    }
}

/// Concrete `AtPort` type alias for hardware use.
#[cfg(feature = "esp32")]
pub type HardwareAtPort = AtPort<esp_idf_hal::uart::UartDriver<'static>>;

use crate::modem::urc;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slow_sms_storage_commands_have_separate_timeouts() {
        for command in ["+CPMS=\"ME\"", "+CPMS?", "+CMGR=1", "+CMGD=1"] {
            assert_eq!(command_timeout(command), SMS_STORAGE_TIMEOUT);
        }
        assert_eq!(command_timeout("+CNMI?"), SMS_SETUP_TIMEOUT);
        assert_eq!(command_timeout("+CMGF=0"), SMS_SETUP_TIMEOUT);
    }

    #[test]
    fn status_query_timeouts_allow_the_documented_response_window() {
        assert!(STATUS_QUERY_TIMEOUT >= Duration::from_secs(9));
        for command in [
            "+CSQ",
            "+CREG?",
            "+CGREG?",
            "+CEREG?",
            "+COPS?",
            "+CPSI?",
            "+CGATT?",
            "+CGACT?",
            "+CGDCONT?",
            "+CIREG?",
        ] {
            assert_eq!(command_timeout(command), STATUS_QUERY_TIMEOUT);
        }
        for command in ["", "+CREG=0", "+COPS=?", "+CGATT=1", "+CGACT=0,1"] {
            assert_eq!(command_timeout(command), CMD_TIMEOUT);
        }
    }
}
