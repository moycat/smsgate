//! Host-side tests for `AtPort<MockUart>`.
//!
//! Run with: cargo test --no-default-features --features testing --test test_at

use smsgate::modem::a76xx::at::{AtPort, UartPort};
use smsgate::testing::mocks::MockUart;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct SharedUart {
    rx: Arc<Mutex<VecDeque<u8>>>,
}

struct ControlledUart {
    inner: Arc<Mutex<MockUart>>,
}

struct NoisyUart {
    offset: usize,
    response: VecDeque<u8>,
    command_written: bool,
}

impl UartPort for NoisyUart {
    fn read_byte(&mut self, _ticks: u32) -> Option<u8> {
        if self.command_written {
            self.response.pop_front()
        } else {
            const NOISE: &[u8] = b"+CGEV: ME PDN ACT 8,0\r\n";
            let byte = NOISE[self.offset % NOISE.len()];
            self.offset += 1;
            Some(byte)
        }
    }

    fn write_all(&mut self, _data: &[u8]) -> Result<(), smsgate::modem::ModemError> {
        self.command_written = true;
        Ok(())
    }
}

impl UartPort for SharedUart {
    fn read_byte(&mut self, _ticks: u32) -> Option<u8> {
        self.rx.lock().unwrap().pop_front()
    }

    fn write_all(&mut self, _data: &[u8]) -> Result<(), smsgate::modem::ModemError> {
        Ok(())
    }
}

impl UartPort for ControlledUart {
    fn read_byte(&mut self, ticks: u32) -> Option<u8> {
        self.inner.lock().unwrap().read_byte(ticks)
    }

    fn write_all(&mut self, data: &[u8]) -> Result<(), smsgate::modem::ModemError> {
        self.inner.lock().unwrap().write_all(data)
    }
}

fn port(uart: MockUart) -> AtPort<MockUart> {
    AtPort::new(uart)
}

#[test]
fn continuous_urcs_do_not_block_at_command() {
    let mut port = AtPort::new(NoisyUart {
        offset: 0,
        response: VecDeque::from(b"+CSQ: 13,0\r\nOK\r\n".to_vec()),
        command_written: false,
    });
    let start = Instant::now();
    let response = port
        .send_at_with_timeout("+CSQ", Duration::from_secs(1))
        .unwrap();
    assert!(response.ok);
    assert_eq!(response.body, "+CSQ: 13,0");
    assert!(start.elapsed() < Duration::from_secs(1));
}

// ── send_at ──────────────────────────────────────────────────────────────────

#[test]
fn send_at_ok() {
    let mut uart = MockUart::new();
    uart.queue_response_line("OK");
    let mut p = port(uart);
    let r = p.send_at("+CSQ").unwrap();
    assert!(r.ok);
    assert_eq!(r.body, "");
    assert!(p.inner().sent_str().contains("AT+CSQ\r"));
}

#[test]
fn send_at_error() {
    let mut uart = MockUart::new();
    uart.queue_response_line("ERROR");
    let mut p = port(uart);
    let r = p.send_at("+CMGR=1").unwrap();
    assert!(!r.ok);
    assert!(r.body.contains("ERROR"));
}

#[test]
fn send_at_cme_error() {
    let mut uart = MockUart::new();
    uart.queue_response_line("+CME ERROR: 10");
    let mut p = port(uart);
    let r = p.send_at("+CPMS?").unwrap();
    assert!(!r.ok);
    assert!(r.body.contains("+CME ERROR"));
}

#[test]
fn send_at_cms_error() {
    let mut uart = MockUart::new();
    uart.queue_response_line("+CMS ERROR: 302");
    let mut p = port(uart);
    let r = p.send_at("+CMGS=10").unwrap();
    assert!(!r.ok);
    assert!(r.body.contains("+CMS ERROR"));
}

#[test]
fn send_at_body_lines() {
    let mut uart = MockUart::new();
    uart.queue_response_line("+CSQ: 20,0");
    uart.queue_response_line("OK");
    let mut p = port(uart);
    let r = p.send_at("+CSQ").unwrap();
    assert!(r.ok);
    assert_eq!(r.body, "+CSQ: 20,0");
}

#[test]
fn send_at_multiline_body() {
    let mut uart = MockUart::new();
    uart.queue_response_line("+COPS: 0,0,\"Operator\",7");
    uart.queue_response_line("+COPS: (1,\"Op1\",\"Op1\"),(2,\"Op2\",\"Op2\")");
    uart.queue_response_line("OK");
    let mut p = port(uart);
    let r = p.send_at("+COPS=?").unwrap();
    assert!(r.ok);
    let lines: Vec<_> = r.body.lines().collect();
    assert_eq!(lines.len(), 2);
}

#[test]
fn send_at_with_timeout_returns_quick_timeout() {
    let uart = MockUart::new();
    let mut p = port(uart);
    let started = std::time::Instant::now();
    let result = p.send_at_with_timeout("+CSQ", std::time::Duration::from_millis(50));

    assert!(matches!(result, Err(smsgate::modem::ModemError::Timeout)));
    assert!(started.elapsed() < std::time::Duration::from_millis(250));
}

#[test]
fn timed_out_command_requires_probe_marker_before_next_response() {
    let uart = Arc::new(Mutex::new(MockUart::new()));
    let mut p = AtPort::new(ControlledUart {
        inner: uart.clone(),
    });
    assert!(matches!(
        p.send_at_with_timeout("+CMGL=4", std::time::Duration::from_millis(10)),
        Err(smsgate::modem::ModemError::Timeout)
    ));

    // The old command's late OK arrives after the probe write. It must not
    // complete the probe without the distinctive +CMGF marker.
    {
        let mut source = uart.lock().unwrap();
        source.queue_response_line("OK");
        source.queue_response_line("+CMTI: \"ME\",7");
        source.queue_response_line("+CMGF: 0");
        source.queue_response_line("OK");
        source.finish_response();
        source.queue_response_line("+CSQ: 19,0");
        source.queue_response_line("OK");
    }
    let response = p
        .send_at_with_timeout("+CSQ", std::time::Duration::from_secs(1))
        .unwrap();
    assert_eq!(response.body, "+CSQ: 19,0");
    assert_eq!(p.poll_urc().as_deref(), Some("+CMTI: \"ME\",7"));
    let sent = uart.lock().unwrap().sent_str().to_owned();
    assert!(sent.contains("AT+CMGL=4\rAT+CMGF?\rAT+CSQ\r"));
}

// ── URC handling ─────────────────────────────────────────────────────────────

#[test]
fn urc_piggybacked_during_command() {
    // A +CMTI URC arrives in the middle of a command response.
    // It must end up in the URC buffer, not the command body.
    let mut uart = MockUart::new();
    uart.queue_response_line("+CSQ: 18,0");
    uart.queue_response_line("+CMTI: \"ME\",3");
    uart.queue_response_line("OK");
    let mut p = port(uart);
    let r = p.send_at("+CSQ").unwrap();
    assert!(r.ok);
    assert_eq!(r.body, "+CSQ: 18,0"); // URC not in body
    let urc = p.poll_urc().expect("URC should be buffered");
    assert!(urc.contains("+CMTI"));
}

#[test]
fn poll_urc_direct() {
    let mut uart = MockUart::new();
    uart.feed_line("+CMTI: \"ME\",1"); // immediately visible (simulates FIFO-resident URC)
    let mut p = port(uart);
    let urc = p.poll_urc().expect("URC available");
    assert!(urc.contains("+CMTI"));
}

#[test]
fn poll_urc_empty_returns_none() {
    let uart = MockUart::new();
    let mut p = port(uart);
    assert!(p.poll_urc().is_none());
}

#[test]
fn poll_urc_keeps_fragment_until_lf() {
    let rx = Arc::new(Mutex::new(VecDeque::from(b"+CMT".to_vec())));
    let mut p = AtPort::new(SharedUart { rx: rx.clone() });
    assert!(
        p.poll_urc().is_none(),
        "unterminated URC must not be returned"
    );
    rx.lock().unwrap().extend(b"I: \"ME\",3\r\n");
    assert_eq!(p.poll_urc().as_deref(), Some("+CMTI: \"ME\",3"));
}

#[test]
fn cgev_interleaved_with_cmgr_is_not_sms_body() {
    let mut uart = MockUart::new();
    uart.queue_response_line("+CMGR: 0,,3");
    uart.queue_response_line("+CGEV: ME PDN DEACT 8");
    uart.queue_response_line("001122");
    uart.queue_response_line("OK");
    let mut p = port(uart);
    let r = p.send_at("+CMGR=1").unwrap();
    assert_eq!(r.body, "+CMGR: 0,,3\n001122");
    assert_eq!(p.poll_urc().as_deref(), Some("+CGEV: ME PDN DEACT 8"));
}

#[test]
fn cmt_pdu_interleaved_with_command_stays_with_header() {
    let mut uart = MockUart::new();
    uart.queue_response_line("+CMT: ,3");
    uart.queue_response_line("001122");
    uart.queue_response_line("+CSQ: 20,0");
    uart.queue_response_line("OK");
    let mut p = port(uart);
    let r = p.send_at("+CSQ").unwrap();
    assert_eq!(r.body, "+CSQ: 20,0");
    assert_eq!(p.poll_urc().as_deref(), Some("+CMT: ,3"));
    assert_eq!(p.poll_urc().as_deref(), Some("001122"));
}

#[test]
fn cmt_header_survives_command_completion_before_pdu() {
    let mut uart = MockUart::new();
    uart.queue_response_line("+CMT: ,3");
    uart.queue_response_line("OK");
    uart.finish_response();
    uart.queue_response_line("001122");
    uart.queue_response_line("OK");
    let mut p = port(uart);
    assert!(p.send_at("+CSQ").unwrap().ok);
    assert!(p.send_at("+COPS?").unwrap().ok);
    assert_eq!(p.poll_urc().as_deref(), Some("+CMT: ,3"));
    assert_eq!(p.poll_urc().as_deref(), Some("001122"));
}

// ── wait_for_prompt ───────────────────────────────────────────────────────────

#[test]
fn wait_for_prompt_found() {
    let mut uart = MockUart::new();
    uart.feed(b"> ");
    let mut p = port(uart);
    let found = p.wait_for_prompt(b'>', std::time::Duration::from_millis(200));
    assert!(found);
}

#[test]
fn wait_for_prompt_timeout() {
    let uart = MockUart::new(); // empty — no '>' will arrive
    let mut p = port(uart);
    let found = p.wait_for_prompt(b'>', std::time::Duration::from_millis(50));
    assert!(!found);
}

// ── CMGS with concurrent URCs ──────────────────────────────────────────────

#[test]
fn cmgs_preserves_urcs_before_prompt_and_after_payload() {
    let mut uart = MockUart::new();
    uart.queue_response_line("+CMTI: \"ME\",7");
    uart.queue_response_line("RING");
    uart.queue_response(b"> ");
    uart.finish_response();
    uart.queue_response_line("+CGEV: ME PDN DEACT 8");
    uart.queue_response_line("+CMGS: 17");
    uart.queue_response_line("OK");

    let mut p = port(uart);
    assert_eq!(p.send_cmgs_pdu("001122", 3).unwrap(), 17);
    assert_eq!(p.poll_urc().as_deref(), Some("+CMTI: \"ME\",7"));
    assert_eq!(p.poll_urc().as_deref(), Some("RING"));
    assert_eq!(p.poll_urc().as_deref(), Some("+CGEV: ME PDN DEACT 8"));
    assert!(p.inner().sent_str().contains("AT+CMGS=3\r001122\u{1a}"));
}

#[test]
fn cmgs_keeps_interleaved_cmt_pair_during_final_result() {
    let mut uart = MockUart::new();
    uart.queue_response(b"> ");
    uart.finish_response();
    uart.queue_response_line("+CMT: ,3");
    uart.queue_response_line("001122");
    uart.queue_response_line("+CMGS: 4");
    uart.queue_response_line("OK");

    let mut p = port(uart);
    assert_eq!(p.send_cmgs_pdu("AABBCC", 3).unwrap(), 4);
    assert_eq!(p.poll_urc().as_deref(), Some("+CMT: ,3"));
    assert_eq!(p.poll_urc().as_deref(), Some("001122"));
}

#[test]
fn cmgs_error_before_prompt_aborts_input() {
    let mut uart = MockUart::new();
    uart.queue_response_line("+CMS ERROR: 500");
    let mut p = port(uart);
    let result = p.send_cmgs_pdu("001122", 3);
    assert!(matches!(
        result,
        Err(smsgate::modem::ModemError::AtError(message)) if message == "+CMS ERROR: 500"
    ));
    assert_eq!(p.inner().sent_str(), "AT+CMGS=3\r\u{1b}");
}

// ── buffer caps ───────────────────────────────────────────────────────────────

#[test]
fn body_lines_capped() {
    // A truncated response must fail rather than masquerade as a full reply.
    let mut uart = MockUart::new();
    for i in 0..70u32 {
        uart.queue_response_line(&format!("line{}", i));
    }
    uart.queue_response_line("OK");
    let mut p = port(uart);
    assert!(matches!(
        p.send_at("+TEST"),
        Err(smsgate::modem::ModemError::ResponseTooLong)
    ));
    assert_eq!(p.take_diagnostics().dropped_response_lines, 6);
}

#[test]
fn body_bytes_capped_even_when_line_count_is_small() {
    let mut uart = MockUart::new();
    for _ in 0..30 {
        uart.queue_response_line(&"A".repeat(800));
    }
    uart.queue_response_line("OK");
    let mut port = port(uart);
    assert!(matches!(
        port.send_at("+TEST"),
        Err(smsgate::modem::ModemError::ResponseTooLong)
    ));
    assert!(port.take_diagnostics().dropped_response_lines > 0);
}

#[test]
fn streaming_at_response_reads_past_body_cap_and_preserves_urcs() {
    let mut uart = MockUart::new();
    for index in 1..=100 {
        uart.queue_response_line(&format!("+CMGL: {index},0,,18"));
        uart.queue_response_line("001122");
        if index == 50 {
            uart.queue_response_line("+CMTI: \"ME\",101");
        }
    }
    uart.queue_response_line("OK");
    let mut p = port(uart);
    let mut headers = Vec::new();
    p.send_at_streaming(
        "+CMGL=4",
        std::time::Duration::from_millis(100),
        std::time::Duration::from_secs(1),
        &mut |line| {
            if line.starts_with("+CMGL:") {
                headers.push(line.to_owned());
            }
        },
    )
    .unwrap();
    assert_eq!(headers.len(), 100);
    assert_eq!(p.poll_urc().as_deref(), Some("+CMTI: \"ME\",101"));
    assert_eq!(p.take_diagnostics().dropped_response_lines, 0);
}

#[test]
fn storage_listing_yields_to_incoming_call_urcs() {
    let mut uart = MockUart::new();
    uart.queue_response_line("+CMGL: 1,0,,18");
    uart.queue_response_line("001122");
    uart.queue_response_line("RING");
    uart.queue_response_line("+CLIP: \"+15551234567\",145");
    uart.queue_response_line("OK");
    let mut p = port(uart);

    assert!(matches!(
        p.send_at_streaming(
            "+CMGL=4",
            std::time::Duration::from_secs(10),
            std::time::Duration::from_secs(120),
            &mut |_| {},
        ),
        Err(smsgate::modem::ModemError::InterruptedForCall)
    ));
    assert_eq!(p.poll_urc().as_deref(), Some("RING"));
    assert_eq!(p.poll_urc().as_deref(), Some("+CLIP: \"+15551234567\",145"));
}

#[test]
fn stored_sms_read_yields_to_incoming_call() {
    let mut uart = MockUart::new();
    uart.queue_response_line("RING");
    uart.queue_response_line("OK");
    let mut p = port(uart);

    assert!(matches!(
        p.send_at("+CMGR=3"),
        Err(smsgate::modem::ModemError::InterruptedForCall)
    ));
    assert_eq!(p.poll_urc().as_deref(), Some("RING"));
}

#[test]
fn urc_buf_capped() {
    // Flood with 40 URCs piggybacked in a command response; MAX_URC_BUF = 32.
    let mut uart = MockUart::new();
    for i in 0..40u32 {
        uart.queue_response_line(&format!("+CMTI: \"ME\",{}", i));
    }
    uart.queue_response_line("OK");
    let mut p = port(uart);
    let _r = p.send_at("+TEST").unwrap();
    let mut count = 0usize;
    while p.poll_urc().is_some() {
        count += 1;
    }
    assert!(
        count <= 32,
        "expected at most 32 buffered URCs, got {}",
        count
    );
    assert_eq!(p.take_diagnostics().dropped_urcs, 8);
}

#[test]
fn overlong_line_is_discarded_without_splitting_next_line() {
    let mut uart = MockUart::new();
    uart.queue_response(&vec![b'A'; 16 * 1024 + 1]);
    uart.queue_response(b"\r\n");
    uart.queue_response_line("+CSQ: 18,0");
    uart.queue_response_line("OK");
    let mut p = port(uart);
    let r = p.send_at("+CSQ").unwrap();
    assert_eq!(r.body, "+CSQ: 18,0");
    assert_eq!(p.take_diagnostics().overlong_lines, 1);
}

#[test]
fn cgev_coalescing_preserves_critical_urcs() {
    let mut uart = MockUart::new();
    for i in 0..40 {
        uart.queue_response_line(&format!("+CGEV: ME PDN DEACT {i}"));
    }
    uart.queue_response_line("+CMTI: \"ME\",3");
    uart.queue_response_line("OK");
    let mut p = port(uart);
    assert!(p.send_at("+TEST").unwrap().ok);
    assert_eq!(p.poll_urc().as_deref(), Some("+CGEV: ME PDN DEACT 39"));
    assert_eq!(p.poll_urc().as_deref(), Some("+CMTI: \"ME\",3"));
    let diagnostics = p.take_diagnostics();
    assert_eq!(diagnostics.dropped_urcs, 0);
    assert_eq!(diagnostics.pdn_deactivations, 40);
}

#[test]
fn critical_urc_evicts_cgev_when_queue_is_full() {
    let mut uart = MockUart::new();
    for i in 0..31 {
        uart.queue_response_line(&format!("+CMTI: \"ME\",{i}"));
    }
    uart.queue_response_line("+CGEV: ME PDN DEACT 8");
    uart.queue_response_line("RING");
    uart.queue_response_line("OK");
    let mut p = port(uart);
    assert!(p.send_at("+TEST").unwrap().ok);
    for _ in 0..31 {
        assert!(p.poll_urc().unwrap().starts_with("+CMTI:"));
    }
    assert_eq!(p.poll_urc().as_deref(), Some("RING"));
    assert_eq!(p.take_diagnostics().dropped_urcs, 0);
}

#[test]
fn call_evicts_recoverable_cmti_when_queue_is_full() {
    let mut uart = MockUart::new();
    for i in 0..32 {
        uart.queue_response_line(&format!("+CMTI: \"ME\",{i}"));
    }
    uart.queue_response_line("RING");
    uart.queue_response_line("OK");
    let mut p = port(uart);
    assert!(p.send_at("+TEST").unwrap().ok);
    assert_eq!(p.poll_urc().as_deref(), Some("+CMTI: \"ME\",1"));
    for _ in 1..31 {
        assert!(p.poll_urc().unwrap().starts_with("+CMTI:"));
    }
    assert_eq!(p.poll_urc().as_deref(), Some("RING"));
    assert_eq!(p.take_diagnostics().dropped_urcs, 1);
}

#[test]
fn full_urc_queue_preserves_cmt_by_evicting_recoverable_cmti() {
    let mut uart = MockUart::new();
    for i in 0..31 {
        uart.queue_response_line(&format!("+CMTI: \"ME\",{i}"));
    }
    uart.queue_response_line("+CMT: ,3");
    uart.queue_response_line("001122");
    uart.queue_response_line("OK");
    let mut p = port(uart);
    assert!(p.send_at("+TEST").unwrap().ok);
    for _ in 0..30 {
        assert!(p.poll_urc().unwrap().starts_with("+CMTI:"));
    }
    assert_eq!(p.poll_urc().as_deref(), Some("+CMT: ,3"));
    assert_eq!(p.poll_urc().as_deref(), Some("001122"));
    assert!(p.poll_urc().is_none());
    assert_eq!(p.take_diagnostics().dropped_urcs, 1);
}
