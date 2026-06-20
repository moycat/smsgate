//! Command dispatch and handler tests.

use smsgate::commands::{
    builtin::*,
    Command, CommandContext, CommandRegistry,
};
use smsgate::i18n;
use smsgate::log_ring::{LogEntry, LogRing};
use smsgate::modem::ModemStatus;
use smsgate::persist::{keys, mem::MemStore, save_bool};
use smsgate::sms::sender::SmsSender;

fn make_registry() -> CommandRegistry {
    let mut r = CommandRegistry::new();
    r.register(Box::new(HelpCommand { help_text: "help text".to_string() }));
    r.register(Box::new(StatusCommand));
    r.register(Box::new(SendCommand));
    r.register(Box::new(LogCommand));
    r.register(Box::new(BlockCommand));
    r.register(Box::new(UnblockCommand));
    r.register(Box::new(PauseCommand));
    r.register(Box::new(ResumeCommand));
    r.register(Box::new(RestartCommand));
    r
}

fn ctx<'a>(
    store: &'a dyn smsgate::persist::Store,
    status: &'a ModemStatus,
    log: &'a LogRing,
    queue: &'a SmsSender,
) -> CommandContext<'a> {
    CommandContext { store, modem_status: status, log_ring: log, send_queue: queue, uptime_ms: 12345, free_heap_bytes: 0, wifi_info: "" }
}

#[test]
fn registry_help_dispatches() {
    let reg = make_registry();
    let store = MemStore::new();
    let status = ModemStatus::default();
    let log = LogRing::new();
    let queue = SmsSender::new();
    let result = reg.dispatch("/help", &ctx(&store, &status, &log, &queue));
    assert!(result.is_some());
    assert_eq!(result.unwrap(), "help text");
}

#[test]
fn registry_unknown_command_returns_none() {
    let reg = make_registry();
    let store = MemStore::new();
    let status = ModemStatus::default();
    let log = LogRing::new();
    let queue = SmsSender::new();
    let result = reg.dispatch("/nonexistent", &ctx(&store, &status, &log, &queue));
    assert!(result.is_none());
}

#[test]
fn registry_command_count_not_exceeded() {
    let reg = make_registry();
    assert_eq!(reg.command_list().len(), 9);
}

#[test]
fn registry_command_list_includes_all() {
    let reg = make_registry();
    let names: Vec<&str> = reg.command_list().iter().map(|(n, _)| *n).collect();
    assert!(names.contains(&"help"));
    assert!(names.contains(&"status"));
    assert!(names.contains(&"send"));
    assert!(names.contains(&"log"));
    assert!(names.contains(&"block"));
    assert!(names.contains(&"unblock"));
    assert!(names.contains(&"pause"));
    assert!(names.contains(&"resume"));
    assert!(names.contains(&"restart"));
}

#[test]
fn status_command_shows_uptime() {
    let store = MemStore::new();
    let status = ModemStatus { csq: 20, operator: "China Mobile".to_string(), registered: true };
    let log = LogRing::new();
    let queue = SmsSender::new();
    let ctx = CommandContext { store: &store, modem_status: &status, log_ring: &log, send_queue: &queue, uptime_ms: 3661_000, free_heap_bytes: 0, wifi_info: "" };
    let cmd = StatusCommand;
    let result = cmd.handle("", &ctx);
    assert!(result.contains("01h 01m 01s"), "uptime not found in: {}", result);
    assert!(result.contains("China Mobile"));
    assert!(result.contains(i18n::status_reg_ok()));
}

#[test]
fn status_command_paused_shown() {
    let mut store = MemStore::new();
    save_bool(&mut store, keys::FWD_ENABLED, false).unwrap();
    let status = ModemStatus::default();
    let log = LogRing::new();
    let queue = SmsSender::new();
    let ctx = ctx(&store, &status, &log, &queue);
    let result = StatusCommand.handle("", &ctx);
    assert!(result.contains(i18n::status_fwd_off()));
}

#[test]
fn log_command_empty() {
    let store = MemStore::new();
    let status = ModemStatus::default();
    let log = LogRing::new();
    let queue = SmsSender::new();
    let result = LogCommand.handle("", &ctx(&store, &status, &log, &queue));
    assert!(result.contains(i18n::log_empty()));
}

#[test]
fn log_command_shows_entries() {
    let store = MemStore::new();
    let status = ModemStatus::default();
    let mut log = LogRing::new();
    log.push(LogEntry { sender: "+1".to_string(), body_preview: "hello".to_string(), timestamp: "ts".to_string(), forwarded: true });
    let queue = SmsSender::new();
    let result = LogCommand.handle("3", &ctx(&store, &status, &log, &queue));
    assert!(result.contains("+1"));
    assert!(result.contains("hello"));
    assert!(result.contains("✅"));
}


#[test]
fn send_command_missing_args() {
    let store = MemStore::new();
    let status = ModemStatus::default();
    let log = LogRing::new();
    let queue = SmsSender::new();
    let result = SendCommand.handle("", &ctx(&store, &status, &log, &queue));
    assert!(result.contains(i18n::send_usage()));
}

#[test]
fn send_command_valid() {
    let store = MemStore::new();
    let status = ModemStatus::default();
    let log = LogRing::new();
    let queue = SmsSender::new();
    let result = SendCommand.handle("+8613800138000 Hello world", &ctx(&store, &status, &log, &queue));
    assert!(result.contains("+8613800138000"));
    assert!(result.contains("1"));
}

#[test]
fn send_command_too_long_rejected() {
    let store = MemStore::new();
    let status = ModemStatus::default();
    let log = LogRing::new();
    let queue = SmsSender::new();
    let long_body: String = "你好".repeat(500);
    let result = SendCommand.handle(&format!("+1 {}", long_body), &ctx(&store, &status, &log, &queue));
    assert!(result.contains(i18n::send_too_long()),
            "expected too-long message, got: {}", result);
}

#[test]
fn block_command_valid() {
    let store = MemStore::new();
    let status = ModemStatus::default();
    let log = LogRing::new();
    let queue = SmsSender::new();
    let result = BlockCommand.handle("10086", &ctx(&store, &status, &log, &queue));
    assert!(result.contains("10086"));
    assert!(result.contains(smsgate::commands::BLOCK_SENTINEL));
}

#[test]
fn block_command_missing_number() {
    let store = MemStore::new();
    let status = ModemStatus::default();
    let log = LogRing::new();
    let queue = SmsSender::new();
    let result = BlockCommand.handle("", &ctx(&store, &status, &log, &queue));
    assert!(result.contains(i18n::block_usage()));
}

#[test]
fn pause_command_default_60min() {
    let store = MemStore::new();
    let status = ModemStatus::default();
    let log = LogRing::new();
    let queue = SmsSender::new();
    let result = PauseCommand.handle("", &ctx(&store, &status, &log, &queue));
    assert!(result.contains("60"));
}

#[test]
fn pause_command_custom_duration() {
    let store = MemStore::new();
    let status = ModemStatus::default();
    let log = LogRing::new();
    let queue = SmsSender::new();
    let result = PauseCommand.handle("30", &ctx(&store, &status, &log, &queue));
    assert!(result.contains("30"));
}

#[test]
fn resume_command_when_already_active() {
    let store = MemStore::new();
    let status = ModemStatus::default();
    let log = LogRing::new();
    let queue = SmsSender::new();
    let result = ResumeCommand.handle("", &ctx(&store, &status, &log, &queue));
    assert!(result.contains(i18n::resume_already_active()));
}

#[test]
fn resume_command_when_paused() {
    let mut store = MemStore::new();
    save_bool(&mut store, keys::FWD_ENABLED, false).unwrap();
    let status = ModemStatus::default();
    let log = LogRing::new();
    let queue = SmsSender::new();
    let result = ResumeCommand.handle("", &ctx(&store, &status, &log, &queue));
    assert!(result.contains(smsgate::commands::RESUME_SENTINEL));
}

#[test]
fn restart_command_contains_sentinel() {
    let store = MemStore::new();
    let status = ModemStatus::default();
    let log = LogRing::new();
    let queue = SmsSender::new();
    let result = RestartCommand.handle("", &ctx(&store, &status, &log, &queue));
    assert!(result.contains(smsgate::commands::RESTART_SENTINEL));
}

#[test]
fn unblock_command_when_not_blocked() {
    let store = MemStore::new();
    let status = ModemStatus::default();
    let log = LogRing::new();
    let queue = SmsSender::new();
    let result = UnblockCommand.handle("10086", &ctx(&store, &status, &log, &queue));
    assert!(result.contains(i18n::unblock_not_found("10086").as_str()));
}

#[test]
fn unblock_command_missing_number() {
    let store = MemStore::new();
    let status = ModemStatus::default();
    let log = LogRing::new();
    let queue = SmsSender::new();
    let result = UnblockCommand.handle("", &ctx(&store, &status, &log, &queue));
    assert!(result.contains(i18n::unblock_usage()));
}

#[test]
fn unblock_command_when_blocked() {
    use smsgate::bridge::forwarder::add_to_blocklist;
    let mut store = MemStore::new();
    add_to_blocklist("10086", &mut store).unwrap();
    let status = ModemStatus::default();
    let log = LogRing::new();
    let queue = SmsSender::new();
    let result = UnblockCommand.handle("10086", &ctx(&store, &status, &log, &queue));
    assert!(result.contains(smsgate::commands::UNBLOCK_SENTINEL));
}


#[test]
fn send_command_empty_body() {
    let store = MemStore::new();
    let status = ModemStatus::default();
    let log = LogRing::new();
    let queue = SmsSender::new();
    let result = SendCommand.handle("+1  ", &ctx(&store, &status, &log, &queue));
    assert!(result.contains(i18n::send_empty_body()) || result.contains(i18n::send_usage()),
            "empty body should report error: {}", result);
}

#[test]
fn status_command_unknown_signal_and_operator() {
    let store = MemStore::new();
    let status = ModemStatus::default(); // csq=99, empty operator, not registered
    let log = LogRing::new();
    let queue = SmsSender::new();
    let ctx = CommandContext { store: &store, modem_status: &status, log_ring: &log, send_queue: &queue, uptime_ms: 0, free_heap_bytes: 0, wifi_info: "" };
    let result = StatusCommand.handle("", &ctx);
    assert!(result.contains("N/A"), "csq=99 should show N/A: {}", result);
    assert!(result.contains(i18n::status_op_unknown()), "empty operator: {}", result);
    assert!(result.contains(i18n::status_reg_no()), "not registered: {}", result);
}

#[test]
fn registry_help_text_contains_all_commands() {
    let reg = make_registry();
    let text = reg.help_text();
    assert!(text.contains("/help"));
    assert!(text.contains("/status"));
    assert!(text.contains("/send"));
    assert!(text.contains("/block"));
    assert!(text.contains("/unblock"));
    assert!(text.contains("/pause"));
    assert!(text.contains("/resume"));
    assert!(text.contains("/restart"));
}

#[test]
fn registry_strips_bot_username_suffix() {
    let reg = make_registry();
    let store = MemStore::new();
    let status = ModemStatus::default();
    let log = LogRing::new();
    let queue = SmsSender::new();
    let result = reg.dispatch("/help@mybot", &ctx(&store, &status, &log, &queue));
    assert!(result.is_some());
}

#[test]
#[should_panic(expected = "command cap exceeded")]
fn registry_enforces_11_command_cap() {
    let mut r = CommandRegistry::new();
    for _ in 0..11 {
        r.register(Box::new(RestartCommand));
    }
}
