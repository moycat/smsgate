//! Poller sentinel processing tests.

use smsgate::bridge::forwarder::is_blocked;
use smsgate::bridge::poller::poll_and_dispatch;
use smsgate::bridge::reply_router::ReplyRouter;
use smsgate::commands::{builtin::*, CommandRegistry};
use smsgate::im::InboundMessage;
use smsgate::log_ring::LogRing;
use smsgate::modem::ModemStatus;
use smsgate::persist::{keys, load_bool, mem::MemStore};
use smsgate::sms::sender::SmsSender;
use smsgate::testing::mocks::RecordingMessenger;

fn make_registry() -> CommandRegistry {
    let mut r = CommandRegistry::new();
    r.register(Box::new(HelpCommand {
        help_text: String::new(),
    }));
    r.register(Box::new(StatusCommand));
    r.register(Box::new(SendCommand));
    r.register(Box::new(LogCommand));
    r.register(Box::new(BlockCommand));
    r.register(Box::new(BlockListCommand));
    r.register(Box::new(UnblockCommand));
    r.register(Box::new(PauseCommand));
    r.register(Box::new(ResumeCommand));
    r.register(Box::new(RestartCommand));
    r
}

fn msg(text: &str) -> InboundMessage {
    InboundMessage {
        cursor: 1,
        text: text.to_string(),
        reply_to: None,
    }
}

fn reply_msg(text: &str, reply_to: i64) -> InboundMessage {
    InboundMessage {
        cursor: 1,
        text: text.to_string(),
        reply_to: Some(reply_to),
    }
}

#[test]
fn send_sentinel_enqueues_sms() {
    let mut store = MemStore::new();
    let mut messenger = RecordingMessenger::new();
    let router = ReplyRouter::new();
    let reg = make_registry();
    let log = LogRing::new();
    let status = ModemStatus::default();
    let mut sender = SmsSender::new();

    let result = poll_and_dispatch(
        &[msg("/send +8613800138000 Hello world")],
        &mut messenger,
        &mut sender,
        &router,
        &reg,
        &mut store,
        &log,
        &status,
        0,
        0,
        "",
    );
    assert!(result.is_ok());
    assert!(!result.unwrap().0); // no restart

    // SMS was enqueued
    assert_eq!(sender.len(), 1);
    let snap = sender.snapshot();
    assert_eq!(snap[0].phone, "+8613800138000");
    assert_eq!(snap[0].body_preview, "Hello world");

    // IM reply is clean (no sentinel)
    assert_eq!(messenger.sent_count(), 1);
    let reply = messenger.last_sent().unwrap();
    assert!(
        !reply.contains("__SEND__"),
        "sentinel leaked to IM: {}",
        reply
    );
    assert!(reply.contains("+8613800138000"));
}

#[test]
fn send_sentinel_rate_limited_after_5() {
    let mut store = MemStore::new();
    let router = ReplyRouter::new();
    let reg = make_registry();
    let log = LogRing::new();
    let status = ModemStatus::default();
    let mut sender = SmsSender::new();

    // Send 5 — all succeed
    let mut messenger;
    for i in 0..5u8 {
        messenger = RecordingMessenger::new();
        poll_and_dispatch(
            &[msg(&format!("/send +{} hello", i))],
            &mut messenger,
            &mut sender,
            &router,
            &reg,
            &mut store,
            &log,
            &status,
            0,
            0,
            "",
        )
        .unwrap();
        assert_eq!(sender.len(), (i + 1) as usize);
        let reply = messenger.last_sent().unwrap_or_default();
        assert!(
            !reply.contains("Rate limit"),
            "send {} should not be rate limited",
            i
        );
    }

    // 6th in the same window — rate limited, IM reply contains the error
    messenger = RecordingMessenger::new();
    poll_and_dispatch(
        &[msg("/send +9 hello")],
        &mut messenger,
        &mut sender,
        &router,
        &reg,
        &mut store,
        &log,
        &status,
        0,
        0,
        "",
    )
    .unwrap();
    assert_eq!(sender.len(), 5, "no new SMS queued when rate limited");
    let reply = messenger.last_sent().unwrap_or_default();
    assert!(
        reply.contains("Rate limit") || reply.contains("频率限制"),
        "rate limit message expected, got: {}",
        reply
    );
}

#[test]
fn block_sentinel_adds_to_blocklist() {
    let mut store = MemStore::new();
    let mut messenger = RecordingMessenger::new();
    let router = ReplyRouter::new();
    let reg = make_registry();
    let log = LogRing::new();
    let status = ModemStatus::default();
    let mut sender = SmsSender::new();

    poll_and_dispatch(
        &[msg("/block 10086")],
        &mut messenger,
        &mut sender,
        &router,
        &reg,
        &mut store,
        &log,
        &status,
        0,
        0,
        "",
    )
    .unwrap();

    assert!(is_blocked("10086", &store), "number should be blocked");
    let reply = messenger.last_sent().unwrap();
    assert!(!reply.contains("__BLOCK__"), "sentinel leaked: {}", reply);
}

#[test]
fn unblock_sentinel_removes_from_blocklist() {
    let mut store = MemStore::new();
    smsgate::bridge::forwarder::add_to_blocklist("10086", &mut store).unwrap();

    let mut messenger = RecordingMessenger::new();
    let router = ReplyRouter::new();
    let reg = make_registry();
    let log = LogRing::new();
    let status = ModemStatus::default();
    let mut sender = SmsSender::new();

    poll_and_dispatch(
        &[msg("/unblock 10086")],
        &mut messenger,
        &mut sender,
        &router,
        &reg,
        &mut store,
        &log,
        &status,
        0,
        0,
        "",
    )
    .unwrap();

    assert!(!is_blocked("10086", &store), "number should be unblocked");
    let reply = messenger.last_sent().unwrap();
    assert!(!reply.contains("__UNBLOCK__"), "sentinel leaked: {}", reply);
}

#[test]
fn pause_sentinel_disables_forwarding() {
    let mut store = MemStore::new();
    let mut messenger = RecordingMessenger::new();
    let router = ReplyRouter::new();
    let reg = make_registry();
    let log = LogRing::new();
    let status = ModemStatus::default();
    let mut sender = SmsSender::new();

    poll_and_dispatch(
        &[msg("/pause 30")],
        &mut messenger,
        &mut sender,
        &router,
        &reg,
        &mut store,
        &log,
        &status,
        0,
        0,
        "",
    )
    .unwrap();

    assert_eq!(load_bool(&store, keys::FWD_ENABLED), Some(false));
    let reply = messenger.last_sent().unwrap();
    assert!(!reply.contains("__PAUSE__"), "sentinel leaked: {}", reply);
}

#[test]
fn pause_sentinel_returns_duration() {
    let mut store = MemStore::new();
    let mut messenger = RecordingMessenger::new();
    let router = ReplyRouter::new();
    let reg = make_registry();
    let log = LogRing::new();
    let status = ModemStatus::default();
    let mut sender = SmsSender::new();

    let (restart, pause_mins) = poll_and_dispatch(
        &[msg("/pause 45")],
        &mut messenger,
        &mut sender,
        &router,
        &reg,
        &mut store,
        &log,
        &status,
        0,
        0,
        "",
    )
    .unwrap();
    assert!(!restart);
    assert_eq!(
        pause_mins,
        Some(45),
        "pause duration must be returned to caller"
    );
    assert_eq!(load_bool(&store, keys::FWD_ENABLED), Some(false));
}

#[test]
fn resume_sentinel_enables_forwarding() {
    let mut store = MemStore::new();
    smsgate::persist::save_bool(&mut store, keys::FWD_ENABLED, false).unwrap();

    let mut messenger = RecordingMessenger::new();
    let router = ReplyRouter::new();
    let reg = make_registry();
    let log = LogRing::new();
    let status = ModemStatus::default();
    let mut sender = SmsSender::new();

    poll_and_dispatch(
        &[msg("/resume")],
        &mut messenger,
        &mut sender,
        &router,
        &reg,
        &mut store,
        &log,
        &status,
        0,
        0,
        "",
    )
    .unwrap();

    assert_eq!(load_bool(&store, keys::FWD_ENABLED), Some(true));
    let reply = messenger.last_sent().unwrap();
    assert!(!reply.contains("__RESUME__"), "sentinel leaked: {}", reply);
}

#[test]
fn restart_sentinel_returns_true() {
    let mut store = MemStore::new();
    let mut messenger = RecordingMessenger::new();
    let router = ReplyRouter::new();
    let reg = make_registry();
    let log = LogRing::new();
    let status = ModemStatus::default();
    let mut sender = SmsSender::new();

    let result = poll_and_dispatch(
        &[msg("/restart")],
        &mut messenger,
        &mut sender,
        &router,
        &reg,
        &mut store,
        &log,
        &status,
        0,
        0,
        "",
    );
    assert!(result.is_ok());
    assert!(result.unwrap().0, "restart should be signalled");

    let reply = messenger.last_sent().unwrap();
    assert!(!reply.contains("__RESTART__"), "sentinel leaked: {}", reply);
}

#[test]
fn reply_to_sms_enqueues_outbound() {
    let mut store = MemStore::new();
    let mut messenger = RecordingMessenger::new();
    let mut router = ReplyRouter::new();
    let reg = make_registry();
    let log = LogRing::new();
    let status = ModemStatus::default();
    let mut sender = SmsSender::new();

    // Simulate a stored mapping: message 5000 → "+8613800138000"
    router.put(5000, "+8613800138000", &mut store);

    poll_and_dispatch(
        &[reply_msg("Reply text here", 5000)],
        &mut messenger,
        &mut sender,
        &router,
        &reg,
        &mut store,
        &log,
        &status,
        0,
        0,
        "",
    )
    .unwrap();

    // SMS should be enqueued to the original sender
    assert_eq!(sender.len(), 1);
    assert_eq!(sender.snapshot()[0].phone, "+8613800138000");
    assert_eq!(sender.snapshot()[0].body_preview, "Reply text here");
}

#[test]
fn non_command_non_reply_is_ignored() {
    let mut store = MemStore::new();
    let mut messenger = RecordingMessenger::new();
    let router = ReplyRouter::new();
    let reg = make_registry();
    let log = LogRing::new();
    let status = ModemStatus::default();
    let mut sender = SmsSender::new();

    poll_and_dispatch(
        &[msg("just some text")],
        &mut messenger,
        &mut sender,
        &router,
        &reg,
        &mut store,
        &log,
        &status,
        0,
        0,
        "",
    )
    .unwrap();

    // Nothing enqueued, no IM reply
    assert_eq!(sender.len(), 0);
    assert_eq!(messenger.sent_count(), 0);
}

#[test]
fn send_sentinel_body_with_pipe_char() {
    // Bodies containing '|' must not be truncated — split_once splits on first '|' only.
    let mut store = MemStore::new();
    let mut messenger = RecordingMessenger::new();
    let router = ReplyRouter::new();
    let reg = make_registry();
    let log = LogRing::new();
    let status = ModemStatus::default();
    let mut sender = SmsSender::new();

    poll_and_dispatch(
        &[msg("/send +1 Hello|world|test")],
        &mut messenger,
        &mut sender,
        &router,
        &reg,
        &mut store,
        &log,
        &status,
        0,
        0,
        "",
    )
    .unwrap();

    assert_eq!(sender.len(), 1);
    let snap = sender.snapshot();
    assert_eq!(snap[0].phone, "+1");
    assert_eq!(snap[0].body_preview, "Hello|world|test");
}

#[test]
fn send_sentinel_body_with_newline() {
    // Bodies containing '\n' must be encoded in the sentinel and decoded back.
    let mut store = MemStore::new();
    let mut messenger = RecordingMessenger::new();
    let router = ReplyRouter::new();
    let reg = make_registry();
    let log = LogRing::new();
    let status = ModemStatus::default();
    let mut sender = SmsSender::new();

    poll_and_dispatch(
        &[msg("/send +1 line1\nline2")],
        &mut messenger,
        &mut sender,
        &router,
        &reg,
        &mut store,
        &log,
        &status,
        0,
        0,
        "",
    )
    .unwrap();

    assert_eq!(sender.len(), 1);
    let snap = sender.snapshot();
    assert_eq!(snap[0].phone, "+1");
    assert_eq!(snap[0].body_preview, "line1\nline2");
}

#[test]
fn send_body_preview_truncated_at_50_chars() {
    // The Queued: display line shows at most 50 chars of the body.
    let mut store = MemStore::new();
    let mut messenger = RecordingMessenger::new();
    let router = ReplyRouter::new();
    let reg = make_registry();
    let log = LogRing::new();
    let status = ModemStatus::default();
    let mut sender = SmsSender::new();

    let long_body: String = "A".repeat(70);
    poll_and_dispatch(
        &[msg(&format!("/send +1 {}", long_body))],
        &mut messenger,
        &mut sender,
        &router,
        &reg,
        &mut store,
        &log,
        &status,
        0,
        0,
        "",
    )
    .unwrap();

    assert_eq!(sender.len(), 1);
    // snapshot body_preview is truncated to 30 chars by SmsSender
    let expected_30: String = "A".repeat(30);
    assert_eq!(sender.snapshot()[0].body_preview, expected_30);

    // Display reply to Telegram shows 50-char preview (send command truncation)
    let reply = messenger.last_sent().unwrap();
    let preview_50: String = "A".repeat(50);
    assert!(
        reply.contains(&preview_50),
        "reply should show 50-char preview: {}",
        reply
    );
}

#[test]
fn reply_to_unknown_id_does_not_enqueue() {
    let mut store = MemStore::new();
    let mut messenger = RecordingMessenger::new();
    let router = ReplyRouter::new(); // empty — no mappings
    let reg = make_registry();
    let log = LogRing::new();
    let status = ModemStatus::default();
    let mut sender = SmsSender::new();

    poll_and_dispatch(
        &[reply_msg("Reply to unknown", 9999)],
        &mut messenger,
        &mut sender,
        &router,
        &reg,
        &mut store,
        &log,
        &status,
        0,
        0,
        "",
    )
    .unwrap();

    assert_eq!(sender.len(), 0, "unknown reply_to should not enqueue SMS");
}

#[test]
fn unknown_command_sends_no_reply() {
    let mut store = MemStore::new();
    let mut messenger = RecordingMessenger::new();
    let router = ReplyRouter::new();
    let reg = make_registry();
    let log = LogRing::new();
    let status = ModemStatus::default();
    let mut sender = SmsSender::new();

    poll_and_dispatch(
        &[msg("/nonexistent_cmd")],
        &mut messenger,
        &mut sender,
        &router,
        &reg,
        &mut store,
        &log,
        &status,
        0,
        0,
        "",
    )
    .unwrap();

    // Unknown commands produce no reply
    assert_eq!(messenger.sent_count(), 0);
}
