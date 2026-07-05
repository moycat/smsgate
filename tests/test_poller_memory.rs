//! Heap allocation guardrails for command dispatch post-processing.

#[path = "support/alloc_counter.rs"]
mod alloc_counter;

use smsgate::bridge::{poller::poll_and_dispatch, reply_router::ReplyRouter};
use smsgate::commands::{builtin::*, CommandRegistry};
use smsgate::im::InboundMessage;
use smsgate::log_ring::LogRing;
use smsgate::modem::ModemStatus;
use smsgate::persist::mem::MemStore;
use smsgate::sms::sender::SmsSender;
use smsgate::testing::mocks::RecordingMessenger;

fn registry() -> CommandRegistry {
    let mut registry = CommandRegistry::new();
    registry.register(Box::new(HelpCommand {
        help_text: String::new(),
    }));
    registry.register(Box::new(StatusCommand));
    registry.register(Box::new(SendCommand));
    registry.register(Box::new(LogCommand));
    registry.register(Box::new(BlockCommand));
    registry.register(Box::new(BlockListCommand));
    registry.register(Box::new(UnblockCommand));
    registry.register(Box::new(PauseCommand));
    registry.register(Box::new(ResumeCommand));
    registry.register(Box::new(RestartCommand));
    registry
}

fn msg(text: &str) -> InboundMessage {
    InboundMessage {
        cursor: 1,
        text: text.to_string(),
        reply_to: None,
        document: None,
        callback: None,
    }
}

#[test]
fn send_command_dispatch_allocations_are_bounded() {
    let mut store = MemStore::new();
    let mut messenger = RecordingMessenger::new();
    let router = ReplyRouter::new();
    let registry = registry();
    let log = LogRing::new();
    let status = ModemStatus::default();
    let mut sender = SmsSender::new();
    let message = msg("/send +15551234567 hello from allocation guard");

    let (result, allocations) = alloc_counter::count_allocations(|| {
        poll_and_dispatch(
            &[message],
            &mut messenger,
            &mut sender,
            &router,
            &registry,
            &mut store,
            &log,
            &status,
            0,
            0,
            0,
            "",
        )
    });

    result.unwrap();
    assert_eq!(sender.len(), 1);
    assert!(
        allocations <= 40,
        "send command dispatch allocated {allocations} times; expected sentinel cleanup without temporary line collection"
    );
}
