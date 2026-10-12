//! Bounded HTTP(S) firmware downloads with explicit redirect handling.

use fluent_uri::{Uri, UriRef};
use thiserror::Error;

pub const MAX_URL_BYTES: usize = 4096;
pub const MAX_REDIRECTS: usize = 5;
const STREAM_BUFFER_BYTES: usize = 4096;
const TRANSFER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DownloadError {
    #[error("invalid firmware URL: {0}")]
    InvalidUrl(&'static str),
    #[error("HTTP {0} from firmware server")]
    Status(u16),
    #[error("redirect response has no Location header")]
    MissingLocation,
    #[error("firmware download exceeded {MAX_REDIRECTS} redirects")]
    TooManyRedirects,
    #[error("unsupported firmware Content-Encoding")]
    ContentEncoding,
    #[error("invalid firmware Content-Length")]
    ContentLength,
    #[error("firmware image is empty")]
    EmptyImage,
    #[error("file size {size} exceeds OTA slot size {slot_size}")]
    ImageTooLarge { size: usize, slot_size: usize },
    #[error("incomplete firmware download: got {received} of {expected} bytes")]
    Incomplete { received: usize, expected: usize },
    #[error("firmware download exceeds declared Content-Length")]
    ExcessBody,
    #[error("download is not an ESP app image")]
    InvalidImage,
    #[error("HTTP: {0}")]
    Http(String),
    #[error("firmware download timed out")]
    Timeout,
    #[error("flash write: {0}")]
    Write(String),
}

/// A validated absolute HTTP(S) URI without a fragment or user credentials.
#[derive(Clone, PartialEq, Eq)]
pub struct DownloadUrl(Uri<String>);

impl std::fmt::Debug for DownloadUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Signed download queries can contain credentials.
        f.write_str("DownloadUrl(<redacted>)")
    }
}

impl DownloadUrl {
    pub fn parse(input: &str) -> Result<Self, DownloadError> {
        validate_uri_text(input)?;
        let uri = Uri::parse(input).map_err(|_| DownloadError::InvalidUrl("malformed URI"))?;
        if !uri.scheme().as_str().eq_ignore_ascii_case("http")
            && !uri.scheme().as_str().eq_ignore_ascii_case("https")
        {
            return Err(DownloadError::InvalidUrl("use http:// or https://"));
        }
        let authority = uri
            .authority()
            .ok_or(DownloadError::InvalidUrl("missing host"))?;
        if authority.host().is_empty() || authority.host().contains('%') {
            return Err(DownloadError::InvalidUrl("invalid host"));
        }
        if authority.userinfo().is_some() {
            return Err(DownloadError::InvalidUrl(
                "user credentials are not supported",
            ));
        }
        if authority.port().is_some_and(|port| port.is_empty())
            || authority.port_to_u16().is_err()
            || authority.port_to_u16() == Ok(Some(0))
        {
            return Err(DownloadError::InvalidUrl("invalid port"));
        }
        Ok(Self(uri.strip_fragment().to_owned()))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Resolve RFC 3986 Location references without normalizing signed queries.
    pub fn resolve(&self, location: &str) -> Result<Self, DownloadError> {
        validate_uri_text(location)?;
        let reference = UriRef::parse(location)
            .map_err(|_| DownloadError::InvalidUrl("malformed redirect URI"))?;
        if reference.scheme().is_some() {
            return Self::parse(location);
        }
        let resolved = reference
            .resolve_against(&self.0)
            .map_err(|_| DownloadError::InvalidUrl("invalid redirect reference"))?;
        Self::parse(resolved.as_str())
    }
}

fn validate_uri_text(input: &str) -> Result<(), DownloadError> {
    if input.is_empty() || input.len() > MAX_URL_BYTES {
        return Err(DownloadError::InvalidUrl("empty or too long"));
    }
    if !input.is_ascii()
        || input
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        return Err(DownloadError::InvalidUrl(
            "use ASCII without whitespace or control characters",
        ));
    }
    Ok(())
}

/// Response operations shared by the ESP-IDF client and scripted host tests.
pub trait DownloadResponse {
    fn status(&self) -> u16;
    fn location(&self) -> Option<&str>;
    fn content_length(&self) -> Result<Option<usize>, DownloadError>;
    fn content_encoding(&self) -> Option<&str> {
        None
    }
    /// Read decoded response bytes; chunked transfer framing is handled below this trait.
    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, DownloadError>;
}

pub trait DownloadClient {
    type Response: DownloadResponse;

    /// Open one GET without following redirects or reusing an earlier response.
    fn get(&mut self, url: &DownloadUrl) -> Result<Self::Response, DownloadError>;
}

/// Return the final successful response after at most five redirects.
pub fn open_download<C: DownloadClient>(
    client: &mut C,
    url: &DownloadUrl,
) -> Result<C::Response, DownloadError> {
    let mut url = url.clone();
    for redirects in 0..=MAX_REDIRECTS {
        let response = client.get(&url)?;
        match response.status() {
            200 => {
                validate_encoding(&response)?;
                return Ok(response);
            }
            301 | 302 | 303 | 307 | 308 => {
                if redirects == MAX_REDIRECTS {
                    return Err(DownloadError::TooManyRedirects);
                }
                url = url.resolve(response.location().ok_or(DownloadError::MissingLocation)?)?;
                // Drop the old connection before opening a different origin.
            }
            status => return Err(DownloadError::Status(status)),
        }
    }
    Err(DownloadError::TooManyRedirects)
}

fn validate_encoding<R: DownloadResponse>(response: &R) -> Result<(), DownloadError> {
    if response
        .content_encoding()
        .is_some_and(|encoding| !encoding.trim().eq_ignore_ascii_case("identity"))
    {
        return Err(DownloadError::ContentEncoding);
    }
    Ok(())
}

/// Validate known length before initiating an OTA slot erase.
pub fn expected_size<R: DownloadResponse>(
    response: &R,
    slot_size: usize,
) -> Result<Option<usize>, DownloadError> {
    validate_encoding(response)?;
    let size = response.content_length()?;
    match size {
        Some(0) => Err(DownloadError::EmptyImage),
        Some(size) if size > slot_size => Err(DownloadError::ImageTooLarge { size, slot_size }),
        _ => Ok(size),
    }
}

/// Stream a nonempty ESP image without allocating a firmware-sized buffer.
/// ESP-IDF's OTA completion still performs the full firmware validation.
pub fn stream_download<R, F>(
    response: &mut R,
    slot_size: usize,
    mut on_chunk: F,
) -> Result<usize, DownloadError>
where
    R: DownloadResponse,
    F: FnMut(&[u8]) -> Result<(), DownloadError>,
{
    let expected = expected_size(response, slot_size)?;
    let mut received = 0usize;
    let mut buffer = [0u8; STREAM_BUFFER_BYTES];
    let started = std::time::Instant::now();
    loop {
        if started.elapsed() >= TRANSFER_TIMEOUT {
            return Err(DownloadError::Timeout);
        }
        reset_current_task_watchdog();
        let count = response.read(&mut buffer)?;
        if started.elapsed() >= TRANSFER_TIMEOUT {
            return Err(DownloadError::Timeout);
        }
        if count == 0 {
            break;
        }
        if count > buffer.len() {
            return Err(DownloadError::Http("invalid response read length".into()));
        }
        let next = received
            .checked_add(count)
            .ok_or(DownloadError::ImageTooLarge {
                size: usize::MAX,
                slot_size,
            })?;
        if next > slot_size {
            return Err(DownloadError::ImageTooLarge {
                size: next,
                slot_size,
            });
        }
        if expected.is_some_and(|size| next > size) {
            return Err(DownloadError::ExcessBody);
        }
        if received == 0 && buffer[0] != 0xe9 {
            return Err(DownloadError::InvalidImage);
        }
        on_chunk(&buffer[..count])?;
        received = next;
    }
    if received == 0 {
        return Err(DownloadError::EmptyImage);
    }
    if let Some(expected) = expected {
        if received != expected {
            return Err(DownloadError::Incomplete { received, expected });
        }
    }
    Ok(received)
}

fn reset_current_task_watchdog() {
    #[cfg(feature = "esp32")]
    // SAFETY: ESP-IDF only touches the current thread's watchdog subscription.
    unsafe {
        let _ = esp_idf_sys::esp_task_wdt_reset();
    }
}

#[cfg(feature = "esp32")]
pub use esp::EspDownloadClient;

#[cfg(feature = "esp32")]
mod esp {
    use super::*;
    use esp_idf_svc::http::client::{
        Configuration, EspHttpConnection, FollowRedirectsPolicy, Method,
    };
    use std::time::Duration;

    #[derive(Default)]
    pub struct EspDownloadClient;

    impl EspDownloadClient {
        pub fn new() -> Self {
            Self
        }
    }

    impl DownloadClient for EspDownloadClient {
        type Response = EspHttpConnection;

        fn get(&mut self, url: &DownloadUrl) -> Result<Self::Response, DownloadError> {
            let mut connection = EspHttpConnection::new(&Configuration {
                timeout: Some(Duration::from_secs(20)),
                buffer_size: Some(4096),
                follow_redirects_policy: FollowRedirectsPolicy::FollowNone,
                crt_bundle_attach: Some(esp_idf_sys::esp_crt_bundle_attach),
                ..Default::default()
            })
            .map_err(http_error)?;
            reset_current_task_watchdog();
            connection
                .initiate_request(
                    Method::Get,
                    url.as_str(),
                    &[
                        ("Accept", "application/octet-stream"),
                        ("Accept-Encoding", "identity"),
                        ("Connection", "close"),
                    ],
                )
                .map_err(http_error)?;
            reset_current_task_watchdog();
            connection.initiate_response().map_err(http_error)?;
            Ok(connection)
        }
    }

    impl DownloadResponse for EspHttpConnection {
        fn status(&self) -> u16 {
            EspHttpConnection::status(self)
        }

        fn location(&self) -> Option<&str> {
            self.header("Location")
        }

        fn content_length(&self) -> Result<Option<usize>, DownloadError> {
            self.header("Content-Length")
                .map(|value| {
                    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                        return Err(DownloadError::ContentLength);
                    }
                    value.parse().map_err(|_| DownloadError::ContentLength)
                })
                .transpose()
        }

        fn content_encoding(&self) -> Option<&str> {
            self.header("Content-Encoding")
        }

        fn read(&mut self, buffer: &mut [u8]) -> Result<usize, DownloadError> {
            EspHttpConnection::read(self, buffer).map_err(http_error)
        }
    }

    fn http_error(error: esp_idf_sys::EspError) -> DownloadError {
        DownloadError::Http(format!("{} (ESP-IDF code {})", error, error.code()))
    }
}
