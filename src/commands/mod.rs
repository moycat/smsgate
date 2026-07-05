//! Bot command abstraction and registry.

pub mod builtin;

use crate::log_ring::LogRing;
use crate::modem::ModemStatus;
use crate::persist::Store;
use crate::sms::sender::SmsSender;

/// Sentinel prefixes embedded in command replies. The poller strips these
/// lines and applies their side effects (enqueue SMS, toggle blocklist, etc.).
/// Defined here so both `builtin/` and `bridge/poller.rs` share one source.
pub const SEND_SENTINEL: &str = "__SEND__:";
pub const BLOCK_SENTINEL: &str = "__BLOCK__:";
pub const UNBLOCK_SENTINEL: &str = "__UNBLOCK__:";
pub const PAUSE_SENTINEL: &str = "__PAUSE__:";
pub const RESUME_SENTINEL: &str = "__RESUME__";
pub const RESTART_SENTINEL: &str = "__RESTART__";
// Keep `pub` (not `pub(crate)`) — integration tests import them.

pub(crate) fn push_encoded_sentinel_body(out: &mut String, body: &str) {
    for ch in body.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            ch => out.push(ch),
        }
    }
}

pub(crate) fn decode_sentinel_body(encoded: &str) -> String {
    let mut out = String::with_capacity(encoded.len());
    let mut chars = encoded.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Read-only context available to a command handler.
pub struct CommandContext<'a> {
    pub store: &'a dyn Store,
    pub modem_status: &'a ModemStatus,
    pub log_ring: &'a LogRing,
    pub send_queue: &'a SmsSender,
    pub uptime_ms: u32,
    /// Free heap in bytes (0 on host/tests; real value on device).
    pub free_heap_bytes: u32,
    /// Minimum free heap in bytes since boot (0 on host/tests; real value on device).
    pub min_free_heap_bytes: u32,
    /// WiFi status string ("" on host/tests).
    pub wifi_info: &'a str,
}

/// A single bot command.
pub trait Command: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    /// Execute the command; `args` is text after the command name.
    fn handle(&self, args: &str, ctx: &CommandContext) -> String;
}

/// Auto-dispatching command registry.
pub struct CommandRegistry {
    commands: Vec<Box<dyn Command>>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        CommandRegistry {
            commands: Vec::new(),
        }
    }

    /// Register a command. Panics if cap is exceeded.
    pub fn register(&mut self, cmd: Box<dyn Command>) {
        assert!(
            self.commands.len() < 10,
            "command cap exceeded (max 10) — remove a command before adding one"
        );
        self.commands.push(cmd);
    }

    /// Dispatch a message text to the matching command. Returns reply or None.
    pub fn dispatch(&self, text: &str, ctx: &CommandContext) -> Option<String> {
        let text = text.trim_start_matches('/');
        let (name, args) = text
            .split_once(|c: char| c.is_whitespace())
            .unwrap_or((text, ""));

        // Strip bot username suffix (e.g. /help@mybot)
        let name = name.split('@').next().unwrap_or(name);

        self.commands
            .iter()
            .find(|c| c.name() == name)
            .map(|c| c.handle(args.trim(), ctx))
    }

    /// Returns (name, description) pairs for registration with IM backend.
    pub fn command_list(&self) -> Vec<(&str, &str)> {
        self.commands
            .iter()
            .map(|c| (c.name(), c.description()))
            .collect()
    }

    /// Generate /help text.
    pub fn help_text(&self) -> String {
        let mut out = String::from("Commands:\n");
        for cmd in &self.commands {
            out.push_str(&format!("/{} — {}\n", cmd.name(), cmd.description()));
        }
        out
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}
