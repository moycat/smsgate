//! 中文 UI 字符串。

// ── 系统 / 生命周期 ─────────────────────────────────────────────────────────

pub fn started() -> &'static str {
    "✅ smsgate 已启动"
}
pub fn nvs_fail() -> &'static str {
    "⚠️ NVS 初始化失败，以无持久化模式运行。\
     黑名单和游标将在重启后重置。"
}
pub fn rebooting() -> &'static str {
    "♻️ 正在重启…"
}
pub fn low_heap(free_bytes: u32) -> String {
    format!("⚠️ 可用内存不足：{} 字节", free_bytes)
}
pub fn sms_sent_ok(phone: &str) -> String {
    format!("✅ 短信已发出：{}", phone)
}
pub fn sms_failed(phone: &str) -> String {
    format!("❌ 发往 {} 的短信发送失败（已达最大重试次数）", phone)
}
pub fn low_signal(csq: u8) -> String {
    format!("⚠️ 蜂窝信号弱（CSQ {}）", csq)
}
pub fn signal_restored(csq: u8) -> String {
    format!("✅ 蜂窝信号已恢复（CSQ {}）", csq)
}
pub fn operator_changed(old: &str, new: &str) -> String {
    format!("⚠️ 运营商变更：{} → {}", old, new)
}
pub fn ota_wifi_required() -> &'static str {
    "OTA 需要 WiFi。蜂窝回退模式不能下载固件。"
}
pub fn ota_starting(name: &str, size: Option<u64>) -> String {
    match size {
        Some(bytes) => format!("⬇️ OTA 开始：{}（{}）", name, format_bytes(bytes)),
        None => format!("⬇️ OTA 开始：{}（大小未知）", name),
    }
}
pub fn ota_progress(written: usize, total: Option<usize>) -> String {
    match total {
        Some(total) if total > 0 => format!(
            "⬇️ OTA 进度：{} / {}（{}%）",
            format_bytes(written as u64),
            format_bytes(total as u64),
            written.saturating_mul(100) / total
        ),
        _ => format!("⬇️ OTA 进度：{}", format_bytes(written as u64)),
    }
}
pub fn ota_complete() -> &'static str {
    "✅ OTA 写入完成，正在重启到新固件。"
}
pub fn ota_failed(error: &str) -> String {
    format!("❌ OTA 失败：{}", error)
}
pub fn ota_ignored_stale(name: &str) -> String {
    format!("已忽略较旧的 OTA 文件：{name}。本批次将使用最新的 OTA 文件。")
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

// ── 转发 ────────────────────────────────────────────────────────────────────

pub fn sms_received(sender: &str, ts: &str, body: &str) -> String {
    format!(
        "📱 <code>{}</code>\n🕐 {}\n\n{}",
        super::html_escape(sender),
        super::html_escape(ts),
        super::html_escape(body)
    )
}
pub fn mms_notification(
    url: &str,
    message_size: Option<u64>,
    expiry: Option<crate::mms::MmsExpiry>,
) -> String {
    let mut lines = vec![
        "📎 彩信通知（未下载内容）".to_string(),
        format!("下载地址：{url}"),
    ];
    if let Some(bytes) = message_size {
        lines.push(format!("大小：{}", format_bytes(bytes)));
    }
    if let Some(expiry) = expiry {
        lines.push(format!("过期：{}", format_mms_expiry(expiry)));
    }
    lines.join("\n")
}
pub fn incoming_call(display: &str) -> String {
    format!("📞 来电：{}", display)
}

fn format_mms_expiry(expiry: crate::mms::MmsExpiry) -> String {
    match expiry {
        crate::mms::MmsExpiry::RelativeSeconds(seconds) => format_duration_after(seconds),
        crate::mms::MmsExpiry::AbsoluteUnixSeconds(seconds) => format!("Unix 时间戳 {seconds}"),
    }
}

fn format_duration_after(seconds: u64) -> String {
    if seconds.is_multiple_of(86_400) {
        format!("收到后 {} 天", seconds / 86_400)
    } else if seconds.is_multiple_of(3_600) {
        format!("收到后 {} 小时", seconds / 3_600)
    } else if seconds.is_multiple_of(60) {
        format!("收到后 {} 分钟", seconds / 60)
    } else {
        format!("收到后 {seconds} 秒")
    }
}

// ── /status ──────────────────────────────────────────────────────────────────

pub fn status_op_unknown() -> &'static str {
    "未知"
}
pub fn status_reg_ok() -> &'static str {
    "已注册"
}
pub fn status_reg_no() -> &'static str {
    "未注册"
}
pub fn status_fwd_on() -> &'static str {
    "已启用"
}
pub fn status_fwd_off() -> &'static str {
    "已暂停"
}
pub fn status_build(commit: &str) -> String {
    format!("🔖 版本：{}", commit)
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
    min_free_heap_kb: u32,
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
        format!("📶 WiFi：{}\n", wifi_info)
    };
    let heap_line = if free_heap_kb > 0 {
        if min_free_heap_kb > 0 {
            format!(
                "💾 内存：{} KB 可用，最低 {} KB\n",
                free_heap_kb, min_free_heap_kb
            )
        } else {
            format!("💾 内存：{} KB 可用\n", free_heap_kb)
        }
    } else {
        String::new()
    };
    let last_line = match last_sms {
        Some((sender, ts)) => format!("📩 最近：{}（{}）\n", sender, ts),
        None => String::new(),
    };
    format!(
        "📊 smsgate 状态\n\
         ⏱ 运行时间：{:02}h {:02}m {:02}s\n\
         {}📶 信号：{} — {}\n\
         🌐 网络：{}\n\
         {}📬 队列：{} 条待发\n\
         🚫 屏蔽：{} 个号码\n\
         📋 日志：{} 条\n\
         🔄 转发：{}\n\
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

// ── /send ────────────────────────────────────────────────────────────────────

pub fn send_usage() -> &'static str {
    "用法：/send <号码> <消息内容>"
}
pub fn send_invalid_number() -> &'static str {
    "无效号码"
}
pub fn send_empty_body() -> &'static str {
    "消息内容为空"
}
pub fn send_too_long() -> &'static str {
    "消息过长（超过 10 条短信）"
}

pub fn send_queued(phone: &str, preview: &str, truncated: bool, parts: usize) -> String {
    let ellipsis = if truncated { "…" } else { "" };
    format!(
        "已入队：{} → \"{}{}\"（{} 条）",
        phone, preview, ellipsis, parts
    )
}
pub fn send_rate_limited() -> &'static str {
    "频率限制：每分钟最多发送 5 条 /send 命令。"
}

// ── /log ─────────────────────────────────────────────────────────────────────

pub fn log_empty() -> &'static str {
    "暂无短信记录。"
}
pub fn log_header(page_len: usize, total: usize, offset: usize, _page_size: usize) -> String {
    format!("日志页：本页 {page_len} 条，总计 {total} 条，offset {offset}。\n")
}
pub fn log_read_failed(error: &str) -> String {
    format!("读取日志失败：{error}")
}
pub fn log_button_newer() -> &'static str {
    "新日志"
}
pub fn log_button_older() -> &'static str {
    "旧日志"
}

// ── /block + /unblock ────────────────────────────────────────────────────────

pub fn block_usage() -> &'static str {
    "用法：/block <号码>"
}
pub fn block_ok(phone: &str) -> String {
    format!("已屏蔽：{}", phone)
}
pub fn blocklist_empty() -> &'static str {
    "屏蔽名单为空。"
}
pub fn blocklist_header(n: usize) -> String {
    format!("屏蔽名单（{} 个）：\n", n)
}

pub fn unblock_usage() -> &'static str {
    "用法：/unblock <号码>"
}
pub fn unblock_not_found(phone: &str) -> String {
    format!("{} 不在屏蔽名单中。", phone)
}
pub fn unblock_ok(phone: &str) -> String {
    format!("已解除屏蔽：{}", phone)
}

// ── /pause + /resume ─────────────────────────────────────────────────────────

pub fn pause_ok(mins: u32) -> String {
    format!("转发已暂停 {} 分钟。", mins)
}
pub fn resume_already_active() -> &'static str {
    "转发已处于启用状态。"
}
pub fn resume_ok() -> &'static str {
    "转发已恢复。"
}

// ── /restart ─────────────────────────────────────────────────────────────────

pub fn restart_ok() -> &'static str {
    "正在重启…"
}

// ── 命令描述（Telegram 自动补全）────────────────────────────────────────────

pub fn desc_help() -> &'static str {
    "显示帮助"
}
pub fn desc_status() -> &'static str {
    "设备状态"
}
pub fn desc_send() -> &'static str {
    "发送短信：/send <号码> <内容>"
}
pub fn desc_log() -> &'static str {
    "查看事件日志分页：/log [offset]"
}
pub fn desc_block() -> &'static str {
    "屏蔽号码"
}
pub fn desc_blocklist() -> &'static str {
    "查看屏蔽名单"
}
pub fn desc_unblock() -> &'static str {
    "解除屏蔽"
}
pub fn desc_pause() -> &'static str {
    "暂停转发（默认 60 分钟）"
}
pub fn desc_resume() -> &'static str {
    "恢复转发"
}
pub fn desc_restart() -> &'static str {
    "重启设备"
}
