use smsgate::im::{InboundDocument, InboundMessage};
use smsgate::ota::{
    format_running_slot_summary, format_starting_message, is_ota_caption,
    latest_ota_request_cursor, parse_ota_request, parse_ota_text, OtaSource, RunningSlotSummary,
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
fn latest_ota_request_cursor_selects_newest_ota_document() {
    let messages = vec![
        inbound(10, "/ota", Some(document("old.bin"))),
        inbound(11, "/status", None),
        inbound(12, "/ota", Some(document("new.bin"))),
    ];

    assert_eq!(latest_ota_request_cursor(&messages), Some(12));
}

#[test]
fn latest_ota_request_cursor_ignores_non_ota_documents() {
    let messages = vec![
        inbound(10, "/log", Some(document("notes.txt"))),
        inbound(11, "/status", None),
    ];

    assert_eq!(latest_ota_request_cursor(&messages), None);
}

#[test]
fn ota_text_accepts_http_https_and_bot_suffix() {
    for text in [
        "/ota http://example.com/firmware.bin",
        "/ota@smsgate_bot https://example.com/firmware.bin?signature=secret#fragment",
    ] {
        let url = parse_ota_text(text).unwrap().unwrap();
        assert!(url.as_str().starts_with("http"));
        assert!(!url.as_str().contains('#'));
    }
    assert!(parse_ota_text("/status").is_none());
    assert!(parse_ota_text("https://example.com/firmware.bin").is_none());
}

#[test]
fn ota_text_rejects_missing_url_extra_args_and_invalid_urls() {
    for text in [
        "/ota",
        "/ota https://example.com/fw.bin another-argument",
        "/ota ftp://example.com/fw.bin",
        "/ota https:///fw.bin",
        "/ota https://user:password@example.com/fw.bin",
        "/ota https://example.com/\0fw.bin",
    ] {
        assert!(parse_ota_text(text).unwrap().is_err(), "{text:?}");
    }
}

#[test]
fn ota_request_keeps_document_compatibility_and_hides_signed_url() {
    let file = inbound(10, "/ota", Some(document("firmware.bin")));
    let source = parse_ota_request(&file).unwrap().unwrap();
    assert!(matches!(source, OtaSource::Document(_)));
    assert_eq!(source.display_name(), "firmware.bin");
    assert_eq!(source.file_size(), Some(1024));

    let message = inbound(11, "/ota https://example.com/fw.bin?token=secret", None);
    let source = parse_ota_request(&message).unwrap().unwrap();
    assert!(matches!(source, OtaSource::Url(_)));
    assert!(!source.display_name().contains("secret"));
    assert_eq!(source.file_size(), None);
}

#[test]
fn latest_ota_request_handles_mixed_sources_and_invalid_newer_request() {
    let messages = vec![
        inbound(10, "/ota", Some(document("old.bin"))),
        inbound(12, "/ota http://example.com/new.bin", None),
        inbound(11, "/ota", Some(document("middle.bin"))),
        inbound(13, "/ota ftp://example.com/bad.bin", None),
    ];
    assert_eq!(latest_ota_request_cursor(&messages), Some(12));

    let messages = vec![
        inbound(10, "/ota http://example.com/old.bin", None),
        inbound(12, "/ota", Some(document("new.bin"))),
    ];
    assert_eq!(latest_ota_request_cursor(&messages), Some(12));
}

#[test]
fn ota_callbacks_are_not_download_requests() {
    let mut message = inbound(10, "/ota https://example.com/firmware.bin", None);
    message.callback = Some(smsgate::im::InboundCallback {
        id: "callback".into(),
        data: message.text.clone(),
        message_id: 100,
    });
    assert!(parse_ota_request(&message).is_none());
    assert_eq!(latest_ota_request_cursor(&[message]), None);
}

#[test]
fn running_slot_summary_includes_partition_and_build() {
    let summary = format_running_slot_summary(&RunningSlotSummary {
        slot_label: "ota_0",
        slot_state: "Valid",
        partition_label: "ota_0",
        partition_address: 0x20000,
        partition_size: 0x1e0000,
        firmware_released: "unknown",
        build_commit: "17c4ac8",
    });

    assert!(summary.contains("slot=ota_0"));
    assert!(summary.contains("state=Valid"));
    assert!(summary.contains("partition=ota_0"));
    assert!(summary.contains("offset=0x20000"));
    assert!(summary.contains("size=1966080"));
    assert!(summary.contains("build=17c4ac8"));
    assert!(!summary.contains("version="));
}

#[test]
fn starting_message_includes_build_only() {
    let message = format_starting_message("17c4ac8");

    assert!(message.contains("smsgate starting"));
    assert!(message.contains("build=17c4ac8"));
    assert!(!message.contains("version="));
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
