#![no_main]

use libfuzzer_sys::fuzz_target;
use smsgate::commands::{builtin::*, Command, CommandContext, CommandRegistry};
use smsgate::log_ring::LogRing;
use smsgate::modem::ModemStatus;
use smsgate::persist::mem::MemStore;
use smsgate::sms::sender::SmsSender;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else {
        return;
    };

    let mut reg = CommandRegistry::new();
    reg.register(Box::new(HelpCommand {
        help_text: String::new(),
    }));
    reg.register(Box::new(StatusCommand));
    reg.register(Box::new(SendCommand));
    reg.register(Box::new(LogCommand));
    reg.register(Box::new(BlockCommand));
    reg.register(Box::new(UnblockCommand));
    reg.register(Box::new(PauseCommand));
    reg.register(Box::new(ResumeCommand));
    reg.register(Box::new(RestartCommand));

    let store = MemStore::new();
    let status = ModemStatus::default();
    let log = LogRing::new();
    let queue = SmsSender::new();
    let ctx = CommandContext {
        store: &store,
        modem_status: &status,
        log_ring: &log,
        send_queue: &queue,
        uptime_ms: 0,
        free_heap_bytes: 0,
        wifi_info: "",
    };

    let _ = reg.dispatch(s, &ctx);
});
