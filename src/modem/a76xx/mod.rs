//! A76xx modem driver — ESP32 / UART implementation.

pub mod at;
pub mod sim;

#[cfg(feature = "esp32")]
pub mod qhttp;
#[cfg(feature = "esp32")]
pub mod sms;

#[cfg(feature = "esp32")]
use super::{creg_registered, AtResponse, AtTransport, ModemError, ModemPort};
#[cfg(feature = "esp32")]
use at::HardwareAtPort as AtPort;
#[cfg(feature = "esp32")]
use std::time::Duration;

/// A76xx modem driver (A7670, A7608, A7672, etc.).
#[cfg(feature = "esp32")]
pub struct A76xxModem {
    port: AtPort,
}

#[cfg(feature = "esp32")]
impl A76xxModem {
    /// Create from an already-configured `AtPort`.
    pub fn new(port: AtPort) -> Self {
        A76xxModem { port }
    }

    pub(crate) fn port_mut(&mut self) -> &mut AtPort {
        &mut self.port
    }

    /// Run the initialisation sequence:
    /// - Echo off, unlock SIM if needed, PDU mode, enable CMT URCs, wait for network registration.
    /// - Optionally attach or detach packet-switched service (`AT+CGATT`).
    pub fn init(&mut self, cellular_data: bool, sim_pin: &str) -> Result<(), ModemError> {
        // Probe until the modem responds to AT (up to 15 s).
        // A7670G typically takes 5-10 s after power-on to become responsive.
        let probe_deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let r = self.send_at(""); // sends "AT\r" — basic liveness check
            if r.is_ok() {
                log::info!("[a76xx] modem responded to AT probe");
                break;
            }
            if std::time::Instant::now() > probe_deadline {
                log::error!("[a76xx] modem did not respond within 30 s");
                return Err(ModemError::Timeout);
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }

        let r = self.send_at("E0")?;
        if r.ok {
            log::info!("[a76xx] init ATE0 OK");
        } else {
            log::warn!("[a76xx] init ATE0 ERROR: {}", r.body.trim());
        }

        sim::ensure_sim_unlocked(self, sim_pin)?;

        match self.send_at("+CTZU=1") {
            Ok(r) if r.ok => log::info!("[a76xx] network time update enabled (CTZU=1)"),
            Ok(r) => log::warn!("[a76xx] CTZU=1 ERROR: {}", r.body.trim()),
            Err(e) => log::warn!("[a76xx] CTZU=1 failed: {}", e),
        }

        for cmd in &["+CMGF=0", "+CLIP=1"] {
            let r = self.send_at(cmd)?;
            if r.ok {
                log::info!("[a76xx] init AT{} OK", cmd);
            } else {
                log::warn!("[a76xx] init AT{} ERROR: {}", cmd, r.body.trim());
            }
        }

        // AT+CNMI=2,1,0,0,0 must succeed for +CMTI notifications to work.
        // On warm reboot the modem resets and its SMS subsystem may not be ready
        // when the AT probe first succeeds. Retry until accepted (up to 30 s).
        let cnmi_deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            match self.send_at("+CNMI=2,1,0,0,0") {
                Ok(r) if r.ok => {
                    log::info!("[a76xx] CNMI set OK");
                    break;
                }
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
            Ok(r) if r.ok => log::info!("[a76xx] CNMI: {}", r.body.trim()),
            Ok(r) => log::warn!("[a76xx] CNMI? error: {}", r.body.trim()),
            Err(_) => log::warn!("[a76xx] CNMI? timed out"),
        }

        // Query active storage for diagnostics. Non-fatal; some SIM/modem combos
        // return +CMS ERROR here if SMS management isn't supported.
        match self.send_at("+CPMS?") {
            Ok(r) if r.ok => log::info!("[a76xx] CPMS: {}", r.body.trim()),
            Ok(r) => log::debug!("[a76xx] CPMS? not supported: {}", r.body.trim()),
            Err(_) => log::debug!("[a76xx] CPMS? timed out"),
        }

        // Wait for network registration (up to 30 s)
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let r = self.send_at("+CREG?")?;
            if creg_registered(&r.body) {
                log::info!("[a76xx] network registered");
                break;
            }
            if std::time::Instant::now() > deadline {
                log::warn!("[a76xx] network registration timed out — continuing anyway");
                break;
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }

        // Only send AT+CGATT=1 when cellular data is explicitly requested.
        // AT+CGATT=0 (detach) is unreliable on A7670G — the modem frequently
        // doesn't respond within CMD_TIMEOUT, causing a 5 s stall at boot.
        // SMS delivery works without touching CGATT.
        if cellular_data {
            match self.send_at("+CGATT=1") {
                Ok(r) if r.ok => log::info!("[a76xx] cellular data enabled (AT+CGATT=1 OK)"),
                Ok(r) => log::warn!("[a76xx] AT+CGATT=1: {}", r.body.trim()),
                Err(e) => log::warn!("[a76xx] AT+CGATT=1 failed: {}", e),
            }
        }
        Ok(())
    }
}

#[cfg(feature = "esp32")]
impl AtTransport for A76xxModem {
    fn send_at(&mut self, cmd: &str) -> Result<AtResponse, ModemError> {
        self.port.send_at(cmd)
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
}

#[cfg(feature = "esp32")]
impl ModemPort for A76xxModem {
    // send_pdu_sms: default (standard AT+CMGS handshake via AtTransport)
    // hang_up: default (ATH)

    fn post_telegram_https(&mut self, path: &str, json: &str) -> Result<String, ModemError> {
        qhttp::post_json(self, path, json)
    }
}
