use crate::commands::{Command, CommandContext};

pub struct LogCommand;

impl Command for LogCommand {
    fn name(&self) -> &'static str {
        "log"
    }
    fn description(&self) -> &'static str {
        crate::i18n::desc_log()
    }

    fn handle(&self, args: &str, ctx: &CommandContext) -> String {
        let n: usize = args.trim().parse().unwrap_or(10).min(50);
        let entries = ctx.log_ring.last_n(n);
        if entries.is_empty() {
            return crate::i18n::log_empty().to_string();
        }
        let mut out = crate::i18n::log_header(entries.len());
        for e in entries {
            let status = if e.forwarded { "✅" } else { "🚫" };
            out.push_str(&format!(
                "{} {} — {}: {}\n",
                status, e.timestamp, e.sender, e.body_preview
            ));
        }
        out
    }
}
