//! /send <number> <text>
//!
//! Returns a sentinel line for poller.rs to parse + a user-visible confirmation.

use crate::commands::{
    encoded_sentinel_body_len, push_encoded_sentinel_body, Command, CommandContext, SEND_SENTINEL,
};
use crate::sms::{codec::count_sms_parts, MAX_SMS_PARTS};

pub struct SendCommand;

impl Command for SendCommand {
    fn name(&self) -> &'static str {
        "send"
    }
    fn description(&self) -> &'static str {
        crate::i18n::desc_send()
    }

    fn handle(&self, args: &str, _ctx: &CommandContext) -> String {
        let args = args.trim();
        let Some((phone_raw, body)) = args.split_once(|c: char| c.is_whitespace()) else {
            return crate::i18n::send_usage().to_string();
        };
        let phone = crate::sms::codec::normalize_phone(phone_raw);
        if phone.is_empty() {
            return crate::i18n::send_invalid_number().to_string();
        }
        let body = body.trim();
        if body.is_empty() {
            return crate::i18n::send_empty_body().to_string();
        }
        let parts = count_sms_parts(body, MAX_SMS_PARTS);
        if parts == 0 {
            return crate::i18n::send_too_long().to_string();
        }
        let (preview, truncated) = crate::text::char_prefix(body, 50);
        let encoded_len = encoded_sentinel_body_len(body);
        let queued_len = crate::i18n::send_queued_len(&phone, preview, truncated, parts);
        let mut out = String::with_capacity(
            SEND_SENTINEL.len() + phone.len() + 1 + encoded_len + 1 + queued_len,
        );
        out.push_str(SEND_SENTINEL);
        out.push_str(&phone);
        out.push('|');
        push_encoded_sentinel_body(&mut out, body);
        out.push('\n');
        crate::i18n::push_send_queued(&mut out, &phone, preview, truncated, parts);
        out
    }
}
