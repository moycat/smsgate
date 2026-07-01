use crate::commands::{Command, CommandContext};
use crate::im::{InlineKeyboard, InlineKeyboardButton, MessageFormat};
use crate::log_ring::LOG_PAGE_SIZE;

pub struct LogCommand;

pub struct LogPage {
    pub text: String,
    pub keyboard: Option<InlineKeyboard>,
    pub format: MessageFormat,
}

impl Command for LogCommand {
    fn name(&self) -> &'static str {
        "log"
    }
    fn description(&self) -> &'static str {
        crate::i18n::desc_log()
    }

    fn handle(&self, args: &str, ctx: &CommandContext) -> String {
        render_log_page(ctx, parse_log_offset(args)).text
    }
}

pub fn parse_log_offset(args: &str) -> usize {
    args.trim().parse().unwrap_or(0)
}

pub fn parse_log_callback(data: &str) -> Option<usize> {
    data.strip_prefix("log:")?.parse().ok()
}

pub fn render_log_page(ctx: &CommandContext, offset: usize) -> LogPage {
    let total = ctx.log_ring.len();
    let entries = match ctx.log_ring.page(offset, LOG_PAGE_SIZE) {
        Ok(entries) => entries,
        Err(e) => {
            return LogPage {
                text: crate::i18n::log_read_failed(&e.to_string()),
                keyboard: None,
                format: MessageFormat::Plain,
            }
        }
    };
    let page_len = entries.len();
    let mut out = crate::i18n::log_header(page_len, total, offset, LOG_PAGE_SIZE);
    if entries.is_empty() {
        out.push_str(crate::i18n::log_empty());
    } else {
        for e in &entries {
            out.push_str(&format!(
                "<blockquote><b>{}</b> {} - {}: {}</blockquote>\n",
                html_escape(&e.timestamp),
                e.kind.label(),
                html_escape(&e.sender),
                html_escape(&e.body_preview)
            ));
        }
    }
    LogPage {
        text: out,
        keyboard: log_keyboard(total, offset, page_len),
        format: if page_len == 0 {
            MessageFormat::Plain
        } else {
            MessageFormat::RichHtml
        },
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn log_keyboard(total: usize, offset: usize, page_len: usize) -> Option<InlineKeyboard> {
    let mut buttons = Vec::new();
    if total > 0 && offset > 0 {
        let newer_offset = if offset >= total {
            total.saturating_sub(LOG_PAGE_SIZE)
        } else {
            offset.saturating_sub(LOG_PAGE_SIZE)
        };
        buttons.push(InlineKeyboardButton::new(
            crate::i18n::log_button_newer(),
            format!("log:{newer_offset}"),
        ));
    }

    let older_offset = offset.saturating_add(page_len);
    if page_len > 0 && older_offset < total {
        buttons.push(InlineKeyboardButton::new(
            crate::i18n::log_button_older(),
            format!("log:{older_offset}"),
        ));
    }

    if buttons.is_empty() {
        None
    } else {
        Some(InlineKeyboard::single_row(buttons))
    }
}
