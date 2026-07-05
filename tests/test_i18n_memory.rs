//! Heap allocation guardrails for user-visible text rendering.

#[path = "support/alloc_counter.rs"]
mod alloc_counter;

#[test]
fn sms_received_html_escape_allocations_are_bounded() {
    let (text, allocations) = alloc_counter::count_allocations(|| {
        smsgate::i18n::sms_received(
            "ACME & Co <ops>",
            "2026-07-04T19:44:00-07:00",
            "2 < 3 & 5 > 4",
        )
    });

    assert!(text.contains("ACME &amp; Co &lt;ops&gt;"));
    assert!(text.contains("2 &lt; 3 &amp; 5 &gt; 4"));
    assert!(
        allocations <= 5,
        "sms_received allocated {allocations} times; expected single-pass HTML escaping"
    );
}
