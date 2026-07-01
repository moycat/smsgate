//! Host-side tests for `AtPort<MockUart>`.
//!
//! Run with: cargo test --no-default-features --features testing --test test_at

use smsgate::modem::a76xx::at::AtPort;
use smsgate::testing::mocks::MockUart;

fn port(uart: MockUart) -> AtPort<MockUart> {
    AtPort::new(uart)
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
    let result = p.send_at_with_timeout("+QHTTPREAD=30", std::time::Duration::from_millis(50));

    assert!(matches!(result, Err(smsgate::modem::ModemError::Timeout)));
    assert!(started.elapsed() < std::time::Duration::from_millis(250));
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

// ── send_at_connect_payload ───────────────────────────────────────────────────

#[test]
fn send_at_connect_payload_ok() {
    let mut uart = MockUart::new();
    // First write (the AT command) → CONNECT
    uart.queue_response_line("CONNECT");
    uart.finish_response();
    // Second write (the payload) → response lines + OK
    uart.queue_response_line("+QHTTPPOST: 0,200,52");
    uart.queue_response_line("OK");
    let mut p = port(uart);
    let r = p
        .send_at_connect_payload_with_timeout(
            "+QHTTPPOST=52,60,60",
            "{\"test\":1}",
            std::time::Duration::from_secs(30),
        )
        .unwrap();
    assert!(r.ok);
    let sent = p.inner().sent_str();
    assert!(sent.contains("{\"test\":1}"));
}

#[test]
fn send_at_connect_payload_error_before_connect() {
    let mut uart = MockUart::new();
    uart.queue_response_line("ERROR");
    let mut p = port(uart);
    let r = p
        .send_at_connect_payload_with_timeout(
            "+QHTTPPOST=52,60,60",
            "body",
            std::time::Duration::from_secs(30),
        )
        .unwrap();
    assert!(!r.ok);
}

#[test]
fn send_at_connect_payload_with_timeout_returns_quick_timeout() {
    let uart = MockUart::new();
    let mut p = port(uart);
    let started = std::time::Instant::now();
    let result = p.send_at_connect_payload_with_timeout(
        "+QHTTPPOST=52,30,30",
        "body",
        std::time::Duration::from_millis(50),
    );

    assert!(matches!(result, Err(smsgate::modem::ModemError::Timeout)));
    assert!(started.elapsed() < std::time::Duration::from_millis(250));
}

// ── buffer caps ───────────────────────────────────────────────────────────────

#[test]
fn body_lines_capped() {
    // Feed 70 body lines + OK; only MAX_BODY_LINES (64) should survive.
    let mut uart = MockUart::new();
    for i in 0..70u32 {
        uart.queue_response_line(&format!("line{}", i));
    }
    uart.queue_response_line("OK");
    let mut p = port(uart);
    let r = p.send_at("+TEST").unwrap();
    assert!(r.ok);
    let count = r.body.lines().count();
    assert!(count <= 64, "expected at most 64 body lines, got {}", count);
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
}
