//! HTTPS client for Telegram API over ESP-IDF TLS.

use super::{
    build_get_file_body,
    types::{ApiResult, TelegramFile},
};
use esp_idf_svc::tls::{Config as TlsConfig, EspTls, InternalSocket, KeepAliveConfig, X509};
use std::time::Duration;

const HOST: &str = "api.telegram.org";
const PORT: u16 = 443;
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const FILE_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_HEADER_BYTES: usize = 2 * 1024;
const MAX_BODY_BYTES: usize = 8 * 1024;
const STREAM_BUF_BYTES: usize = 4096;

/// TLS-backed HTTPS client for api.telegram.org.
pub struct TelegramHttpClient {
    tls: EspTls<InternalSocket>,
    ca_bundle: Option<&'static [u8]>,
}

impl TelegramHttpClient {
    /// Create a new client with optional CA bundle for server verification.
    pub fn new(ca_bundle: Option<&'static [u8]>) -> anyhow::Result<Self> {
        let conf = Self::tls_config(ca_bundle);
        log::debug!("[http] connecting TLS to {}:{}", HOST, PORT);
        let mut tls = EspTls::new()?;
        tls.connect(HOST, PORT, &conf)?;
        log::debug!("[http] TLS connected to {}:{}", HOST, PORT);
        Ok(TelegramHttpClient { tls, ca_bundle })
    }

    fn tls_config(ca_bundle: Option<&'static [u8]>) -> TlsConfig<'static> {
        TlsConfig {
            ca_cert: ca_bundle.map(X509::pem_until_nul),
            timeout_ms: READ_TIMEOUT.as_millis() as u32,
            keep_alive_cfg: Some(KeepAliveConfig {
                enable: true,
                idle: Duration::from_secs(60),
                interval: Duration::from_secs(10),
                count: 5,
            }),
            ..Default::default()
        }
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

    /// Resolve a Telegram file id into a temporary file path.
    pub fn get_file(&mut self, token: &str, file_id: &str) -> anyhow::Result<TelegramFile> {
        log::info!(
            "[http] Telegram getFile start: file_id_len={}",
            file_id.len()
        );
        let path = format!("/bot{}/getFile", token);
        let body = build_get_file_body(file_id);
        let raw = self.post(&path, &body)?;
        let result: ApiResult<TelegramFile> = serde_json::from_str(&raw)?;
        if result.ok {
            let file = result
                .result
                .ok_or_else(|| anyhow::anyhow!("getFile result missing"))?;
            log::info!(
                "[http] Telegram getFile ok: path_len={} size={:?}",
                file.file_path.as_deref().map(str::len).unwrap_or(0),
                file.file_size
            );
            Ok(file)
        } else {
            anyhow::bail!(
                "getFile API error: {}",
                result.description.unwrap_or_default()
            );
        }
    }

    /// Stream a Telegram file download into `on_chunk` without buffering it in RAM.
    pub fn download_file<F>(
        &mut self,
        token: &str,
        file_path: &str,
        mut on_chunk: F,
    ) -> anyhow::Result<usize>
    where
        F: FnMut(&[u8]) -> anyhow::Result<()>,
    {
        log::info!(
            "[http] Telegram file download start: path_len={}",
            file_path.len()
        );
        let path = format!("/file/bot{}/{}", token, file_path);
        let request = format!(
            "GET {} HTTP/1.1\r\n\
             Host: {}\r\n\
             Accept: application/octet-stream\r\n\
             Connection: close\r\n\
             \r\n",
            path, HOST
        );
        self.tls.write_all(request.as_bytes())?;

        let headers = self.read_binary_headers()?;
        if !(200..300).contains(&headers.status) {
            anyhow::bail!("HTTP {}", headers.status);
        }
        if headers.chunked {
            anyhow::bail!("chunked file downloads are not supported");
        }
        log::info!(
            "[http] Telegram file response: status={} content_length={:?} remainder={}",
            headers.status,
            headers.content_length,
            headers.remainder.len()
        );

        let mut received = 0usize;
        if !headers.remainder.is_empty() {
            let len = headers
                .content_length
                .map(|cl| cl.saturating_sub(received).min(headers.remainder.len()))
                .unwrap_or(headers.remainder.len());
            if len > 0 {
                on_chunk(&headers.remainder[..len])?;
                received += len;
            }
        }

        let mut buf = [0u8; STREAM_BUF_BYTES];
        let mut deadline = std::time::Instant::now() + FILE_IDLE_TIMEOUT;
        while headers.content_length.map_or(true, |cl| received < cl) {
            if std::time::Instant::now() > deadline {
                anyhow::bail!("file download idle timeout");
            }
            let n = self.tls.read(&mut buf)?;
            if n == 0 {
                break;
            }
            deadline = std::time::Instant::now() + FILE_IDLE_TIMEOUT;
            let len = headers
                .content_length
                .map(|cl| cl.saturating_sub(received).min(n))
                .unwrap_or(n);
            if len > 0 {
                on_chunk(&buf[..len])?;
                received += len;
            }
        }

        if let Some(cl) = headers.content_length {
            if received < cl {
                anyhow::bail!("incomplete file download: got {} of {} bytes", received, cl);
            }
        }
        log::info!("[http] Telegram file download complete: {} bytes", received);
        Ok(received)
    }

    fn do_post(&mut self, path: &str, json_body: &str) -> anyhow::Result<String> {
        let body_bytes = json_body.as_bytes();
        log::debug!(
            "[http] POST request: path_len={} body_len={}",
            path.len(),
            body_bytes.len()
        );
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
            if response.len() > MAX_HEADER_BYTES && !response.contains("\r\n\r\n") {
                anyhow::bail!("HTTP headers exceeded {} bytes", MAX_HEADER_BYTES);
            }
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
        if cl > MAX_BODY_BYTES {
            anyhow::bail!("HTTP body length {} exceeds {} bytes", cl, MAX_BODY_BYTES);
        }

        // Find body start
        let body_start = response
            .find("\r\n\r\n")
            .map(|i| i + 4)
            .unwrap_or(response.len());
        let mut body = response[body_start..].to_string();
        if body.len() > MAX_BODY_BYTES {
            anyhow::bail!("HTTP body exceeded {} bytes", MAX_BODY_BYTES);
        }

        // Read remaining body bytes
        while body.len() < cl {
            if std::time::Instant::now() > deadline {
                anyhow::bail!("body read timeout");
            }
            let n = self.tls.read(&mut buf)?;
            if n == 0 {
                break;
            }
            if body.len() + n > MAX_BODY_BYTES {
                anyhow::bail!("HTTP body exceeded {} bytes", MAX_BODY_BYTES);
            }
            body.push_str(&String::from_utf8_lossy(&buf[..n]));
        }

        Ok(body)
    }

    fn read_binary_headers(&mut self) -> anyhow::Result<ResponseHeaders> {
        let mut raw = Vec::with_capacity(MAX_HEADER_BYTES);
        let mut buf = [0u8; 512];
        let deadline = std::time::Instant::now() + READ_TIMEOUT;

        loop {
            if std::time::Instant::now() > deadline {
                anyhow::bail!("header read timeout");
            }
            let n = self.tls.read(&mut buf)?;
            if n == 0 {
                anyhow::bail!("connection closed before headers received");
            }
            raw.extend_from_slice(&buf[..n]);
            if raw.len() > MAX_HEADER_BYTES && find_header_end(&raw).is_none() {
                anyhow::bail!("HTTP headers exceeded {} bytes", MAX_HEADER_BYTES);
            }
            if let Some(pos) = find_header_end(&raw) {
                let header = String::from_utf8_lossy(&raw[..pos]).to_string();
                let remainder = raw[pos + 4..].to_vec();
                return parse_headers(&header, remainder);
            }
        }
    }

    fn reconnect(&mut self) -> anyhow::Result<()> {
        let conf = Self::tls_config(self.ca_bundle);
        let mut tls = EspTls::new()?;
        log::debug!("[http] reconnecting TLS to {}:{}", HOST, PORT);
        tls.connect(HOST, PORT, &conf)?;
        self.tls = tls;
        log::debug!("[http] TLS reconnect complete");
        Ok(())
    }
}

struct ResponseHeaders {
    status: u16,
    content_length: Option<usize>,
    chunked: bool,
    remainder: Vec<u8>,
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn parse_headers(header: &str, remainder: Vec<u8>) -> anyhow::Result<ResponseHeaders> {
    let status = header
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("cannot parse HTTP status"))?;
    let content_length = header
        .lines()
        .find(|line| {
            line.get(..15)
                .is_some_and(|p| p.eq_ignore_ascii_case("content-length:"))
        })
        .and_then(|line| line.split_once(':').map(|(_, value)| value))
        .and_then(|value| value.trim().parse().ok());
    let chunked = header.lines().any(|line| {
        line.get(..18)
            .is_some_and(|p| p.eq_ignore_ascii_case("transfer-encoding:"))
            && line.to_ascii_lowercase().contains("chunked")
    });
    Ok(ResponseHeaders {
        status,
        content_length,
        chunked,
        remainder,
    })
}
