//! Raw AT send/receive over a UART-like byte port.

use crate::modem::{AtResponse, ModemError};
use std::time::{Duration, Instant};

/// FreeRTOS ticks to block waiting for a byte.
/// 10 ticks ≈ 10 ms at the default 1 kHz tick rate.
/// Blocking yields the CPU to the IDLE task and prevents the Task WDT from firing.
const UART_READ_TICKS: u32 = 10;

const CMD_TIMEOUT: Duration = Duration::from_secs(5);
const READLINE_TIMEOUT: Duration = Duration::from_millis(500);
/// Maximum buffered URC lines. Prevents unbounded queue growth on UART noise flood.
const MAX_URC_BUF: usize = 32;
/// Maximum response body lines collected per AT command.
/// A well-formed modem never sends more; caps UART garbage.
const MAX_BODY_LINES: usize = 64;

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
}

impl<U: UartPort> AtPort<U> {
    pub fn new(uart: U) -> Self {
        AtPort {
            uart,
            urc_buf: std::collections::VecDeque::new(),
        }
    }

    /// Access the underlying UART (useful in tests to inspect sent bytes).
    #[cfg(feature = "testing")]
    pub fn inner(&self) -> &U {
        &self.uart
    }

    /// Send "AT<cmd>\r" and collect lines until OK/ERROR/timeout.
    pub fn send_at(&mut self, cmd: &str) -> Result<AtResponse, ModemError> {
        self.drain_urcs();

        let command = format!("AT{}\r", cmd);
        self.uart.write_all(command.as_bytes())?;

        let deadline = Instant::now() + CMD_TIMEOUT;
        let mut body_lines: Vec<String> = Vec::new();
        if let Some(err) = self.collect_until_ok(deadline, &mut body_lines)? {
            return Ok(err);
        }
        Ok(AtResponse {
            body: body_lines.join("\n"),
            ok: true,
        })
    }

    /// Non-blocking: drain one URC line if available.
    pub fn poll_urc(&mut self) -> Option<String> {
        if let Some(urc) = self.urc_buf.pop_front() {
            return Some(urc);
        }
        let line = self.read_line(Duration::from_millis(10))?;
        let line = line.trim().to_string();
        if line.is_empty() {
            return None;
        }
        Some(line)
    }

    // ---- private ----

    fn drain_urcs(&mut self) {
        // Short window collects bytes already waiting in the UART FIFO.
        while let Some(line) = self.read_line(Duration::from_millis(20)) {
            let line = line.trim().to_string();
            if !line.is_empty() {
                if self.urc_buf.len() < MAX_URC_BUF {
                    self.urc_buf.push_back(line);
                } else {
                    log::warn!("[at] urc_buf full — discarding: {}", line);
                }
            }
        }
    }

    /// Read response lines until `OK` or an error terminal, respecting `deadline`.
    /// Returns `Ok(response)` on `OK`, `Ok(error_response)` on ERROR, `Err(Timeout)` on deadline.
    fn collect_until_ok(
        &mut self,
        deadline: Instant,
        body_lines: &mut Vec<String>,
    ) -> Result<Option<AtResponse>, ModemError> {
        loop {
            if Instant::now() > deadline {
                return Err(ModemError::Timeout);
            }
            if let Some(line) = self.read_line(READLINE_TIMEOUT) {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                if line == "OK" {
                    return Ok(None);
                }
                if line.starts_with("ERROR")
                    || line.starts_with("+CME ERROR")
                    || line.starts_with("+CMS ERROR")
                {
                    return Ok(Some(AtResponse {
                        body: line,
                        ok: false,
                    }));
                }
                self.buffer_line(line, body_lines);
            }
        }
    }

    fn read_line(&mut self, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        let mut line = String::new();
        loop {
            if Instant::now() > deadline {
                if !line.is_empty() {
                    return Some(line);
                }
                return None;
            }
            let Some(c) = self.uart.read_byte(UART_READ_TICKS) else {
                continue;
            };
            if c == b'\n' {
                return Some(line);
            }
            if c != b'\r' {
                line.push(c as char);
            }
        }
    }

    /// Route a non-terminal response line into either the URC buffer or the
    /// command body accumulator, respecting both caps.
    fn buffer_line(&mut self, line: String, body: &mut Vec<String>) {
        if urc::is_urc(&line) {
            if self.urc_buf.len() < MAX_URC_BUF {
                self.urc_buf.push_back(line);
            }
        } else if body.len() < MAX_BODY_LINES {
            body.push(line);
        } else {
            log::warn!("[at] body cap exceeded — discarding: {}", line);
        }
    }

    /// Write raw bytes to UART (used for AT+CMGS PDU send).
    pub fn write_raw(&mut self, data: &[u8]) -> Result<(), ModemError> {
        self.uart.write_all(data)
    }

    /// Send a command whose response includes `CONNECT`, write `payload`, then read until `OK`.
    /// Used for Quectel `AT+QHTTPURL` / `AT+QHTTPPOST` data phases.
    pub fn send_at_connect_payload(
        &mut self,
        cmd: &str,
        payload: &str,
    ) -> Result<AtResponse, ModemError> {
        self.drain_urcs();
        let command = format!("AT{}\r", cmd);
        self.uart.write_all(command.as_bytes())?;

        let deadline = Instant::now() + Duration::from_secs(90);
        let mut body_lines: Vec<String> = Vec::new();

        loop {
            if Instant::now() > deadline {
                return Err(ModemError::Timeout);
            }
            if let Some(line) = self.read_line(READLINE_TIMEOUT) {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                if line.contains("CONNECT") {
                    break;
                }
                if line == "OK" || line.starts_with("ERROR") || line.starts_with("+CME ERROR") {
                    return Ok(AtResponse {
                        body: line.clone(),
                        ok: line == "OK",
                    });
                }
                self.buffer_line(line, &mut body_lines);
            }
        }

        let pl = format!(
            "{}\r\n",
            payload.trim_end_matches('\r').trim_end_matches('\n')
        );
        self.uart.write_all(pl.as_bytes())?;

        body_lines.clear();
        if let Some(err) = self.collect_until_ok(deadline, &mut body_lines)? {
            return Ok(err);
        }
        Ok(AtResponse {
            body: body_lines.join("\n"),
            ok: true,
        })
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
