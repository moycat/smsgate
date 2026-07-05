//! Heap allocation guardrails for Telegram request construction.

#[path = "support/alloc_counter.rs"]
mod alloc_counter;

use smsgate::im::telegram::build_set_my_commands_body;

#[test]
fn set_my_commands_body_allocations_are_bounded_by_output() {
    let commands = [
        ("start", "start gateway"),
        ("status", "device status"),
        ("log", "tail log"),
        ("forward", "forward toggle"),
        ("send", "send sms"),
        ("block", "block number"),
        ("unblock", "unblock number"),
        ("queue", "queue status"),
        ("reboot", "reboot device"),
        ("ota", "update firmware"),
    ];

    let (body, allocations) =
        alloc_counter::count_allocations(|| build_set_my_commands_body(&commands));

    assert!(body.contains(r#""command":"status""#));
    assert!(
        allocations <= 2,
        "setMyCommands body allocated {allocations} times; expected one output allocation"
    );
}
