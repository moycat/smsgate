//! Heap allocation guardrails for rendering `/log` pages.

#[path = "support/alloc_counter.rs"]
mod alloc_counter;

use smsgate::commands::{builtin::log_cmd::render_log_page, CommandContext};
use smsgate::log_ring::{LogEntry, LogRing};
use smsgate::modem::ModemStatus;
use smsgate::persist::mem::MemStore;
use smsgate::sms::sender::SmsSender;

fn ctx<'a>(
    store: &'a MemStore,
    status: &'a ModemStatus,
    log: &'a LogRing,
    queue: &'a SmsSender,
) -> CommandContext<'a> {
    CommandContext {
        store,
        modem_status: status,
        log_ring: log,
        send_queue: queue,
        uptime_ms: 0,
        free_heap_bytes: 0,
        min_free_heap_bytes: 0,
        wifi_info: "",
    }
}

#[test]
fn log_page_render_allocations_are_bounded_by_page_size() {
    let store = MemStore::new();
    let status = ModemStatus::default();
    let mut log = LogRing::new();
    let queue = SmsSender::new();
    for i in 0..20 {
        log.push(LogEntry::sms(
            format!("+1555<&>{i}"),
            format!("body <tag> & value > {i}"),
            format!("ts<&>{i}"),
            true,
        ));
    }
    let ctx = ctx(&store, &status, &log, &queue);

    let (page, allocations) = alloc_counter::count_allocations(|| render_log_page(&ctx, 0));

    assert!(page.text.contains("&lt;tag&gt; &amp; value &gt; 19"));
    assert!(
        allocations <= 170,
        "log page render allocated {allocations} times; expected allocation count to scale with page size"
    );
}
