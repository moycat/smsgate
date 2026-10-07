//! Request-stage regression tests for safe recovery of stale Telegram connections.

use smsgate::im::telegram::http::{HttpClient, HttpTransport, TelegramHttpError};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

const PATH: &str = "/bot-test/sendMessage";
const BODY: &str = r#"{"chat_id":1,"text":"test notification"}"#;
const RESPONSE_BODY: &str = r#"{"ok":true,"result":{"message_id":17}}"#;
const FILE_BYTES: &[u8] = b"\xe9\x00\x01\x02test";

#[derive(Debug)]
enum Step {
    Connect(Result<(), &'static str>),
    Write {
        expected: Vec<u8>,
        accepted: usize,
        error: Option<&'static str>,
    },
    Read(Result<Vec<u8>, &'static str>),
}

#[derive(Debug, PartialEq, Eq)]
enum Event {
    Connect,
    Close,
    Write { bytes: Vec<u8>, accepted: usize },
    Read,
}

#[derive(Debug)]
struct State {
    steps: VecDeque<Step>,
    events: Vec<Event>,
}

struct ScriptedTransport {
    state: Arc<Mutex<State>>,
}

impl HttpTransport for ScriptedTransport {
    fn reconnect(&mut self) -> anyhow::Result<()> {
        let mut state = self.state.lock().unwrap();
        if state.events.contains(&Event::Connect) {
            assert_eq!(
                state.events.last(),
                Some(&Event::Close),
                "the previous connection must be closed before reconnecting"
            );
        }
        state.events.push(Event::Connect);
        match state.steps.pop_front().expect("unexpected reconnect") {
            Step::Connect(result) => result.map_err(anyhow::Error::msg),
            other => panic!("expected {other:?}, got reconnect"),
        }
    }

    fn close(&mut self) {
        self.state.lock().unwrap().events.push(Event::Close);
    }

    fn write_all(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        let mut state = self.state.lock().unwrap();
        match state.steps.pop_front().expect("unexpected request write") {
            Step::Write {
                expected,
                accepted,
                error,
            } => {
                assert_eq!(bytes, expected, "unexpected request bytes");
                assert!(accepted <= bytes.len());
                state.events.push(Event::Write {
                    bytes: bytes.to_vec(),
                    accepted,
                });
                match error {
                    Some(error) => Err(anyhow::anyhow!(error)),
                    None => {
                        assert_eq!(accepted, bytes.len());
                        Ok(())
                    }
                }
            }
            other => panic!("expected {other:?}, got request write"),
        }
    }

    fn read(&mut self, buffer: &mut [u8]) -> anyhow::Result<usize> {
        let mut state = self.state.lock().unwrap();
        state.events.push(Event::Read);
        match state.steps.pop_front().expect("unexpected response read") {
            Step::Read(Ok(bytes)) => {
                assert!(bytes.len() <= buffer.len());
                buffer[..bytes.len()].copy_from_slice(&bytes);
                Ok(bytes.len())
            }
            Step::Read(Err(error)) => Err(anyhow::anyhow!(error)),
            other => panic!("expected {other:?}, got response read"),
        }
    }
}

fn request_header(body: &str) -> Vec<u8> {
    request_header_for(PATH, body)
}

fn request_header_for(path: &str, body: &str) -> Vec<u8> {
    format!(
        "POST {path} HTTP/1.1\r\n\
         Host: api.telegram.org\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: keep-alive\r\n\
         \r\n",
        body.len()
    )
    .into_bytes()
}

fn write_ok(bytes: impl Into<Vec<u8>>) -> Step {
    let expected = bytes.into();
    let accepted = expected.len();
    Step::Write {
        expected,
        accepted,
        error: None,
    }
}

fn write_failed(bytes: impl Into<Vec<u8>>, accepted: usize) -> Step {
    Step::Write {
        expected: bytes.into(),
        accepted,
        error: Some("simulated TLS write failure"),
    }
}

fn response() -> Step {
    response_with_body(RESPONSE_BODY.as_bytes(), false)
}

fn response_with_body(body: &[u8], close: bool) -> Step {
    let close_header = if close { "Connection: close\r\n" } else { "" };
    let mut bytes = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n{close_header}\r\n",
        body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(body);
    Step::Read(Ok(bytes))
}

fn download_request() -> Vec<u8> {
    b"GET /file/bottest-token/documents/test.bin HTTP/1.1\r\n\
      Host: api.telegram.org\r\n\
      Accept: application/octet-stream\r\n\
      Connection: close\r\n\
      \r\n"
        .to_vec()
}

fn client(steps: Vec<Step>) -> (HttpClient<ScriptedTransport>, Arc<Mutex<State>>) {
    let state = Arc::new(Mutex::new(State {
        steps: steps.into(),
        events: Vec::new(),
    }));
    let transport = ScriptedTransport {
        state: Arc::clone(&state),
    };
    (HttpClient::connect(transport).unwrap(), state)
}

fn assert_finished(state: &Arc<Mutex<State>>, expected_connects: usize, body_writes: usize) {
    let state = state.lock().unwrap();
    assert!(
        state.steps.is_empty(),
        "unconsumed steps: {:?}",
        state.steps
    );
    assert_eq!(
        state
            .events
            .iter()
            .filter(|event| **event == Event::Connect)
            .count(),
        expected_connects
    );
    assert_eq!(
        state
            .events
            .iter()
            .filter(|event| matches!(event, Event::Write { bytes, .. } if bytes == BODY.as_bytes()))
            .count(),
        body_writes
    );
}

fn assert_failed_connection_closed(state: &Arc<Mutex<State>>) {
    assert_eq!(state.lock().unwrap().events.last(), Some(&Event::Close));
}

#[test]
fn stale_keep_alive_header_failure_reconnects_without_replaying_a_body() {
    let (mut client, state) = client(vec![
        Step::Connect(Ok(())),
        write_ok(request_header(BODY)),
        write_ok(BODY),
        response(),
        write_failed(request_header(BODY), 0),
        Step::Connect(Ok(())),
        write_ok(request_header(BODY)),
        write_ok(BODY),
        response(),
    ]);

    assert_eq!(client.post(PATH, BODY).unwrap(), RESPONSE_BODY);
    assert_eq!(client.post(PATH, BODY).unwrap(), RESPONSE_BODY);

    assert_finished(&state, 2, 2);
}

#[test]
fn partial_header_failure_reconnects_before_sending_the_only_body() {
    let (mut client, state) = client(vec![
        Step::Connect(Ok(())),
        write_failed(request_header(BODY), 37),
        Step::Connect(Ok(())),
        write_ok(request_header(BODY)),
        write_ok(BODY),
        response(),
    ]);

    assert_eq!(client.post(PATH, BODY).unwrap(), RESPONSE_BODY);

    assert_finished(&state, 2, 1);
}

#[test]
fn body_write_failure_has_unknown_outcome_and_is_not_replayed() {
    let (mut client, state) = client(vec![
        Step::Connect(Ok(())),
        write_ok(request_header(BODY)),
        write_failed(BODY, BODY.len() / 2),
    ]);

    let result = client.post(PATH, BODY);

    assert!(matches!(result, Err(TelegramHttpError::OutcomeUnknown(_))));
    assert_finished(&state, 1, 1);
    assert_failed_connection_closed(&state);
}

#[test]
fn response_read_failure_after_body_has_unknown_outcome_and_is_not_replayed() {
    let (mut client, state) = client(vec![
        Step::Connect(Ok(())),
        write_ok(request_header(BODY)),
        write_ok(BODY),
        Step::Read(Err("simulated response timeout")),
    ]);

    let result = client.post(PATH, BODY);

    assert!(matches!(result, Err(TelegramHttpError::OutcomeUnknown(_))));
    assert_finished(&state, 1, 1);
    assert_failed_connection_closed(&state);
}

#[test]
fn response_eof_after_body_has_unknown_outcome_and_is_not_replayed() {
    let (mut client, state) = client(vec![
        Step::Connect(Ok(())),
        write_ok(request_header(BODY)),
        write_ok(BODY),
        Step::Read(Ok(Vec::new())),
    ]);

    let result = client.post(PATH, BODY);

    assert!(matches!(result, Err(TelegramHttpError::OutcomeUnknown(_))));
    assert_finished(&state, 1, 1);
    assert_failed_connection_closed(&state);
}

#[test]
fn reconnect_failure_after_header_failure_remains_safe_to_retry_later() {
    let (mut client, state) = client(vec![
        Step::Connect(Ok(())),
        write_failed(request_header(BODY), 0),
        Step::Connect(Err("simulated connection failure")),
    ]);

    let result = client.post(PATH, BODY);

    assert!(matches!(result, Err(TelegramHttpError::NotSent(_))));
    assert_finished(&state, 2, 0);
}

#[test]
fn second_header_failure_stops_immediate_retries_without_sending_a_body() {
    let (mut client, state) = client(vec![
        Step::Connect(Ok(())),
        write_failed(request_header(BODY), 0),
        Step::Connect(Ok(())),
        write_failed(request_header(BODY), 37),
    ]);

    let result = client.post(PATH, BODY);

    assert!(matches!(result, Err(TelegramHttpError::NotSent(_))));
    assert_finished(&state, 2, 0);
    assert_failed_connection_closed(&state);
}

#[test]
fn empty_body_header_failure_has_unknown_outcome_and_is_not_replayed() {
    let header = request_header("");
    let accepted = header.len();
    let (mut client, state) = client(vec![Step::Connect(Ok(())), write_failed(header, accepted)]);

    let result = client.post(PATH, "");

    assert!(matches!(result, Err(TelegramHttpError::OutcomeUnknown(_))));
    assert_finished(&state, 1, 0);
    assert_failed_connection_closed(&state);
}

#[test]
fn connection_close_response_forces_reconnect_before_next_post() {
    let (mut client, state) = client(vec![
        Step::Connect(Ok(())),
        write_ok(request_header(BODY)),
        write_ok(BODY),
        response_with_body(RESPONSE_BODY.as_bytes(), true),
        Step::Connect(Ok(())),
        write_ok(request_header(BODY)),
        write_ok(BODY),
        response(),
    ]);

    assert_eq!(client.post(PATH, BODY).unwrap(), RESPONSE_BODY);
    assert_eq!(client.post(PATH, BODY).unwrap(), RESPONSE_BODY);

    assert_finished(&state, 2, 2);
}

#[test]
fn get_file_connection_close_reconnects_before_download() {
    let get_file_body = r#"{"file_id":"file-test"}"#;
    let file_response = br#"{"ok":true,"result":{"file_id":"file-test","file_unique_id":"unique-test","file_size":8,"file_path":"documents/test.bin"}}"#;
    let (mut client, state) = client(vec![
        Step::Connect(Ok(())),
        write_ok(request_header_for("/bottest-token/getFile", get_file_body)),
        write_ok(get_file_body),
        response_with_body(file_response, true),
        Step::Connect(Ok(())),
        write_ok(download_request()),
        response_with_body(FILE_BYTES, true),
    ]);

    let file = client.get_file("test-token", "file-test").unwrap();
    assert_eq!(file.file_size, Some(FILE_BYTES.len() as u64));
    let mut downloaded = Vec::new();
    let received = client
        .download_file("test-token", file.file_path.as_deref().unwrap(), |chunk| {
            downloaded.extend_from_slice(chunk);
            Ok(())
        })
        .unwrap();

    assert_eq!(received, FILE_BYTES.len());
    assert_eq!(downloaded, FILE_BYTES);
    assert_finished(&state, 2, 0);
}

#[test]
fn completed_download_forces_reconnect_before_next_post() {
    let (mut client, state) = client(vec![
        Step::Connect(Ok(())),
        write_ok(download_request()),
        response_with_body(FILE_BYTES, true),
        Step::Connect(Ok(())),
        write_ok(request_header(BODY)),
        write_ok(BODY),
        response(),
    ]);

    let mut downloaded = Vec::new();
    let received = client
        .download_file("test-token", "documents/test.bin", |chunk| {
            downloaded.extend_from_slice(chunk);
            Ok(())
        })
        .unwrap();
    assert_eq!(received, FILE_BYTES.len());
    assert_eq!(downloaded, FILE_BYTES);
    assert_eq!(client.post(PATH, BODY).unwrap(), RESPONSE_BODY);

    assert_finished(&state, 2, 1);
}

#[test]
fn aborted_download_forces_reconnect_before_next_post() {
    let (mut client, state) = client(vec![
        Step::Connect(Ok(())),
        write_ok(download_request()),
        Step::Read(Ok(
            b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\n".to_vec(),
        )),
        Step::Read(Ok(FILE_BYTES[..4].to_vec())),
        Step::Connect(Ok(())),
        write_ok(request_header(BODY)),
        write_ok(BODY),
        response(),
    ]);

    let mut callbacks = 0;
    let result = client.download_file("test-token", "documents/test.bin", |chunk| {
        callbacks += 1;
        assert_eq!(chunk, &FILE_BYTES[..4]);
        anyhow::bail!("simulated download callback abort")
    });
    assert_eq!(callbacks, 1);
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("simulated download callback abort"));
    assert_eq!(client.post(PATH, BODY).unwrap(), RESPONSE_BODY);

    assert_finished(&state, 2, 1);
}

#[test]
fn retry_body_failure_has_unknown_outcome_without_another_replay() {
    let (mut client, state) = client(vec![
        Step::Connect(Ok(())),
        write_failed(request_header(BODY), 0),
        Step::Connect(Ok(())),
        write_ok(request_header(BODY)),
        write_failed(BODY, BODY.len() / 2),
    ]);

    let result = client.post(PATH, BODY);

    assert!(matches!(result, Err(TelegramHttpError::OutcomeUnknown(_))));
    assert_finished(&state, 2, 1);
    assert_failed_connection_closed(&state);
}
