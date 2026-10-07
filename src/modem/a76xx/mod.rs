//! A76xx modem driver — ESP32 / UART implementation.

pub mod at;
pub mod sim;

#[cfg(feature = "esp32")]
pub mod sms;

#[cfg(feature = "esp32")]
use super::creg_registered;
#[cfg(any(feature = "esp32", feature = "testing"))]
use super::{AtResponse, AtTransport, ModemDiagnostics, ModemError, ModemPort};
#[cfg(feature = "esp32")]
use at::HardwareAtPort as AtPort;
#[cfg(any(feature = "esp32", feature = "testing"))]
use std::time::Duration;

/// A76xx modem driver (A7670, A7608, A7672, etc.).
#[cfg(feature = "esp32")]
pub struct A76xxModem {
    port: AtPort,
}

#[cfg(all(feature = "testing", not(feature = "esp32")))]
pub struct A76xxModem<U: at::UartPort> {
    port: at::AtPort<U>,
}

#[cfg(feature = "esp32")]
impl A76xxModem {
    /// Create from an already-configured `AtPort`.
    pub fn new(port: AtPort) -> Self {
        A76xxModem { port }
    }

    /// Run the initialisation sequence:
    /// - Echo off, unlock SIM if needed, PDU mode, enable CMT URCs, wait for network registration.
    pub fn init(&mut self, sim_pin: &str) -> Result<(), ModemError> {
        // Probe until the modem responds to AT (up to 15 s).
        // A7670G typically takes 5-10 s after power-on to become responsive.
        let probe_deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let r = self.send_at(""); // sends "AT\r" — basic liveness check
            if r.is_ok() {
                break;
            }
            if std::time::Instant::now() > probe_deadline {
                log::error!("[a76xx] modem did not respond within 30 s");
                return Err(ModemError::Timeout);
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }

        let r = self.send_at("E0")?;
        if !r.ok {
            log::warn!("[a76xx] init ATE0 ERROR: {}", r.body.trim());
        }

        sim::ensure_sim_unlocked(self, sim_pin)?;

        match self.send_at("+CTZU=1") {
            Ok(r) if r.ok => {}
            Ok(r) => log::warn!("[a76xx] CTZU=1 ERROR: {}", r.body.trim()),
            Err(e) => log::warn!("[a76xx] CTZU=1 failed: {}", e),
        }

        for cmd in &["+CMGF=0", "+CLIP=1"] {
            let r = self.send_at(cmd)?;
            if !r.ok {
                log::warn!("[a76xx] init AT{} ERROR: {}", cmd, r.body.trim());
            }
        }

        // AT+CNMI=2,1,0,0,0 must succeed for +CMTI notifications to work.
        // On warm reboot the modem resets and its SMS subsystem may not be ready
        // when the AT probe first succeeds. Retry until accepted (up to 30 s).
        let cnmi_deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            match self.send_at("+CNMI=2,1,0,0,0") {
                Ok(r) if r.ok => break,
                Ok(r) => log::warn!("[a76xx] CNMI ERROR: {} — retrying", r.body.trim()),
                Err(e) => log::warn!("[a76xx] CNMI timeout: {} — retrying", e),
            }
            if std::time::Instant::now() > cnmi_deadline {
                log::error!(
                    "[a76xx] CNMI never accepted after 30 s — SMS notifications may not work"
                );
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }

        // Verify CNMI setting was accepted
        match self.send_at("+CNMI?") {
            Ok(r) if r.ok => {}
            Ok(r) => log::warn!("[a76xx] CNMI? error: {}", r.body.trim()),
            Err(_) => log::warn!("[a76xx] CNMI? timed out"),
        }

        // Query active storage for diagnostics. Non-fatal; some SIM/modem combos
        // return +CMS ERROR here if SMS management isn't supported.
        if let Err(error) = self.send_at("+CPMS?") {
            log::warn!("[a76xx] CPMS? failed: {}", error);
        }

        // Wait for network registration (up to 30 s)
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let r = self.send_at("+CREG?")?;
            if creg_registered(&r.body) {
                break;
            }
            if std::time::Instant::now() > deadline {
                log::warn!("[a76xx] network registration timed out — continuing anyway");
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }

        Ok(())
    }
}

#[cfg(all(feature = "testing", not(feature = "esp32")))]
impl<U: at::UartPort> A76xxModem<U> {
    /// Create a test modem from a mock UART while preserving the A76xx behavior.
    pub fn new_test(uart: U) -> Self {
        A76xxModem {
            port: at::AtPort::new(uart),
        }
    }

    pub fn port(&self) -> &at::AtPort<U> {
        &self.port
    }
}

#[cfg(any(feature = "esp32", feature = "testing"))]
fn hang_up_voice_call<T: AtTransport + ?Sized>(modem: &mut T) -> Result<(), ModemError> {
    let r = modem.send_at("+CHUP")?;
    if r.ok {
        Ok(())
    } else {
        Err(ModemError::AtError("AT+CHUP failed".into()))
    }
}

#[cfg(feature = "esp32")]
impl AtTransport for A76xxModem {
    fn send_at(&mut self, cmd: &str) -> Result<AtResponse, ModemError> {
        self.port.send_at(cmd)
    }

    fn send_at_streaming(
        &mut self,
        cmd: &str,
        idle_timeout: Duration,
        hard_timeout: Duration,
        on_line: &mut dyn FnMut(&str),
    ) -> Result<(), ModemError> {
        self.port
            .send_at_streaming(cmd, idle_timeout, hard_timeout, on_line)
    }

    fn poll_urc(&mut self) -> Option<String> {
        self.port.poll_urc()
    }

    fn write_raw(&mut self, data: &[u8]) -> Result<(), ModemError> {
        self.port.write_raw(data)
    }

    fn wait_for_prompt(&mut self, prompt: u8, timeout: Duration) -> bool {
        self.port.wait_for_prompt(prompt, timeout)
    }

    fn take_diagnostics(&mut self) -> ModemDiagnostics {
        self.port.take_diagnostics()
    }
}

#[cfg(all(feature = "testing", not(feature = "esp32")))]
impl<U: at::UartPort> AtTransport for A76xxModem<U> {
    fn send_at(&mut self, cmd: &str) -> Result<AtResponse, ModemError> {
        self.port.send_at(cmd)
    }

    fn send_at_streaming(
        &mut self,
        cmd: &str,
        idle_timeout: Duration,
        hard_timeout: Duration,
        on_line: &mut dyn FnMut(&str),
    ) -> Result<(), ModemError> {
        self.port
            .send_at_streaming(cmd, idle_timeout, hard_timeout, on_line)
    }

    fn poll_urc(&mut self) -> Option<String> {
        self.port.poll_urc()
    }

    fn write_raw(&mut self, data: &[u8]) -> Result<(), ModemError> {
        self.port.write_raw(data)
    }

    fn wait_for_prompt(&mut self, prompt: u8, timeout: Duration) -> bool {
        self.port.wait_for_prompt(prompt, timeout)
    }

    fn take_diagnostics(&mut self) -> ModemDiagnostics {
        self.port.take_diagnostics()
    }
}

#[cfg(feature = "esp32")]
impl ModemPort for A76xxModem {
    fn send_pdu_sms(&mut self, hex: &str, tpdu_len: u8) -> Result<u8, ModemError> {
        self.port.send_cmgs_pdu(hex, tpdu_len)
    }

    fn hang_up(&mut self) -> Result<(), ModemError> {
        hang_up_voice_call(self)
    }
}

#[cfg(all(feature = "testing", not(feature = "esp32")))]
impl<U: at::UartPort> ModemPort for A76xxModem<U> {
    fn send_pdu_sms(&mut self, hex: &str, tpdu_len: u8) -> Result<u8, ModemError> {
        self.port.send_cmgs_pdu(hex, tpdu_len)
    }

    fn hang_up(&mut self) -> Result<(), ModemError> {
        hang_up_voice_call(self)
    }
}
