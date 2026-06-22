use smsgate::im::{InboundDocument, InboundMessage};
use smsgate::ota::{
    format_running_slot_summary, format_starting_message, is_ota_caption,
    latest_ota_document_cursor, RunningSlotSummary,
};

#[test]
fn ota_caption_accepts_plain_command() {
    assert!(is_ota_caption("/ota"));
}

#[test]
fn ota_caption_accepts_command_with_bot_suffix_and_args() {
    assert!(is_ota_caption("/ota@smsgate_bot sha256:abc"));
}

#[test]
fn ota_caption_rejects_other_text() {
    assert!(!is_ota_caption(""));
    assert!(!is_ota_caption("ota"));
    assert!(!is_ota_caption("/status"));
    assert!(!is_ota_caption("/ota2"));
}

#[test]
fn latest_ota_document_cursor_selects_newest_ota_document() {
    let messages = vec![
        inbound(10, "/ota", Some(document("old.bin"))),
        inbound(11, "/status", None),
        inbound(12, "/ota", Some(document("new.bin"))),
    ];

    assert_eq!(latest_ota_document_cursor(&messages), Some(12));
}

#[test]
fn latest_ota_document_cursor_ignores_non_ota_documents() {
    let messages = vec![
        inbound(10, "/log", Some(document("notes.txt"))),
        inbound(11, "/status", None),
    ];

    assert_eq!(latest_ota_document_cursor(&messages), None);
}

#[test]
fn running_slot_summary_includes_partition_and_firmware_version() {
    let summary = format_running_slot_summary(&RunningSlotSummary {
        slot_label: "ota_0",
        slot_state: "Valid",
        partition_label: "ota_0",
        partition_address: 0x20000,
        partition_size: 0x1e0000,
        firmware_version: "17c4ac8-dirty",
        firmware_released: "unknown",
        build_commit: "17c4ac8",
    });

    assert!(summary.contains("slot=ota_0"));
    assert!(summary.contains("state=Valid"));
    assert!(summary.contains("partition=ota_0"));
    assert!(summary.contains("offset=0x20000"));
    assert!(summary.contains("size=1966080"));
    assert!(summary.contains("version=17c4ac8-dirty"));
    assert!(summary.contains("build=17c4ac8"));
}

#[test]
fn starting_message_includes_firmware_version_and_build() {
    let message = format_starting_message("17c4ac8-dirty", "17c4ac8");

    assert!(message.contains("smsgate starting"));
    assert!(message.contains("version=17c4ac8-dirty"));
    assert!(message.contains("build=17c4ac8"));
}

fn inbound(cursor: i64, text: &str, document: Option<InboundDocument>) -> InboundMessage {
    InboundMessage {
        cursor,
        text: text.to_string(),
        reply_to: None,
        document,
        callback: None,
    }
}

fn document(file_name: &str) -> InboundDocument {
    InboundDocument {
        file_id: format!("file-id-{file_name}"),
        file_unique_id: format!("unique-id-{file_name}"),
        file_name: Some(file_name.to_string()),
        mime_type: Some("application/octet-stream".to_string()),
        file_size: Some(1024),
        caption: Some("/ota".to_string()),
    }
}
