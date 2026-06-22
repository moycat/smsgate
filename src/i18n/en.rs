//! English UI strings.

// ── System / lifecycle ────────────────────────────────────────────────────────

pub fn started() -> &'static str {
    "✅ smsgate started"
}
pub fn nvs_fail() -> &'static str {
    "⚠️ NVS init failed — running without persistence. \
     Block list and cursor will reset on reboot."
}
pub fn rebooting() -> &'static str {
    "♻️ Rebooting now…"
}
pub fn low_heap(free_bytes: u32) -> String {
    format!("⚠️ Low heap: {} bytes", free_bytes)
}
pub fn sms_sent_ok(phone: &str) -> String {
    format!("✅ SMS sent to {}", phone)
}
pub fn sms_failed(phone: &str) -> String {
    format!("❌ SMS to {} failed (max retries)", phone)
}
pub fn poll_thread_stale(mins: u32) -> String {
    format!("⚠️ Telegram polling thread unresponsive for {} min", mins)
}
pub fn low_signal(csq: u8) -> String {
    format!("⚠️ Weak cellular signal (CSQ {})", csq)
}
pub fn signal_restored(csq: u8) -> String {
    format!("✅ Cellular signal restored (CSQ {})", csq)
}
pub fn operator_changed(old: &str, new: &str) -> String {
    format!("⚠️ Operator changed: {} → {}", old, new)
}
pub fn ota_wifi_required() -> &'static str {
    "OTA requires WiFi. Cellular fallback mode cannot download firmware."
}
pub fn ota_starting(name: &str, size: Option<u64>) -> String {
    match size {
        Some(bytes) => format!("⬇️ OTA starting: {} ({})", name, format_bytes(bytes)),
        None => format!("⬇️ OTA starting: {} (unknown size)", name),
    }
}
pub fn ota_progress(written: usize, total: Option<usize>) -> String {
    match total {
        Some(total) if total > 0 => format!(
            "⬇️ OTA progress: {} / {} ({}%)",
            format_bytes(written as u64),
            format_bytes(total as u64),
            written.saturating_mul(100) / total
        ),
        _ => format!("⬇️ OTA progress: {}", format_bytes(written as u64)),
    }
}
pub fn ota_complete() -> &'static str {
    "✅ OTA flashed. Rebooting into new firmware."
}
pub fn ota_failed(error: &str) -> String {
    format!("❌ OTA failed: {}", error)
}
pub fn ota_ignored_stale(name: &str) -> String {
    format!("Ignored older OTA file: {name}. The newest OTA file in this batch will be used.")
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.2} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{} B", bytes)
    }
}

// ── Forwarding ────────────────────────────────────────────────────────────────

pub fn sms_received(sender: &str, ts: &str, body: &str) -> String {
    format!("📱 SMS from {}\n🕐 {}\n\n{}", sender, ts, body)
}
pub fn incoming_call(display: &str) -> String {
    format!("📞 Incoming call from {}", display)
}

// ── /status ───────────────────────────────────────────────────────────────────

pub fn status_op_unknown() -> &'static str {
    "unknown"
}
pub fn status_reg_ok() -> &'static str {
    "registered"
}
pub fn status_reg_no() -> &'static str {
    "not registered"
}
pub fn status_fwd_on() -> &'static str {
    "enabled"
}
pub fn status_fwd_off() -> &'static str {
    "PAUSED"
}
pub fn status_build(commit: &str) -> String {
    format!("🔖 Build: {}", commit)
}

#[allow(clippy::too_many_arguments)]
pub fn format_status(
    h: u32,
    m: u32,
    s: u32,
    signal: &str,
    operator: &str,
    registered: bool,
    free_heap_kb: u32,
    queue_n: usize,
    blocked_n: usize,
    log_n: usize,
    fwd_on: bool,
    last_sms: Option<(&str, &str)>,
    wifi_info: &str,
) -> String {
    let reg = if registered {
        status_reg_ok()
    } else {
        status_reg_no()
    };
    let fwd = if fwd_on {
        status_fwd_on()
    } else {
        status_fwd_off()
    };
    let wifi_line = if wifi_info.is_empty() {
        String::new()
    } else {
        format!("📶 WiFi: {}\n", wifi_info)
    };
    let heap_line = if free_heap_kb > 0 {
        format!("💾 Heap: {} KB free\n", free_heap_kb)
    } else {
        String::new()
    };
    let last_line = match last_sms {
        Some((sender, ts)) => format!("📩 Last SMS: {} at {}\n", sender, ts),
        None => String::new(),
    };
    format!(
        "📊 smsgate status\n\
         ⏱ Uptime: {:02}h {:02}m {:02}s\n\
         {}📶 Signal: {} — {}\n\
         🌐 Network: {}\n\
         {}📬 Queue: {} pending\n\
         🚫 Blocked: {} number(s)\n\
         📋 Log: {} messages\n\
         🔄 Forwarding: {}\n\
         {}",
        h,
        m,
        s,
        wifi_line,
        signal,
        operator,
        reg,
        heap_line,
        queue_n,
        blocked_n,
        log_n,
        fwd,
        last_line,
    )
}

// ── /send ─────────────────────────────────────────────────────────────────────

pub fn send_usage() -> &'static str {
    "Usage: /send <number> <message text>"
}
pub fn send_invalid_number() -> &'static str {
    "Invalid phone number"
}
pub fn send_empty_body() -> &'static str {
    "Message body is empty"
}
pub fn send_too_long() -> &'static str {
    "Message too long (> 10 SMS parts)"
}

pub fn send_queued(phone: &str, preview: &str, parts: usize) -> String {
    format!("Queued: {} → \"{}…\" ({} part(s))", phone, preview, parts)
}
pub fn send_rate_limited() -> &'static str {
    "Rate limit: at most 5 /send commands per minute."
}

// ── /log ──────────────────────────────────────────────────────────────────────

pub fn log_empty() -> &'static str {
    "No SMS history."
}
pub fn log_header(page_len: usize, total: usize, offset: usize, _page_size: usize) -> String {
    format!("Log page: {page_len} entries, total {total}, offset {offset}.\n")
}
pub fn log_read_failed(error: &str) -> String {
    format!("Log read failed: {error}")
}
pub fn log_button_newer() -> &'static str {
    "Newer"
}
pub fn log_button_older() -> &'static str {
    "Older"
}

// ── /block + /unblock ─────────────────────────────────────────────────────────

pub fn block_usage() -> &'static str {
    "Usage: /block <number>"
}
pub fn block_ok(phone: &str) -> String {
    format!("Blocked: {}", phone)
}
pub fn blocklist_empty() -> &'static str {
    "Block list is empty."
}
pub fn blocklist_header(n: usize) -> String {
    format!("Blocked numbers ({}):\n", n)
}

pub fn unblock_usage() -> &'static str {
    "Usage: /unblock <number>"
}
pub fn unblock_not_found(phone: &str) -> String {
    format!("{} is not in the block list.", phone)
}
pub fn unblock_ok(phone: &str) -> String {
    format!("Unblocked: {}", phone)
}

// ── /pause + /resume ──────────────────────────────────────────────────────────

pub fn pause_ok(mins: u32) -> String {
    format!("Forwarding paused for {} min.", mins)
}
pub fn resume_already_active() -> &'static str {
    "Forwarding is already active."
}
pub fn resume_ok() -> &'static str {
    "Forwarding resumed."
}

// ── /restart ──────────────────────────────────────────────────────────────────

pub fn restart_ok() -> &'static str {
    "Rebooting…"
}

// ── Command descriptions (shown in Telegram autocomplete) ─────────────────────

pub fn desc_help() -> &'static str {
    "List all commands"
}
pub fn desc_status() -> &'static str {
    "Show device health and stats"
}
pub fn desc_send() -> &'static str {
    "Send an SMS: /send <number> <text>"
}
pub fn desc_log() -> &'static str {
    "Show event log page: /log [offset]"
}
pub fn desc_block() -> &'static str {
    "Block SMS from a number"
}
pub fn desc_blocklist() -> &'static str {
    "Show blocked numbers"
}
pub fn desc_unblock() -> &'static str {
    "Unblock SMS from a number"
}
pub fn desc_pause() -> &'static str {
    "Pause SMS forwarding (default 60 min)"
}
pub fn desc_resume() -> &'static str {
    "Resume SMS forwarding immediately"
}
pub fn desc_restart() -> &'static str {
    "Reboot the device"
}
