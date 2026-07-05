//! Heap allocation guardrails for SMS storage sweeps.

#[path = "support/alloc_counter.rs"]
mod alloc_counter;

use smsgate::bridge::sms_handler::read_stored_sms;
use smsgate::testing::{mocks::ScriptedModem, pdu};

const HELLO_PDU: &str = "00040D91683108108300F0000062400110000000 05C8329BFD06";

#[test]
fn stored_pdu_sweep_allocations_are_bounded_by_record_count() {
    let mut body = String::new();
    for index in 1..=16 {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(&format!("+CMGL: {index},0,,18\n{}", pdu(HELLO_PDU)));
    }
    let mut modem = ScriptedModem::new()
        .expect("+CMGF=0", "", true)
        .expect("+CMGL=4", &body, true);

    let (stored, allocations) =
        alloc_counter::count_allocations(|| read_stored_sms("ME", &mut modem));

    modem.check_consumed();
    assert_eq!(stored.len(), 16);
    assert!(
        allocations <= 80,
        "stored SMS sweep allocated {allocations} times; expected parser allocations to scale tightly with records"
    );
}
