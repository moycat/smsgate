//! HTTPS client for Telegram API over ESP-IDF TLS.

use esp_idf_svc::tls::{EspTls, InternalSocket, KeepAliveConfig, X509};
use std::time::Duration;

const HOST: &str = "api.telegram.org";
const PORT: u16 = 443;
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// TLS-backed HTTPS client for api.telegram.org.
pub struct TelegramHttpClient {
    tls: EspTls<InternalSocket>,
}

impl TelegramHttpClient {
    /// Create a new client with optional CA bundle for server verification.
    pub fn new(ca_bundle: Option<&'static [u8]>) -> anyhow::Result<Self> {
        let conf = esp_idf_svc::tls::Config {
            ca_cert: ca_bundle.map(|b| X509::pem_until_nul(b)),
            keep_alive_cfg: Some(KeepAliveConfig {
                enable: true,
                idle: std::time::Duration::from_secs(60),
                interval: std::time::Duration::from_secs(10),
                count: 5,
            }),
            ..Default::default()
        };
        let mut tls = EspTls::new()?;
        tls.connect(HOST, PORT, &conf)?;
        Ok(TelegramHttpClient { tls })
    }

    /// POST JSON to a Telegram Bot API path; returns the response body.
    ///
    /// On connection-level failure (server closed keep-alive, timeout, etc.),
    /// reconnects once and retries automatically.
    pub fn post(&mut self, path: &str, json_body: &str) -> anyhow::Result<String> {
        match self.do_post(path, json_body) {
            Ok(body) => Ok(body),
            Err(e) => {
                log::warn!("[http] request failed ({}), reconnecting…", e);
                self.reconnect()?;
                self.do_post(path, json_body)
            }
        }
    }

    fn do_post(&mut self, path: &str, json_body: &str) -> anyhow::Result<String> {
        let body_bytes = json_body.as_bytes();
        let request = format!(
            "POST {} HTTP/1.1\r\n\
             Host: {}\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: keep-alive\r\n\
             \r\n\
             {}",
            path,
            HOST,
            body_bytes.len(),
            json_body
        );

        self.tls.write_all(request.as_bytes())?;

        let mut response = String::with_capacity(4096);
        let mut buf = [0u8; 1024];
        let deadline = std::time::Instant::now() + READ_TIMEOUT;

        // Read until we have the full headers
        loop {
            if std::time::Instant::now() > deadline {
                anyhow::bail!("read timeout");
            }
            let n = self.tls.read(&mut buf)?;
            if n == 0 {
                // Server closed the connection — trigger reconnect
                anyhow::bail!("connection closed before headers received");
            }
            response.push_str(&String::from_utf8_lossy(&buf[..n]));
            if response.contains("\r\n\r\n") {
                break;
            }
        }

        // Parse Content-Length
        let cl: usize = response
            .lines()
            .find(|l| {
                l.get(..15)
                    .is_some_and(|p| p.eq_ignore_ascii_case("content-length:"))
            })
            .and_then(|l| l.splitn(2, ':').nth(1))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0);

        // Find body start
        let body_start = response
            .find("\r\n\r\n")
            .map(|i| i + 4)
            .unwrap_or(response.len());
        let mut body = response[body_start..].to_string();

        // Read remaining body bytes
        while body.len() < cl {
            if std::time::Instant::now() > deadline {
                anyhow::bail!("body read timeout");
            }
            let n = self.tls.read(&mut buf)?;
            if n == 0 {
                break;
            }
            body.push_str(&String::from_utf8_lossy(&buf[..n]));
        }

        Ok(body)
    }

    fn reconnect(&mut self) -> anyhow::Result<()> {
        let conf = esp_idf_svc::tls::Config {
            keep_alive_cfg: Some(KeepAliveConfig {
                enable: true,
                idle: std::time::Duration::from_secs(60),
                interval: std::time::Duration::from_secs(10),
                count: 5,
            }),
            ..Default::default()
        };
        let mut tls = EspTls::new()?;
        tls.connect(HOST, PORT, &conf)?;
        self.tls = tls;
        Ok(())
    }
}
