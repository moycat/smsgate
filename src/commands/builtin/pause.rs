//! /pause and /resume commands.

use crate::commands::{Command, CommandContext, PAUSE_SENTINEL, RESUME_SENTINEL};
use crate::persist::{keys, load_bool};

pub struct PauseCommand;
pub struct ResumeCommand;

impl Command for PauseCommand {
    fn name(&self) -> &'static str {
        "pause"
    }
    fn description(&self) -> &'static str {
        crate::i18n::desc_pause()
    }

    fn handle(&self, args: &str, _ctx: &CommandContext) -> String {
        let mins: u32 = args.trim().parse().unwrap_or(60);
        format!(
            "{}{}  \n{}",
            PAUSE_SENTINEL,
            mins,
            crate::i18n::pause_ok(mins)
        )
    }
}

impl Command for ResumeCommand {
    fn name(&self) -> &'static str {
        "resume"
    }
    fn description(&self) -> &'static str {
        crate::i18n::desc_resume()
    }

    fn handle(&self, _args: &str, ctx: &CommandContext) -> String {
        if load_bool(ctx.store, keys::FWD_ENABLED).unwrap_or(true) {
            crate::i18n::resume_already_active().to_string()
        } else {
            format!("{}  \n{}", RESUME_SENTINEL, crate::i18n::resume_ok())
        }
    }
}
