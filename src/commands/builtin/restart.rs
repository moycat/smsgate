use crate::commands::{Command, CommandContext, RESTART_SENTINEL};

pub struct RestartCommand;

impl Command for RestartCommand {
    fn name(&self) -> &'static str {
        "restart"
    }
    fn description(&self) -> &'static str {
        crate::i18n::desc_restart()
    }

    fn handle(&self, _args: &str, _ctx: &CommandContext) -> String {
        format!("{}  \n{}", RESTART_SENTINEL, crate::i18n::restart_ok())
    }
}
