//! Concatenated SMS reassembly tests.

use smsgate::sms::{
    codec::SmsPdu,
    concat::{ConcatReassembler, FeedOutcome, StorageSlot},
};

fn slot(mem: &str, index: u16) -> StorageSlot {
    StorageSlot {
        mem: mem.to_string(),
        index,
        fingerprint: u64::from(index),
    }
}

fn make_part(sender: &str, ref_num: u16, total: u8, part: u8, content: &str) -> SmsPdu {
    SmsPdu {
        sender: sender.to_string(),
        timestamp: "26/04/10,12:00:00+00".to_string(),
        content: content.to_string(),
        is_concatenated: true,
        concat_ref: ref_num,
        concat_total: total,
        concat_part: part,
        ..Default::default()
    }
}

#[test]
fn two_parts_reassembled_in_order() {
    let mut r = ConcatReassembler::new();
    let r1 = r.feed(&make_part("+8613800138000", 1, 2, 1, "Hello "));
    assert!(r1.is_none(), "should wait for part 2");
    let r2 = r.feed(&make_part("+8613800138000", 1, 2, 2, "World"));
    assert!(r2.is_some());
    let complete = r2.unwrap();
    assert_eq!(complete.content, "Hello World");
    assert_eq!(complete.sender, "+8613800138000");
}

#[test]
fn two_parts_reassembled_out_of_order() {
    let mut r = ConcatReassembler::new();
    let r2 = r.feed(&make_part("+8613800138000", 2, 2, 2, "World"));
    assert!(r2.is_none());
    let r1 = r.feed(&make_part("+8613800138000", 2, 2, 1, "Hello "));
    assert!(r1.is_some());
    assert_eq!(r1.unwrap().content, "Hello World");
}

#[test]
fn three_parts_reassembled() {
    let mut r = ConcatReassembler::new();
    assert!(r.feed(&make_part("A", 1, 3, 1, "one ")).is_none());
    assert!(r.feed(&make_part("A", 1, 3, 3, " three")).is_none());
    let done = r.feed(&make_part("A", 1, 3, 2, "two"));
    assert!(done.is_some());
    assert_eq!(done.unwrap().content, "one two three");
}

#[test]
fn duplicate_part_ignored() {
    let mut r = ConcatReassembler::new();
    r.feed(&make_part("A", 1, 2, 1, "first"));
    r.feed(&make_part("A", 1, 2, 1, "duplicate")); // ignored
    let done = r.feed(&make_part("A", 1, 2, 2, " second"));
    assert!(done.is_some());
    assert_eq!(done.unwrap().content, "first second");
}

#[test]
fn different_senders_separate_groups() {
    let mut r = ConcatReassembler::new();
    r.feed(&make_part("Alice", 1, 2, 1, "Alice "));
    r.feed(&make_part("Bob", 1, 2, 1, "Bob "));
    let alice_done = r.feed(&make_part("Alice", 1, 2, 2, "here"));
    let bob_done = r.feed(&make_part("Bob", 1, 2, 2, "here too"));
    assert!(alice_done.is_some());
    assert!(bob_done.is_some());
    assert_eq!(alice_done.unwrap().content, "Alice here");
    assert_eq!(bob_done.unwrap().content, "Bob here too");
}

#[test]
fn non_concat_pdu_returns_none() {
    let mut r = ConcatReassembler::new();
    let pdu = SmsPdu {
        sender: "A".to_string(),
        content: "single".to_string(),
        timestamp: "".to_string(),
        is_concatenated: false,
        ..Default::default()
    };
    assert!(r.feed(&pdu).is_none());
}

#[test]
fn malformed_total_zero_discarded() {
    let mut r = ConcatReassembler::new();
    // concat_total=0 is invalid — must not create a group
    let result = r.feed(&make_part("A", 1, 0, 1, "body"));
    assert!(result.is_none());
    assert_eq!(r.group_count(), 0, "malformed PDU must not consume a slot");
}

#[test]
fn malformed_part_zero_discarded() {
    let mut r = ConcatReassembler::new();
    let result = r.feed(&make_part("A", 1, 3, 0, "body"));
    assert!(result.is_none());
    assert_eq!(r.group_count(), 0);
}

#[test]
fn malformed_part_exceeds_total_discarded() {
    let mut r = ConcatReassembler::new();
    // part=5 but total=3 — invalid
    let result = r.feed(&make_part("A", 1, 3, 5, "body"));
    assert!(result.is_none());
    assert_eq!(r.group_count(), 0);
}

#[test]
fn max_groups_evicts_oldest() {
    let mut r = ConcatReassembler::new();
    // Fill up 8 groups (max capacity)
    for i in 0..8u16 {
        r.feed(&make_part("sender", i, 2, 1, "part1"));
    }
    assert_eq!(r.group_count(), 8);
    // Adding a 9th group should evict the oldest
    r.feed(&make_part("sender", 100, 2, 1, "new"));
    assert_eq!(r.group_count(), 8);
}

#[test]
fn completed_message_returns_all_stored_slots_across_banks() {
    let mut r = ConcatReassembler::new();
    assert!(matches!(
        r.feed_with_slot(&make_part("A", 1, 2, 1, "Hello "), Some(slot("SM", 3))),
        FeedOutcome::Incomplete
    ));
    let done = r.feed_with_slot(&make_part("A", 1, 2, 2, "World"), Some(slot("ME", 4)));
    let FeedOutcome::Complete(done) = done else {
        panic!("second part should complete the message");
    };
    assert_eq!(done.content, "Hello World");
    assert_eq!(done.slots, vec![slot("SM", 3), slot("ME", 4)]);
}

#[test]
fn duplicate_parts_track_each_distinct_slot_once() {
    let mut r = ConcatReassembler::new();
    let first = make_part("A", 1, 2, 1, "first");
    assert!(matches!(
        r.feed_with_slot(&first, Some(slot("ME", 1))),
        FeedOutcome::Incomplete
    ));
    assert!(matches!(
        r.feed_with_slot(&first, Some(slot("ME", 1))),
        FeedOutcome::Incomplete
    ));
    assert!(matches!(
        r.feed_with_slot(&first, Some(slot("ME", 2))),
        FeedOutcome::Incomplete
    ));
    let done = r.feed_with_slot(&make_part("A", 1, 2, 2, " second"), None);
    let FeedOutcome::Complete(done) = done else {
        panic!("second part should complete the message");
    };
    assert_eq!(done.content, "first second");
    assert_eq!(done.slots, vec![slot("ME", 1), slot("ME", 2)]);
}

#[test]
fn conflicting_duplicate_part_does_not_consume_its_slot() {
    let mut r = ConcatReassembler::new();
    assert!(matches!(
        r.feed_with_slot(&make_part("A", 1, 2, 1, "first"), Some(slot("ME", 1))),
        FeedOutcome::Incomplete
    ));
    assert!(matches!(
        r.feed_with_slot(&make_part("A", 1, 2, 1, "other"), Some(slot("ME", 2))),
        FeedOutcome::Invalid
    ));
    let done = r.feed_with_slot(&make_part("A", 1, 2, 2, " second"), None);
    let FeedOutcome::Complete(done) = done else {
        panic!("second part should complete the original message");
    };
    assert_eq!(done.content, "first second");
    assert_eq!(done.slots, vec![slot("ME", 1)]);
}

#[test]
fn invalid_headers_and_slot_overflow_do_not_complete_a_group() {
    let mut r = ConcatReassembler::new();
    assert!(matches!(
        r.feed_with_slot(&make_part("A", 1, 0, 1, "bad"), Some(slot("ME", 1))),
        FeedOutcome::Invalid
    ));
    assert_eq!(r.group_count(), 0);

    let first = make_part("A", 1, 2, 1, "first");
    for index in 0..64 {
        assert!(matches!(
            r.feed_with_slot(&first, Some(slot("ME", index))),
            FeedOutcome::Incomplete
        ));
    }
    assert!(matches!(
        r.feed_with_slot(&first, Some(slot("ME", 64))),
        FeedOutcome::Invalid
    ));
    let done = r.feed_with_slot(&make_part("A", 1, 2, 2, " second"), None);
    let FeedOutcome::Complete(done) = done else {
        panic!("overflow should leave existing group intact");
    };
    assert_eq!(done.slots.len(), 64);
}
