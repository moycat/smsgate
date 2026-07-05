#![cfg(feature = "testing")]

#[path = "support/alloc_counter.rs"]
mod alloc_counter;

use smsgate::sms::{codec::SmsPdu, concat::ConcatReassembler};

fn make_part(sender: &str, ref_num: u16, total: u8, part: u8, content: String) -> SmsPdu {
    SmsPdu {
        sender: sender.to_string(),
        timestamp: "26/04/10,12:00:00+00".to_string(),
        content,
        is_concatenated: true,
        concat_ref: ref_num,
        concat_total: total,
        concat_part: part,
        ..Default::default()
    }
}

#[test]
fn final_concat_part_allocations_are_bounded() {
    let mut reassembler = ConcatReassembler::new();
    assert!(reassembler
        .feed(&make_part("A", 1, 3, 1, "a".repeat(120)))
        .is_none());
    assert!(reassembler
        .feed(&make_part("A", 1, 3, 2, "b".repeat(120)))
        .is_none());
    let final_part = make_part("A", 1, 3, 3, "c".repeat(120));

    let (completed, allocations) =
        alloc_counter::count_allocations(|| reassembler.feed(&final_part));

    assert_eq!(completed.unwrap().content.len(), 360);
    assert!(
        allocations <= 2,
        "final concat part allocated {allocations} times; expected one stored part clone and one assembled body"
    );
}
