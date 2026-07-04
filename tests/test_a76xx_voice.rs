//! A76xx voice-call behavior tests.

use smsgate::modem::{a76xx::A76xxModem, ModemPort};
use smsgate::testing::mocks::MockUart;

#[test]
fn a76xx_hang_up_uses_chup() {
    let mut uart = MockUart::new();
    uart.queue_response_line("OK");
    let mut modem = A76xxModem::new_test(uart);

    modem.hang_up().unwrap();

    assert_eq!(modem.port().inner().sent_str(), "AT+CHUP\r");
}
