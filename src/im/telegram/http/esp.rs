//! ESP-IDF TLS adapter for the host-testable Telegram HTTP client.

use super::{HttpClient, HttpTransport, HOST, READ_TIMEOUT};
use anyhow::Context;
use esp_idf_svc::tls::{Config as TlsConfig, EspTls, InternalSocket, KeepAliveConfig, X509};
use std::time::Duration;

const PORT: u16 = 443;

pub type TelegramHttpClient = HttpClient<EspTransport>;

pub struct EspTransport {
    tls: Option<EspTls<InternalSocket>>,
    ca_bundle: Option<&'static [u8]>,
}

impl TelegramHttpClient {
    /// Create a new client with optional CA bundle for server verification.
    pub fn new(ca_bundle: Option<&'static [u8]>) -> anyhow::Result<Self> {
        Self::connect(EspTransport {
            tls: None,
            ca_bundle,
        })
    }
}

impl EspTransport {
    fn tls_mut(&mut self) -> anyhow::Result<&mut EspTls<InternalSocket>> {
        self.tls
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("TLS client not connected"))
    }
}

impl HttpTransport for EspTransport {
    fn reconnect(&mut self) -> anyhow::Result<()> {
        self.close();
        let conf = TlsConfig {
            ca_cert: self.ca_bundle.map(X509::pem_until_nul),
            timeout_ms: READ_TIMEOUT.as_millis() as u32,
            keep_alive_cfg: Some(KeepAliveConfig {
                enable: true,
                idle: Duration::from_secs(60),
                interval: Duration::from_secs(10),
                count: 5,
            }),
            ..Default::default()
        };
        let mut tls = EspTls::new().context("tls client allocation failed")?;
        log::debug!("[http] connecting TLS to {}:{}", HOST, PORT);
        tls.connect(HOST, PORT, &conf)
            .map_err(tls_error)
            .with_context(|| format!("tls connect {}:{} failed", HOST, PORT))?;
        self.tls = Some(tls);
        log::debug!("[http] TLS connected to {}:{}", HOST, PORT);
        Ok(())
    }

    fn close(&mut self) {
        self.tls.take();
    }

    fn write_all(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        self.tls_mut()?.write_all(bytes).map_err(tls_error)
    }

    fn read(&mut self, buf: &mut [u8]) -> anyhow::Result<usize> {
        self.tls_mut()?.read(buf).map_err(tls_error)
    }
}

fn tls_error(error: esp_idf_sys::EspError) -> anyhow::Error {
    // EspError's Display collapses unrecognized TLS/socket errors to "ERROR".
    anyhow::anyhow!("{} (ESP-TLS code {})", error, error.code())
}
