use crate::commands::{Command, CommandContext};

pub struct HelpCommand {
    pub help_text: String,
}

impl Command for HelpCommand {
    fn name(&self) -> &'static str {
        "help"
    }
    fn description(&self) -> &'static str {
        crate::i18n::desc_help()
    }
    fn handle(&self, _args: &str, _ctx: &CommandContext) -> String {
        self.help_text.clone()
    }
}
