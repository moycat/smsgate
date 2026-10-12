use smsgate::ota::download::{
    expected_size, open_download, stream_download, DownloadClient, DownloadError, DownloadResponse,
    DownloadUrl, MAX_REDIRECTS, MAX_URL_BYTES,
};
use std::collections::VecDeque;

struct Response {
    status: u16,
    location: Option<String>,
    size: Result<Option<usize>, DownloadError>,
    encoding: Option<String>,
    reads: VecDeque<Result<Vec<u8>, DownloadError>>,
}

impl Response {
    fn image(size: Option<usize>, chunks: &[&[u8]]) -> Self {
        Self {
            status: 200,
            location: None,
            size: Ok(size),
            encoding: None,
            reads: chunks.iter().map(|chunk| Ok(chunk.to_vec())).collect(),
        }
    }

    fn redirect(status: u16, location: &str) -> Self {
        let mut response = Self::image(None, &[]);
        response.status = status;
        response.location = Some(location.into());
        response
    }
}

impl DownloadResponse for Response {
    fn status(&self) -> u16 {
        self.status
    }

    fn location(&self) -> Option<&str> {
        self.location.as_deref()
    }

    fn content_length(&self) -> Result<Option<usize>, DownloadError> {
        match &self.size {
            Ok(size) => Ok(*size),
            Err(_) => Err(DownloadError::ContentLength),
        }
    }

    fn content_encoding(&self) -> Option<&str> {
        self.encoding.as_deref()
    }

    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, DownloadError> {
        let bytes = self.reads.pop_front().unwrap_or_else(|| Ok(Vec::new()))?;
        assert!(bytes.len() <= buffer.len());
        buffer[..bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len())
    }
}

struct Client {
    steps: VecDeque<(&'static str, Response)>,
}

impl DownloadClient for Client {
    type Response = Response;

    fn get(&mut self, url: &DownloadUrl) -> Result<Self::Response, DownloadError> {
        let (expected, response) = self.steps.pop_front().expect("unexpected request");
        assert_eq!(url.as_str(), expected);
        Ok(response)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            assert!(self.steps.is_empty(), "unconsumed download steps");
        }
    }
}

fn url() -> DownloadUrl {
    DownloadUrl::parse("https://firmware.example/path/app.bin").unwrap()
}

fn download(response: &mut Response, slot_size: usize) -> Result<Vec<u8>, DownloadError> {
    let mut bytes = Vec::new();
    let received = stream_download(response, slot_size, |chunk| {
        bytes.extend_from_slice(chunk);
        Ok(())
    })?;
    assert_eq!(received, bytes.len());
    Ok(bytes)
}

#[test]
fn urls_accept_http_https_ports_and_ipv6_and_remove_fragment() {
    for valid in [
        "http://firmware.example/app.bin",
        "HTTPS://firmware.example:443/app.bin",
        "https://[2001:db8::1]:8443/app.bin",
    ] {
        assert_eq!(DownloadUrl::parse(valid).unwrap().as_str(), valid);
    }
    let signed = "https://firmware.example/app.bin?sig=a%2Fb+c&name=%7e";
    assert_eq!(
        DownloadUrl::parse(&format!("{signed}#part"))
            .unwrap()
            .as_str(),
        signed
    );
}

#[test]
fn urls_reject_unsupported_or_ambiguous_inputs() {
    for invalid in [
        "",
        "ftp://firmware.example/app.bin",
        "//firmware.example/app.bin",
        "https:app.bin",
        "https:///app.bin",
        "https://user:password@firmware.example/app.bin",
        "https://@firmware.example/app.bin",
        "https://firmware.example:0/app.bin",
        "https://firmware.example:65536/app.bin",
        "https://firmware.example:/app.bin",
        "https://firmware.example/app.bin more",
        "https://firmware.example/app.bin\r\nX:foo",
        "https://firmware.example/你好.bin",
        "https://firmware.example/%zz",
        "https://firmware%2eexample/app.bin",
        "https://firmware.example/a\\b",
    ] {
        assert!(DownloadUrl::parse(invalid).is_err(), "accepted {invalid:?}");
    }
    assert!(DownloadUrl::parse(&format!(
        "https://example.com/{}",
        "a".repeat(MAX_URL_BYTES)
    ))
    .is_err());
}

#[test]
fn redirect_references_resolve_paths_hosts_queries_and_fragments() {
    let base = DownloadUrl::parse("https://example.com/a/b/app.bin?token=a%2fb+c").unwrap();
    for (reference, expected) in [
        ("../new.bin", "https://example.com/a/new.bin"),
        ("./new.bin", "https://example.com/a/b/new.bin"),
        ("/new.bin", "https://example.com/new.bin"),
        ("//cdn.example/app.bin", "https://cdn.example/app.bin"),
        (
            "?token=x%2Fy+z&x=%7e",
            "https://example.com/a/b/app.bin?token=x%2Fy+z&x=%7e",
        ),
        ("#download", "https://example.com/a/b/app.bin?token=a%2fb+c"),
        (
            "https://cdn.example/app.bin?sig=a%2fb+c",
            "https://cdn.example/app.bin?sig=a%2fb+c",
        ),
        ("http://cdn.example/app.bin", "http://cdn.example/app.bin"),
    ] {
        assert_eq!(base.resolve(reference).unwrap().as_str(), expected);
    }
    for invalid in [
        "ftp://example.com/app.bin",
        "//user@cdn.example/app.bin",
        "",
        "\n/new.bin",
    ] {
        assert!(base.resolve(invalid).is_err());
    }
}

#[test]
fn debug_output_does_not_expose_signed_urls() {
    let url = DownloadUrl::parse("https://example.com/app.bin?secret=hidden").unwrap();
    let debug = format!("{url:?}");
    assert!(!debug.contains("hidden"));
    assert!(!debug.contains("example.com"));
}

#[test]
fn open_download_follows_every_supported_redirect_and_returns_only_final_body() {
    let mut client = Client {
        steps: [
            (
                "https://firmware.example/path/app.bin",
                Response::redirect(301, "next.bin"),
            ),
            (
                "https://firmware.example/path/next.bin",
                Response::redirect(302, "/new.bin"),
            ),
            (
                "https://firmware.example/new.bin",
                Response::redirect(303, "//cdn.example/app.bin"),
            ),
            (
                "https://cdn.example/app.bin",
                Response::redirect(307, "?sig=x%2fy+z"),
            ),
            (
                "https://cdn.example/app.bin?sig=x%2fy+z",
                Response::redirect(308, "https://storage.example/final.bin"),
            ),
            (
                "https://storage.example/final.bin",
                Response::image(Some(4), &[&[0xe9, 1, 2, 3]]),
            ),
        ]
        .into(),
    };
    let mut response = open_download(&mut client, &url()).unwrap();
    assert_eq!(download(&mut response, 128).unwrap(), [0xe9, 1, 2, 3]);
}

#[test]
fn redirect_loop_stops_after_five_hops() {
    let mut client = Client {
        steps: (0..=MAX_REDIRECTS)
            .map(|_| {
                (
                    "https://firmware.example/path/app.bin",
                    Response::redirect(302, "app.bin"),
                )
            })
            .collect(),
    };
    assert!(matches!(
        open_download(&mut client, &url()),
        Err(DownloadError::TooManyRedirects)
    ));
}

#[test]
fn redirect_requires_location_and_valid_scheme() {
    let mut missing = Response::image(None, &[]);
    missing.status = 302;
    let mut client = Client {
        steps: [("https://firmware.example/path/app.bin", missing)].into(),
    };
    assert!(matches!(
        open_download(&mut client, &url()),
        Err(DownloadError::MissingLocation)
    ));
    let mut client = Client {
        steps: [(
            "https://firmware.example/path/app.bin",
            Response::redirect(302, "file:///firmware.bin"),
        )]
        .into(),
    };
    assert!(matches!(
        open_download(&mut client, &url()),
        Err(DownloadError::InvalidUrl(_))
    ));
}

#[test]
fn open_download_rejects_partial_content_non_redirect_3xx_and_errors() {
    for status in [204, 206, 300, 304, 305, 400, 403, 404, 500] {
        let mut response = Response::image(None, &[]);
        response.status = status;
        let mut client = Client {
            steps: [("https://firmware.example/path/app.bin", response)].into(),
        };
        assert!(
            matches!(open_download(&mut client, &url()), Err(DownloadError::Status(actual)) if actual == status)
        );
    }
}

#[test]
fn compressed_response_is_rejected_and_identity_is_accepted() {
    for encoding in ["gzip", "br", "identity, gzip", ""] {
        let mut response = Response::image(None, &[]);
        response.encoding = Some(encoding.into());
        let mut client = Client {
            steps: [("https://firmware.example/path/app.bin", response)].into(),
        };
        assert!(matches!(
            open_download(&mut client, &url()),
            Err(DownloadError::ContentEncoding)
        ));
    }
    let mut response = Response::image(Some(1), &[&[0xe9]]);
    response.encoding = Some("Identity".into());
    assert_eq!(download(&mut response, 32).unwrap(), [0xe9]);
}

#[test]
fn validates_declared_size_before_body_read_or_write() {
    let mut response = Response::image(Some(129), &[&[0xe9]]);
    assert_eq!(
        expected_size(&response, 128),
        Err(DownloadError::ImageTooLarge {
            size: 129,
            slot_size: 128
        })
    );
    assert!(matches!(
        stream_download(&mut response, 128, |_| panic!("unexpected write")),
        Err(DownloadError::ImageTooLarge { .. })
    ));
    assert_eq!(response.reads.len(), 1);
    response.size = Err(DownloadError::ContentLength);
    assert_eq!(
        expected_size(&response, 128),
        Err(DownloadError::ContentLength)
    );
    response.size = Ok(Some(0));
    assert_eq!(
        expected_size(&response, 128),
        Err(DownloadError::EmptyImage)
    );
}

#[test]
fn known_and_unknown_length_downloads_stream_identical_decoded_data() {
    for size in [Some(5), None] {
        let mut response = Response::image(size, &[&[0xe9], &[1, 2], &[3, 4]]);
        assert_eq!(download(&mut response, 5).unwrap(), [0xe9, 1, 2, 3, 4]);
    }
}

#[test]
fn unknown_length_exceeding_slot_stops_before_oversized_chunk_write() {
    let mut response = Response::image(None, &[&[0xe9, 1], &[2, 3]]);
    let mut bytes = Vec::new();
    let error = stream_download(&mut response, 3, |chunk| {
        bytes.extend_from_slice(chunk);
        Ok(())
    })
    .unwrap_err();
    assert_eq!(
        error,
        DownloadError::ImageTooLarge {
            size: 4,
            slot_size: 3
        }
    );
    assert_eq!(bytes, [0xe9, 1]);
}

#[test]
fn declared_length_must_match_downloaded_length() {
    let mut truncated = Response::image(Some(5), &[&[0xe9, 1]]);
    assert_eq!(
        download(&mut truncated, 128),
        Err(DownloadError::Incomplete {
            received: 2,
            expected: 5
        })
    );
    let mut extra = Response::image(Some(2), &[&[0xe9, 1], &[2]]);
    assert_eq!(download(&mut extra, 128), Err(DownloadError::ExcessBody));
}

#[test]
fn empty_or_non_image_body_never_gets_written() {
    for body in [&[][..], &b"<html>404</html>"[..], &[0, 1, 2][..]] {
        let mut response = Response::image(None, &[body]);
        assert!(matches!(
            stream_download(&mut response, 128, |_| panic!("unexpected write")),
            Err(DownloadError::EmptyImage | DownloadError::InvalidImage)
        ));
    }
}

#[test]
fn read_and_write_errors_are_returned_without_reading_more_data() {
    let mut response = Response::image(None, &[&[0xe9], &[1]]);
    response
        .reads
        .push_front(Err(DownloadError::Http("disconnected".into())));
    assert_eq!(
        download(&mut response, 128),
        Err(DownloadError::Http("disconnected".into()))
    );
    assert_eq!(response.reads.len(), 2);

    let mut response = Response::image(None, &[&[0xe9], &[1]]);
    assert_eq!(
        stream_download(&mut response, 128, |_| Err(DownloadError::Write(
            "flash failed".into()
        ))),
        Err(DownloadError::Write("flash failed".into()))
    );
    assert_eq!(response.reads.len(), 1);
}
