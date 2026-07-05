//! Tests for Telegram Bot API JSON deserialization (types.rs).

use smsgate::im::telegram::types::{
    json_escape, ApiResult, SendMessageResult, TelegramFile, Update,
};
use smsgate::im::telegram::update_to_inbound_message;

// ---------------------------------------------------------------------------
// json_escape — used by send_message to build valid JSON bodies
// ---------------------------------------------------------------------------

#[test]
fn json_escape_plain_text() {
    assert_eq!(json_escape("hello"), "hello");
}

#[test]
fn json_escape_newline() {
    // SMS forward messages contain literal \n — must become \\n in JSON
    assert_eq!(json_escape("line1\nline2"), "line1\\nline2");
}

#[test]
fn json_escape_carriage_return() {
    assert_eq!(json_escape("a\rb"), "a\\rb");
}

#[test]
fn json_escape_tab() {
    assert_eq!(json_escape("a\tb"), "a\\tb");
}

#[test]
fn json_escape_other_ascii_control_chars() {
    assert_eq!(json_escape("a\0\u{08}\u{0c}b"), "a\\u0000\\b\\fb");
}

#[test]
fn json_escape_backslash() {
    assert_eq!(json_escape("a\\b"), "a\\\\b");
}

#[test]
fn json_escape_double_quote() {
    assert_eq!(json_escape("say \"hi\""), "say \\\"hi\\\"");
}

#[test]
fn json_escape_sms_forward_format() {
    // Matches the actual format used in forward_sms:
    // format!("📱 <code>{}</code>\n🕐 {}\n\n{}", sender, ts, body)
    let text = "📱 <code>+8613812345678</code>\n🕐 2024-01-01 12:00\n\nHello world";
    let escaped = json_escape(text);
    // Verify the result can be embedded in a JSON string without breaking parsing
    let json = format!(r#"{{"text":"{}"}}"#, escaped);
    let v: serde_json::Value = serde_json::from_str(&json).expect("must be valid JSON");
    assert_eq!(v["text"].as_str().unwrap(), text);
}

#[test]
fn json_escape_all_special_chars_combined() {
    let text = "a\nb\\c\"d\re\tf";
    let escaped = json_escape(text);
    let json = format!(r#"{{"t":"{}"}}"#, escaped);
    let v: serde_json::Value = serde_json::from_str(&json).expect("must be valid JSON");
    assert_eq!(v["t"].as_str().unwrap(), text);
}

// ---------------------------------------------------------------------------
// ApiResult<bool> — setMyCommands / deleteMyCommands responses
// ---------------------------------------------------------------------------

#[test]
fn api_result_ok_true() {
    let json = r#"{"ok":true,"result":true}"#;
    let r: ApiResult<bool> = serde_json::from_str(json).unwrap();
    assert!(r.ok);
    assert_eq!(r.result, Some(true));
    assert!(r.description.is_none());
}

#[test]
fn api_result_ok_false_with_description() {
    let json = r#"{"ok":false,"description":"Unauthorized"}"#;
    let r: ApiResult<bool> = serde_json::from_str(json).unwrap();
    assert!(!r.ok);
    assert!(r.result.is_none());
    assert_eq!(r.description.as_deref(), Some("Unauthorized"));
}

// ---------------------------------------------------------------------------
// ApiResult<SendMessageResult> — sendMessage response
// ---------------------------------------------------------------------------

#[test]
fn send_message_result_extracts_message_id() {
    let json = r#"{"ok":true,"result":{"message_id":42,"chat":{"id":123}}}"#;
    let r: ApiResult<SendMessageResult> = serde_json::from_str(json).unwrap();
    assert!(r.ok);
    assert_eq!(r.result.unwrap().message_id, 42);
}

#[test]
fn send_message_result_api_error() {
    let json = r#"{"ok":false,"description":"Bad Request: chat not found"}"#;
    let r: ApiResult<SendMessageResult> = serde_json::from_str(json).unwrap();
    assert!(!r.ok);
    assert!(r.result.is_none());
    assert!(r.description.unwrap().contains("chat not found"));
}

// ---------------------------------------------------------------------------
// ApiResult<Vec<Update>> — getUpdates response
// ---------------------------------------------------------------------------

#[test]
fn get_updates_empty_result() {
    let json = r#"{"ok":true,"result":[]}"#;
    let r: ApiResult<Vec<Update>> = serde_json::from_str(json).unwrap();
    assert!(r.ok);
    assert_eq!(r.result.unwrap().len(), 0);
}

#[test]
fn get_updates_single_message() {
    let json = r#"{
        "ok": true,
        "result": [{
            "update_id": 100,
            "message": {
                "message_id": 5,
                "text": "Hello",
                "chat": {"id": 987654321}
            }
        }]
    }"#;
    let r: ApiResult<Vec<Update>> = serde_json::from_str(json).unwrap();
    assert!(r.ok);
    let updates = r.result.unwrap();
    assert_eq!(updates.len(), 1);
    let u = &updates[0];
    assert_eq!(u.update_id, 100);
    let msg = u.message.as_ref().unwrap();
    assert_eq!(msg.message_id, 5);
    assert_eq!(msg.text.as_deref(), Some("Hello"));
    assert!(msg.reply_to_message.is_none());
}

#[test]
fn get_updates_with_reply_to() {
    let json = r#"{
        "ok": true,
        "result": [{
            "update_id": 200,
            "message": {
                "message_id": 10,
                "text": "/send +1 hi",
                "chat": {"id": 111},
                "reply_to_message": {"message_id": 9}
            }
        }]
    }"#;
    let r: ApiResult<Vec<Update>> = serde_json::from_str(json).unwrap();
    let updates = r.result.unwrap();
    let msg = updates[0].message.as_ref().unwrap();
    assert_eq!(msg.reply_to_message.as_ref().unwrap().message_id, 9);
}

#[test]
fn get_updates_with_document() {
    let json = r#"{
        "ok": true,
        "result": [{
            "update_id": 250,
            "message": {
                "message_id": 11,
                "caption": "/ota sha256:abc",
                "chat": {"id": 111},
                "document": {
                    "file_id": "BQACAgUAAxkBAAIB",
                    "file_unique_id": "AgADabc",
                    "file_name": "smsgate-ota.bin",
                    "mime_type": "application/octet-stream",
                    "file_size": 1312656
                }
            }
        }]
    }"#;
    let r: ApiResult<Vec<Update>> = serde_json::from_str(json).unwrap();
    let updates = r.result.unwrap();
    let msg = updates[0].message.as_ref().unwrap();
    let doc = msg.document.as_ref().unwrap();
    assert_eq!(msg.caption.as_deref(), Some("/ota sha256:abc"));
    assert_eq!(doc.file_id, "BQACAgUAAxkBAAIB");
    assert_eq!(doc.file_unique_id, "AgADabc");
    assert_eq!(doc.file_name.as_deref(), Some("smsgate-ota.bin"));
    assert_eq!(doc.mime_type.as_deref(), Some("application/octet-stream"));
    assert_eq!(doc.file_size, Some(1_312_656));
}

#[test]
fn document_update_maps_to_inbound_message() {
    let json = r#"{
        "update_id": 251,
        "message": {
            "message_id": 12,
            "caption": "/ota",
            "chat": {"id": 111},
            "document": {
                "file_id": "file-123",
                "file_unique_id": "unique-123",
                "file_name": "smsgate-ota.bin",
                "file_size": 1312656
            }
        }
    }"#;
    let update: Update = serde_json::from_str(json).unwrap();

    let inbound = update_to_inbound_message(update, 111).unwrap();

    assert_eq!(inbound.cursor, 252);
    assert_eq!(inbound.text, "/ota");
    let document = inbound.document.as_ref().unwrap();
    assert_eq!(document.file_id, "file-123");
    assert_eq!(document.file_name.as_deref(), Some("smsgate-ota.bin"));
    assert_eq!(document.file_size, Some(1_312_656));
}

#[test]
fn callback_query_maps_to_inbound_message() {
    let json = r#"{
        "update_id": 260,
        "callback_query": {
            "id": "callback-1",
            "data": "log:16",
            "message": {
                "message_id": 44,
                "chat": {"id": 111}
            }
        }
    }"#;
    let update: Update = serde_json::from_str(json).unwrap();

    let inbound = update_to_inbound_message(update, 111).unwrap();

    assert_eq!(inbound.cursor, 261);
    assert_eq!(inbound.text, "log:16");
    let callback = inbound.callback.as_ref().unwrap();
    assert_eq!(callback.id, "callback-1");
    assert_eq!(callback.data, "log:16");
    assert_eq!(callback.message_id, 44);
}

#[test]
fn get_updates_message_without_text() {
    // Non-text messages (stickers, photos, etc.) arrive with no "text" field
    let json = r#"{
        "ok": true,
        "result": [{
            "update_id": 300,
            "message": {
                "message_id": 20,
                "chat": {"id": 111}
            }
        }]
    }"#;
    let r: ApiResult<Vec<Update>> = serde_json::from_str(json).unwrap();
    let updates = r.result.unwrap();
    let msg = updates[0].message.as_ref().unwrap();
    assert!(msg.text.is_none());
}

#[test]
fn get_updates_no_message_field() {
    // Non-message updates (channel_post, etc.) may omit the message field
    let json = r#"{
        "ok": true,
        "result": [{"update_id": 400}]
    }"#;
    let r: ApiResult<Vec<Update>> = serde_json::from_str(json).unwrap();
    let updates = r.result.unwrap();
    assert!(updates[0].message.is_none());
}

#[test]
fn get_updates_multiple_messages() {
    let json = r#"{
        "ok": true,
        "result": [
            {"update_id": 1, "message": {"message_id": 1, "text": "a", "chat": {"id": 1}}},
            {"update_id": 2, "message": {"message_id": 2, "text": "b", "chat": {"id": 1}}}
        ]
    }"#;
    let r: ApiResult<Vec<Update>> = serde_json::from_str(json).unwrap();
    let updates = r.result.unwrap();
    assert_eq!(updates.len(), 2);
    assert_eq!(updates[0].update_id, 1);
    assert_eq!(updates[1].update_id, 2);
}

#[test]
fn get_updates_api_error() {
    let json = r#"{"ok":false,"description":"Too Many Requests: retry after 30"}"#;
    let r: ApiResult<Vec<Update>> = serde_json::from_str(json).unwrap();
    assert!(!r.ok);
    assert!(r.result.is_none());
    assert!(r.description.unwrap().contains("Too Many Requests"));
}

#[test]
fn api_result_deserializes_retry_after_parameter() {
    let json = r#"{"ok":false,"description":"Too Many Requests: retry after 5","parameters":{"retry_after":5}}"#;
    let r: ApiResult<Vec<Update>> = serde_json::from_str(json).unwrap();

    assert_eq!(r.parameters.unwrap().retry_after, Some(5));
}

#[test]
fn get_file_result_extracts_download_path() {
    let json = r#"{
        "ok": true,
        "result": {
            "file_id": "file-123",
            "file_unique_id": "unique-123",
            "file_size": 1312656,
            "file_path": "documents/file_42.bin"
        }
    }"#;

    let r: ApiResult<TelegramFile> = serde_json::from_str(json).unwrap();

    let file = r.result.unwrap();
    assert_eq!(file.file_id, "file-123");
    assert_eq!(file.file_unique_id, "unique-123");
    assert_eq!(file.file_size, Some(1_312_656));
    assert_eq!(file.file_path.as_deref(), Some("documents/file_42.bin"));
}
