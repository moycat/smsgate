use crate::commands::{Command, CommandContext};
use crate::{log_ring::LogKind, modem::CSQ_UNKNOWN};

pub struct StatusCommand;

impl Command for StatusCommand {
    fn name(&self) -> &'static str {
        "status"
    }
    fn description(&self) -> &'static str {
        crate::i18n::desc_status()
    }

    fn handle(&self, _args: &str, ctx: &CommandContext) -> String {
        let uptime_s = ctx.uptime_ms / 1000;
        let h = uptime_s / 3600;
        let m = (uptime_s % 3600) / 60;
        let s = uptime_s % 60;

        let csq = ctx.modem_status.csq;
        let signal = if csq == CSQ_UNKNOWN {
            "N/A".to_string()
        } else {
            format!("{}/31 ({} dBm)", csq, csq_to_dbm(csq))
        };

        let operator = if ctx.modem_status.operator.is_empty() {
            crate::i18n::status_op_unknown().to_string()
        } else {
            ctx.modem_status.operator.clone()
        };

        let queue_n = ctx.send_queue.len();
        let log_n = ctx.log_ring.len();
        let blocked_n = crate::bridge::forwarder::load_blocklist(ctx.store).len();
        let fwd_on =
            crate::persist::load_bool(ctx.store, crate::persist::keys::FWD_ENABLED).unwrap_or(true);
        let free_heap_kb = ctx.free_heap_bytes / 1024;

        let last_sms_entry = ctx.log_ring.latest_of_kind(LogKind::Sms);
        let last_sms = last_sms_entry
            .as_ref()
            .map(|e| (e.sender.as_str(), e.timestamp.as_str()));

        let mut out = crate::i18n::format_status(
            h,
            m,
            s,
            &signal,
            &operator,
            ctx.modem_status.registered,
            free_heap_kb,
            queue_n,
            blocked_n,
            log_n,
            fwd_on,
            last_sms,
            ctx.wifi_info,
        );
        out.push_str(&crate::i18n::status_build(
            crate::config::Config::GIT_COMMIT,
        ));
        out
    }
}

fn csq_to_dbm(csq: u8) -> i32 {
    -113 + 2 * csq as i32
}
