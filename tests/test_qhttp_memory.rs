//! Heap allocation guardrails for modem HTTP helpers.

#[path = "support/alloc_counter.rs"]
mod alloc_counter;

use smsgate::modem::a76xx::qhttp::escape_at_quotes_for_test;

#[test]
fn at_quote_escape_allocations_are_bounded() {
    let (escaped, allocations) =
        alloc_counter::count_allocations(|| escape_at_quotes_for_test("apn\\name\"quoted\""));

    assert_eq!(escaped, "apn\\\\name\\\"quoted\\\"");
    assert!(
        allocations <= 1,
        "AT quote escaping allocated {allocations} times; expected one output allocation"
    );
}
