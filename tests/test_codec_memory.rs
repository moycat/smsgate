//! Heap allocation guardrails for SMS codec helpers.

#[path = "support/alloc_counter.rs"]
mod alloc_counter;

use smsgate::sms::codec::{build_sms_submit_pdus, count_sms_parts};

#[test]
fn count_sms_parts_allocations_are_bounded() {
    let gsm7 = "A".repeat(161);
    let ucs2 = "你".repeat(71);

    let (gsm7_parts, gsm7_allocations) =
        alloc_counter::count_allocations(|| count_sms_parts(&gsm7, 10));
    let (ucs2_parts, ucs2_allocations) =
        alloc_counter::count_allocations(|| count_sms_parts(&ucs2, 10));

    assert_eq!(gsm7_parts, 2);
    assert_eq!(ucs2_parts, 2);
    assert_eq!(
        gsm7_allocations, 0,
        "GSM-7 part counting allocated {gsm7_allocations} times; expected allocation-free counting"
    );
    assert_eq!(
        ucs2_allocations, 0,
        "UCS-2 part counting allocated {ucs2_allocations} times; expected allocation-free counting"
    );
}

#[test]
fn single_part_pdu_encoding_allocations_are_bounded() {
    let (pdus, allocations) = alloc_counter::count_allocations(|| {
        build_sms_submit_pdus("+15551234567", "Hello", 10, false)
    });

    assert_eq!(pdus.len(), 1);
    assert!(
        allocations <= 7,
        "single-part PDU encoding allocated {allocations} times; expected BCD phone encoding without a digit-byte Vec"
    );
}
