//! /block and /unblock commands.

use crate::commands::{Command, CommandContext, BLOCK_SENTINEL, UNBLOCK_SENTINEL};

pub struct BlockCommand;
pub struct UnblockCommand;

impl Command for BlockCommand {
    fn name(&self) -> &'static str {
        "block"
    }
    fn description(&self) -> &'static str {
        crate::i18n::desc_block()
    }

    fn handle(&self, args: &str, _ctx: &CommandContext) -> String {
        let phone = crate::sms::codec::normalize_phone(args.trim());
        if phone.is_empty() {
            return crate::i18n::block_usage().to_string();
        }
        format!(
            "{}{}\n{}",
            BLOCK_SENTINEL,
            phone,
            crate::i18n::block_ok(&phone)
        )
    }
}

impl Command for UnblockCommand {
    fn name(&self) -> &'static str {
        "unblock"
    }
    fn description(&self) -> &'static str {
        crate::i18n::desc_unblock()
    }

    fn handle(&self, args: &str, ctx: &CommandContext) -> String {
        let phone = crate::sms::codec::normalize_phone(args.trim());
        if phone.is_empty() {
            return crate::i18n::unblock_usage().to_string();
        }
        if !crate::bridge::forwarder::is_blocked(&phone, ctx.store) {
            return crate::i18n::unblock_not_found(&phone);
        }
        format!(
            "{}{}\n{}",
            UNBLOCK_SENTINEL,
            phone,
            crate::i18n::unblock_ok(&phone)
        )
    }
}
