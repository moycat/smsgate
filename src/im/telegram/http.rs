//! HTTPS client for Telegram API over ESP-IDF TLS.

use super::{
    build_get_file_body,
    types::{ApiResult, TelegramFile},
};
use anyhow::Context;
use esp_idf_svc::tls::{Config as TlsConfig, EspTls, InternalSocket, KeepAliveConfig, X509};
use std::time::Duration;

const HOST: &str = "api.telegram.org";
const PORT: u16 = 443;
const READ_TIMEOUT: Duration = Duration::from_secs(55);
const FILE_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_HEADER_BYTES: usize = 2 * 1024;
const MAX_BODY_BYTES: usize = 24 * 1024;
const STREAM_BUF_BYTES: usize = 4096;

/// TLS-backed HTTPS client for api.telegram.org.
pub struct TelegramHttpClient {
    tls: Option<EspTls<InternalSocket>>,
    ca_bundle: Option<&'static [u8]>,
}

#[derive(Debug, thiserror::Error)]
pub enum TelegramHttpError {
    #[error("request was not sent: {0}")]
    NotSent(#[source] anyhow::Error),
    #[error("request outcome is unknown: {0}")]
    OutcomeUnknown(#[source] anyhow::Error),
}

impl TelegramHttpClient {
    /// Create a new client with optional CA bundle for server verification.
    pub fn new(ca_bundle: Option<&'static [u8]>) -> anyhow::Result<Self> {
        let conf = Self::tls_config(ca_bundle);
        log::debug!("[http] connecting TLS to {}:{}", HOST, PORT);
        let mut tls = EspTls::new().context("tls client allocation failed")?;
        tls.connect(HOST, PORT, &conf)
            .with_context(|| format!("tls connect {}:{} failed", HOST, PORT))?;
        log::debug!("[http] TLS connected to {}:{}", HOST, PORT);
        Ok(TelegramHttpClient {
            tls: Some(tls),
            ca_bundle,
        })
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
    /// Never replay a POST after an uncertain response: Telegram may already
    /// have accepted a side-effecting request such as sendMessage.
    pub fn post(&mut self, path: &str, json_body: &str) -> Result<String, TelegramHttpError> {
        let method = bot_api_method(path);
        if self.tls.is_none() {
            self.reconnect().map_err(TelegramHttpError::NotSent)?;
        }
        match self.do_post(path, json_body) {
            Ok(body) => Ok(body),
            Err(e) => {
                log::warn!("[http] {} request failed: {:#}", method, e);
                self.tls.take();
                Err(TelegramHttpError::OutcomeUnknown(e))
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
        self.tls_mut()?
            .write_all(request.as_bytes())
            .context("file download request write failed")?;

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
        while headers.content_length.is_none_or(|cl| received < cl) {
            reset_current_task_watchdog();
            if std::time::Instant::now() > deadline {
                anyhow::bail!("file download idle timeout");
            }
            let n = self
                .tls_mut()?
                .read(&mut buf)
                .context("file download body read failed")?;
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
        let method = bot_api_method(path);
        let body_bytes = json_body.as_bytes();
        log::debug!(
            "[http] POST request: path_len={} body_len={}",
            path.len(),
            body_bytes.len()
        );
        let request_head = format!(
            "POST {} HTTP/1.1\r\n\
             Host: {}\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: keep-alive\r\n\
             \r\n",
            path,
            HOST,
            body_bytes.len()
        );

        let tls = self.tls_mut()?;
        tls.write_all(request_head.as_bytes())
            .with_context(|| format!("{} header write failed", method))?;
        tls.write_all(body_bytes)
            .with_context(|| format!("{} body write failed", method))?;

        let headers = self.read_binary_headers()?;
        let close_after_response = headers.connection_close;
        let deadline = std::time::Instant::now() + READ_TIMEOUT;
        let body = if headers.chunked {
            self.read_chunked_body(headers.remainder, deadline)?
        } else {
            let length = headers.content_length.ok_or_else(|| {
                anyhow::anyhow!("HTTP response lacks Content-Length or chunked framing")
            })?;
            if length > MAX_BODY_BYTES {
                anyhow::bail!(
                    "HTTP body length {} exceeds {} bytes",
                    length,
                    MAX_BODY_BYTES
                );
            }
            let mut body = headers.remainder;
            while body.len() < length {
                self.read_more(&mut body, deadline, length)?;
            }
            body.truncate(length);
            body
        };
        let body = String::from_utf8(body).context("HTTP response body is not UTF-8")?;
        if !(200..300).contains(&headers.status) && !body.trim_start().starts_with('{') {
            anyhow::bail!("HTTP {} from Telegram", headers.status);
        }
        if close_after_response {
            self.tls.take();
        }
        Ok(body)
    }

    fn read_more(
        &mut self,
        bytes: &mut Vec<u8>,
        deadline: std::time::Instant,
        limit: usize,
    ) -> anyhow::Result<()> {
        reset_current_task_watchdog();
        if std::time::Instant::now() > deadline {
            anyhow::bail!("HTTP response read timeout");
        }
        let mut buf = [0u8; 1024];
        let n = self
            .tls_mut()?
            .read(&mut buf)
            .context("HTTP body read failed")?;
        if n == 0 {
            anyhow::bail!("HTTP response closed before body completed");
        }
        if bytes.len().saturating_add(n) > limit.saturating_add(1024) {
            anyhow::bail!("HTTP response exceeded {} bytes", limit);
        }
        bytes.extend_from_slice(&buf[..n]);
        Ok(())
    }

    fn read_chunked_body(
        &mut self,
        mut raw: Vec<u8>,
        deadline: std::time::Instant,
    ) -> anyhow::Result<Vec<u8>> {
        let mut body = Vec::new();
        loop {
            let line_end = loop {
                if let Some(pos) = raw.windows(2).position(|pair| pair == b"\r\n") {
                    break pos;
                }
                if raw.len() > 128 {
                    anyhow::bail!("HTTP chunk header too long");
                }
                self.read_more(&mut raw, deadline, MAX_BODY_BYTES)?;
            };
            let size_line = std::str::from_utf8(&raw[..line_end])?;
            let size = usize::from_str_radix(size_line.split(';').next().unwrap_or(""), 16)
                .context("invalid HTTP chunk size")?;
            raw.drain(..line_end + 2);
            if size == 0 {
                // Consume optional trailer fields before reusing the socket.
                loop {
                    if raw.starts_with(b"\r\n") || find_header_end(&raw).is_some() {
                        return Ok(body);
                    }
                    if raw.len() > MAX_HEADER_BYTES {
                        anyhow::bail!("HTTP chunk trailers exceeded {} bytes", MAX_HEADER_BYTES);
                    }
                    self.read_more(&mut raw, deadline, MAX_HEADER_BYTES)?;
                }
            }
            if size > MAX_BODY_BYTES - body.len() {
                anyhow::bail!("HTTP chunked body exceeds {} bytes", MAX_BODY_BYTES);
            }
            while raw.len() < size + 2 {
                self.read_more(&mut raw, deadline, MAX_BODY_BYTES)?;
            }
            if &raw[size..size + 2] != b"\r\n" {
                anyhow::bail!("HTTP chunk terminator missing");
            }
            body.extend_from_slice(&raw[..size]);
            raw.drain(..size + 2);
        }
    }

    fn read_binary_headers(&mut self) -> anyhow::Result<ResponseHeaders> {
        let mut raw = Vec::with_capacity(MAX_HEADER_BYTES);
        let mut buf = [0u8; 512];
        let deadline = std::time::Instant::now() + READ_TIMEOUT;

        loop {
            reset_current_task_watchdog();
            if std::time::Instant::now() > deadline {
                anyhow::bail!("header read timeout");
            }
            let n = self
                .tls_mut()?
                .read(&mut buf)
                .context("HTTP response header read failed")?;
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
        drop(self.tls.take());
        let conf = Self::tls_config(self.ca_bundle);
        let mut tls = EspTls::new().context("tls client allocation failed")?;
        log::debug!("[http] reconnecting TLS to {}:{}", HOST, PORT);
        tls.connect(HOST, PORT, &conf)
            .with_context(|| format!("tls reconnect {}:{} failed", HOST, PORT))?;
        self.tls = Some(tls);
        log::debug!("[http] TLS reconnect complete");
        Ok(())
    }

    fn tls_mut(&mut self) -> anyhow::Result<&mut EspTls<InternalSocket>> {
        self.tls
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("TLS client not connected"))
    }
}

fn reset_current_task_watchdog() {
    // SAFETY: ESP-IDF only touches the current task's watchdog subscription.
    unsafe {
        let _ = esp_idf_sys::esp_task_wdt_reset();
    }
}

fn bot_api_method(path: &str) -> &str {
    path.rsplit('/')
        .next()
        .filter(|method| !method.is_empty())
        .unwrap_or("telegram")
}

struct ResponseHeaders {
    status: u16,
    content_length: Option<usize>,
    chunked: bool,
    connection_close: bool,
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
    let connection_close = header.lines().any(|line| {
        line.get(..11)
            .is_some_and(|p| p.eq_ignore_ascii_case("connection:"))
            && line.split_once(':').is_some_and(|(_, value)| {
                value
                    .split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case("close"))
            })
    });
    Ok(ResponseHeaders {
        status,
        content_length,
        chunked,
        connection_close,
        remainder,
    })
}
