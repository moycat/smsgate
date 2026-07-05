//! Heap allocation guardrails for AT response collection.

#[path = "support/alloc_counter.rs"]
mod alloc_counter;

use smsgate::modem::a76xx::at::AtPort;
use smsgate::testing::mocks::MockUart;

#[test]
fn multiline_at_response_allocations_are_bounded_by_line_count() {
    let mut uart = MockUart::new();
    for i in 0..32u32 {
        uart.queue_response_line(&format!("+DATA: {i:02},payload"));
    }
    uart.queue_response_line("OK");
    let mut port = AtPort::new(uart);

    let (response, allocations) = alloc_counter::count_allocations(|| port.send_at("+TEST"));

    let response = response.unwrap();
    assert!(response.ok);
    assert_eq!(response.body.lines().count(), 32);
    assert!(
        allocations <= 50,
        "multiline AT response allocated {allocations} times; expected one body accumulator plus line reads"
    );
}
