//! smsgate — composition root.
//!
//! `anyhow` is intentionally limited to this file: it handles the startup
//! sequence where ergonomic error propagation matters and heap allocation is
//! acceptable. All inner modules use concrete `thiserror`-derived error types.

#[cfg(feature = "esp32")]
use smsgate::{
    boards::{ta7670x::TA7670X, Board},
    bridge::{
        at_handler::{execute_at_command, parse_hidden_at_command},
        call_handler::{CallHandler, CallNotification},
        forwarder::{
            forward_sms, is_blocked, record_forward_failure, record_forward_success,
            sms_forward_text,
        },
        poller::poll_and_dispatch,
        reply_router::ReplyRouter,
        sms_handler::{
            delete_verified_sms_slot, prepare_pdu_hex_with_slot, prepare_stored_sms,
            read_new_sms_pdu, read_stored_sms_batch, scan_stored_sms_indices, PreparedSms,
            SmsPreparation, VerifiedDeletion,
        },
    },
    commands::{builtin::*, CommandRegistry},
    config::Config,
    creds::RuntimeConfig,
    im::{
        telegram::{
            http::TelegramHttpClient,
            poll_error_log_detail, should_log_poll_error, should_recover_after_poll_errors,
            should_restart_after_stale_poll,
            worker::{QueuedCommandResponder, TelegramSendEvent, TelegramSendWorker},
            TelegramMessenger,
        },
        MessageFormat, MessageId, MessageSink, MessageSource, MessengerError, PollBatch,
    },
    log_clock::LogClock,
    log_ring::LogRing,
    log_ring::{LogEvent, LogKind},
    modem::{
        cnmi_store_notifications_enabled,
        urc::{parse_urc, Urc},
        AtResponse, ModemDiagnostics, ModemError, ModemPort,
    },
    persist::{keys, load_bool, nvs::NvsStore, Store},
    sms::concat::{ConcatReassembler, StorageSlot},
    sms::sender::{DrainOutcome, SmsSender},
    timer::elapsed_since,
};

#[cfg(feature = "esp32")]
use std::collections::VecDeque;

#[cfg(feature = "esp32")]
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};

#[cfg(not(feature = "esp32"))]
fn main() {
    panic!("This binary requires the esp32 feature");
}

/// Lock a `Mutex`, recovering from a poisoned state rather than panicking.
#[cfg(feature = "esp32")]
macro_rules! lock {
    ($m:expr) => {
        $m.lock().unwrap_or_else(|e| e.into_inner())
    };
}

#[cfg(feature = "esp32")]
enum TgPollEvent {
    Batch {
        batch: PollBatch,
        ack: Option<SyncSender<()>>,
    },
    Log(LogEvent),
    RecoverTransport {
        reason: String,
    },
}

#[cfg(feature = "esp32")]
struct PollEventSender(SyncSender<TgPollEvent>);

#[cfg(feature = "esp32")]
struct PendingDeletion {
    slot: StorageSlot,
    retry_after: Option<std::time::Instant>,
}

#[cfg(feature = "esp32")]
struct PendingSmsRead {
    mem: String,
    index: u16,
    attempts: u8,
    retry_after: Option<std::time::Instant>,
}

#[cfg(feature = "esp32")]
impl PendingDeletion {
    fn new(slot: StorageSlot) -> Self {
        Self {
            slot,
            retry_after: None,
        }
    }
}

#[cfg(feature = "esp32")]
impl PollEventSender {
    fn send(&self, mut event: TgPollEvent) -> Result<(), ()> {
        loop {
            match self.0.try_send(event) {
                Ok(()) => return Ok(()),
                Err(TrySendError::Disconnected(_)) => return Err(()),
                Err(TrySendError::Full(queued)) => {
                    event = queued;
                    // Backpressure must not trip the poll task watchdog while
                    // the modem owner is completing a slow AT operation.
                    unsafe {
                        // SAFETY: This feeds only the current poll task's watchdog.
                        let _ = esp_idf_sys::esp_task_wdt_reset();
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        }
    }
}

#[cfg(feature = "esp32")]
enum DeferredNotification {
    Sms(PreparedSms),
    Call(CallNotification),
    Plain { text: String, context: &'static str },
}

#[cfg(feature = "esp32")]
struct PendingNotification {
    item: DeferredNotification,
    receipt: Receiver<Result<MessageId, MessengerError>>,
    queued_at: std::time::Instant,
}

#[cfg(feature = "esp32")]
const MAX_READY_NOTIFICATIONS: usize = 8;

#[cfg(feature = "esp32")]
const MAX_PENDING_NOTIFICATIONS: usize = 4;

#[cfg(feature = "esp32")]
const MAX_PENDING_DELETIONS: usize = 32;

#[cfg(feature = "esp32")]
const MAX_TG_POLL_EVENTS_PER_PASS: usize = 8;

#[cfg(feature = "esp32")]
fn main() {
    esp_idf_sys::link_patches();
    for (key, value) in smsgate::diagnostics::firmware_diagnostic_env() {
        std::env::set_var(key, value);
    }
    esp_idf_svc::log::EspLogger::initialize_default();

    let starting_message = smsgate::ota::format_starting_message(Config::GIT_COMMIT);
    log::info!("{}", starting_message);
    log::info!(
        "[boot] {}",
        smsgate::ota::running_slot_summary(Config::GIT_COMMIT)
    );
    let boot_ms = now_ms();

    // ---- Runtime config (NVS, or compiled defaults when this image requests it) ----
    let nvs_partition = esp_idf_svc::nvs::EspDefaultNvsPartition::take().unwrap();
    let loaded_creds = RuntimeConfig::load(&nvs_partition);
    let creds = RuntimeConfig::resolve_compiled_config(loaded_creds, Config::APPLY_COMPILED_CONFIG);
    if Config::APPLY_COMPILED_CONFIG {
        match creds.save(&nvs_partition) {
            Ok(()) => log::info!("[main] compile-time runtime config applied to NVS"),
            Err(e) => {
                log::error!("[main] failed to persist compile-time runtime config to NVS: {e}")
            }
        }
    }
    if !creds.is_provisioned() {
        log::warn!("[main] not provisioned — entering serial setup");
        serial_provision(&nvs_partition);
    }

    // Mount before modem bring-up so a fatal init error survives the reboot.
    let mut log = build_log_ring();
    let mut log_clock = LogClock::new();
    let mut sms_delete_error_logs: Vec<(StorageSlot, u32)> = Vec::new();
    record_event(
        &mut log,
        &log_clock,
        elapsed_since(boot_ms, now_ms()),
        LogEvent::system("boot", &starting_message),
    );

    // ---- Board init ----
    let mut peripherals = esp_idf_hal::peripherals::Peripherals::take().unwrap();
    let board = TA7670X;
    if let Err(e) = board.init(&mut peripherals) {
        log::error!("[main] board init failed: {e}");
        record_event(
            &mut log,
            &log_clock,
            elapsed_since(boot_ms, now_ms()),
            LogEvent::new(
                LogKind::System,
                "board",
                &format!("init failed: {e}"),
                false,
            ),
        );
        esp_idf_hal::reset::restart();
    }
    let modem = match board.build_modem_port(&mut peripherals, &creds) {
        Ok(modem) => modem,
        Err(e) => {
            log::error!("[main] modem init failed after retry: {e}; rebooting");
            record_event(
                &mut log,
                &log_clock,
                elapsed_since(boot_ms, now_ms()),
                LogEvent::new(
                    LogKind::System,
                    "modem",
                    &format!("init failed: {e}"),
                    false,
                ),
            );
            std::thread::sleep(std::time::Duration::from_secs(2));
            esp_idf_hal::reset::restart();
        }
    };

    // ---- NVS store (fall back to MemStore on NVS failure) ----
    let nvs_failed: bool;
    let mut store: Box<dyn smsgate::persist::Store> = match NvsStore::new(nvs_partition) {
        Ok(nvs) => {
            nvs_failed = false;
            Box::new(nvs)
        }
        Err(e) => {
            nvs_failed = true;
            log::error!("[main] NVS init failed: {} — using volatile MemStore", e);
            record_event(
                &mut log,
                &log_clock,
                elapsed_since(boot_ms, now_ms()),
                LogEvent::new(
                    LogKind::System,
                    "NVS",
                    &format!("init failed; volatile fallback: {e}"),
                    false,
                ),
            );
            Box::new(smsgate::persist::mem::MemStore::default())
        }
    };

    sync_log_clock_from_modem(&modem, &mut log_clock, &mut log, boot_ms, true);
    for event in refresh_modem_notifications(&modem) {
        record_event(
            &mut log,
            &log_clock,
            elapsed_since(boot_ms, now_ms()),
            event,
        );
    }

    // ---- WiFi ----
    let sysloop = esp_idf_svc::eventloop::EspSystemEventLoop::take().unwrap();
    // SAFETY: EspWifi borrows the modem peripheral whose lifetime is tied to
    // `peripherals`, which lives for the duration of main(). Transmuting to
    // 'static is sound because the wifi driver is kept alive until main exits.
    let wifi_inner: esp_idf_svc::wifi::EspWifi<'static> = unsafe {
        std::mem::transmute(
            esp_idf_svc::wifi::EspWifi::new(peripherals.modem, sysloop.clone(), None)
                .expect("WiFi init failed"),
        )
    };
    let mut wifi = esp_idf_svc::wifi::BlockingWifi::wrap(wifi_inner, sysloop.clone())
        .expect("WiFi wrap failed");
    let mut wifi_ok = setup_wifi(&mut wifi, &creds.wifi_ssid, &creds.wifi_pass).is_ok();
    if wifi_ok {
        record_event(
            &mut log,
            &log_clock,
            elapsed_since(boot_ms, now_ms()),
            LogEvent::network("wifi", &format!("connected to {}", creds.wifi_ssid), true),
        );
    } else {
        log::warn!("[wifi] failed after retries");
        record_event(
            &mut log,
            &log_clock,
            elapsed_since(boot_ms, now_ms()),
            LogEvent::network("wifi", "failed after retries", false),
        );
        record_event(
            &mut log,
            &log_clock,
            elapsed_since(boot_ms, now_ms()),
            LogEvent::network("wifi", "offline; SMS service continues", false),
        );
    }
    let mut wifi = wifi; // keep WiFi driver alive; also used for reconnect on drop

    // ---- IM (Telegram) ----
    let (tg_send_event_tx, tg_send_event_rx) = std::sync::mpsc::channel::<TelegramSendEvent>();
    let mut messenger =
        TelegramSendWorker::spawn(creds.bot_token.clone(), creds.chat_id, tg_send_event_tx);
    let mut ready_notifications: VecDeque<DeferredNotification> = VecDeque::new();

    // ---- Subsystems ----
    let mut sender = SmsSender::new();
    let mut router = ReplyRouter::new();
    router.load(&*store);
    let mut concat = ConcatReassembler::new();
    let mut call_handler = CallHandler::new();
    let mut modem_status = smsgate::modem::ModemStatus::default();
    macro_rules! drain_tg_send {
        () => {
            if drain_telegram_send_events(
                &tg_send_event_rx,
                &mut log,
                &log_clock,
                elapsed_since(boot_ms, now_ms()),
            ) && wifi_ok
            {
                esp_idf_hal::reset::restart();
            }
        };
    }

    // ---- Command registry ----
    // Two-pass: first pass generates help text, second bakes it into HelpCommand.
    let help_text = build_registry("").help_text();
    let registry = build_registry(&help_text);

    // Register only when a transport is usable. An offline startup must not
    // leave the send worker retrying a cosmetic menu update for five minutes.
    let mut command_registration = None;
    let mut commands_registered = false;
    let mut last_command_registration_attempt: Option<std::time::Instant> = None;

    // Alert if NVS init failed (now that we have a messenger to send the notification)
    if nvs_failed {
        queue_plain_notification(
            &mut ready_notifications,
            smsgate::i18n::nvs_fail(),
            "NVS failure",
            &mut log,
            &log_clock.timestamp(elapsed_since(boot_ms, now_ms())),
        );
    }

    // /pause is a transient, timer-driven state. The resume timer lives in RAM
    // only — a reboot (crash, /restart, power cycle) loses it. Clear the
    // flag here so forwarding is always enabled at startup; a deliberate
    // long-term pause should be re-issued after reboot if still needed.
    let _ = smsgate::persist::save_bool(&mut *store, smsgate::persist::keys::FWD_ENABLED, true);

    // ---- Sweep existing SMS from ME (device flash) on boot ----
    // Only ME is swept: on T-A7670X, AT+CPMS="SM","SM","SM" floods the UART
    // buffer with CMTI notifications for every stored SIM message, which
    // corrupts the subsequent AT+CMGL=4 response for both SM and ME.
    // Normal operation stores all SMS in ME anyway (+CMTI always says "ME").
    let mut pending_sweep_indices = VecDeque::new();
    {
        let (storage_result, stored_result) = {
            let mut md = lock!(modem);
            let storage = md.send_at("+CPMS=\"ME\",\"ME\",\"ME\"");
            log::info!("[main] sweeping ME storage…");
            (storage, scan_stored_sms_indices("ME", &mut *md))
        };
        if let Some(detail) = at_command_failure(&storage_result) {
            record_event(
                &mut log,
                &log_clock,
                elapsed_since(boot_ms, now_ms()),
                LogEvent::new(
                    LogKind::Sms,
                    "ME",
                    &format!("preferred storage selection failed: {detail}"),
                    false,
                ),
            );
        }
        match stored_result {
            Ok(scan) => {
                if scan.malformed_entries > 0 {
                    record_event(
                        &mut log,
                        &log_clock,
                        elapsed_since(boot_ms, now_ms()),
                        LogEvent::new(
                            LogKind::Sms,
                            "ME",
                            &format!(
                                "boot sweep skipped {} malformed entries",
                                scan.malformed_entries
                            ),
                            false,
                        ),
                    );
                }
                log::info!("[main] boot SMS sweep queued {} slots", scan.indices.len());
                pending_sweep_indices.extend(scan.indices);
            }
            Err(e) => {
                log::error!("[main] boot SMS sweep failed: {}", e);
                record_event(
                    &mut log,
                    &log_clock,
                    elapsed_since(boot_ms, now_ms()),
                    LogEvent::new(
                        LogKind::Sms,
                        "ME",
                        &format!("boot sweep failed: {}", e),
                        false,
                    ),
                );
            }
        }
    }

    log::info!("smsgate ready");
    record_event(
        &mut log,
        &log_clock,
        elapsed_since(boot_ms, now_ms()),
        LogEvent::system("ready", "smsgate ready"),
    );
    queue_plain_notification(
        &mut ready_notifications,
        smsgate::i18n::started(),
        "startup",
        &mut log,
        &log_clock.timestamp(elapsed_since(boot_ms, now_ms())),
    );
    match smsgate::ota::confirm_running() {
        Ok(()) => log::info!("[main] OTA running slot marked valid"),
        Err(e) => log::warn!("[main] OTA confirm skipped: {}", e),
    }

    // Subscribe main task to the Task WDT configured by sdkconfig.defaults.
    unsafe {
        // SAFETY: A null handle selects the current FreeRTOS task.
        esp_idf_sys::esp_task_wdt_add(std::ptr::null_mut());
    }

    // ---- Telegram polling thread ----
    // Runs getUpdates (long-poll) independently so the main loop is never blocked
    // waiting for the network. The channel delivers batches of inbound messages.
    let initial_cursor =
        smsgate::persist::load_i64(&*store, smsgate::persist::keys::IM_CURSOR).unwrap_or(0);
    let (tg_poll_tx, tg_rx) = std::sync::mpsc::sync_channel::<TgPollEvent>(8);
    let tg_tx = PollEventSender(tg_poll_tx);
    let tg_token_poll = creds.bot_token.clone();
    let tg_chat_id_poll = creds.chat_id;
    let tg_poll_interval_ms = creds.poll_interval_ms;
    std::thread::Builder::new()
        .name("tg-poll".into())
        .stack_size(16 * 1024)
        .spawn(move || {
            let mut consecutive_poll_errors: u16 = 0;
            let mut poll_messenger = loop {
                match build_telegram_messenger(tg_token_poll.clone(), tg_chat_id_poll) {
                    Ok(m) => break m,
                    Err(e) => {
                        log::error!("[tg-poll] messenger init failed: {}", e);
                        consecutive_poll_errors = consecutive_poll_errors.saturating_add(1);
                        let error = format!("init failed: {}", e);
                        if should_log_poll_error(consecutive_poll_errors) {
                            let detail = poll_error_log_detail(consecutive_poll_errors, &error);
                            if tg_tx
                                .send(TgPollEvent::Log(LogEvent::network(
                                    "telegram", &detail, false,
                                )))
                                .is_err()
                            {
                                return;
                            }
                        }
                        if should_recover_after_poll_errors(consecutive_poll_errors)
                            && tg_tx
                                .send(TgPollEvent::RecoverTransport {
                                    reason: poll_error_log_detail(consecutive_poll_errors, &error),
                                })
                                .is_err()
                        {
                            return;
                        }
                        std::thread::sleep(std::time::Duration::from_secs(5));
                    }
                }
            };
            consecutive_poll_errors = 0;
            let mut cursor = initial_cursor;
            // Subscribe this thread to the same Task WDT as main.
            // If poll() hangs indefinitely the WDT fires and reboots the device.
            unsafe {
                // SAFETY: A null handle selects this poll task only.
                esp_idf_sys::esp_task_wdt_add(std::ptr::null_mut());
            }
            loop {
                unsafe {
                    // SAFETY: The current poll task was registered above.
                    esp_idf_sys::esp_task_wdt_reset();
                }
                let poll_secs = telegram_poll_timeout_secs(tg_poll_interval_ms);
                match poll_messenger.poll(cursor, poll_secs) {
                    Ok(batch) => {
                        consecutive_poll_errors = 0;
                        let next_cursor = batch.next_cursor;
                        if next_cursor > cursor {
                            let (ack_tx, ack_rx) = std::sync::mpsc::sync_channel(1);
                            if tg_tx
                                .send(TgPollEvent::Batch {
                                    batch,
                                    ack: Some(ack_tx),
                                })
                                .is_err()
                            {
                                break;
                            }
                            // A higher getUpdates offset confirms this update
                            // on Telegram's server. Wait until main has saved
                            // the cursor and handled the update first.
                            loop {
                                unsafe {
                                    // SAFETY: The current poll task was registered above.
                                    let _ = esp_idf_sys::esp_task_wdt_reset();
                                }
                                match ack_rx.recv_timeout(std::time::Duration::from_secs(5)) {
                                    Ok(()) => {
                                        cursor = next_cursor;
                                        break;
                                    }
                                    Err(RecvTimeoutError::Timeout) => continue,
                                    Err(RecvTimeoutError::Disconnected) => return,
                                }
                            }
                        } else if tg_tx.send(TgPollEvent::Batch { batch, ack: None }).is_err() {
                            break;
                        }
                    }
                    Err(e) => {
                        if let Some(retry_after) = smsgate::im::telegram::poll_retry_after(&e) {
                            let retry_after_secs = retry_after.as_secs();
                            log::warn!(
                                "[tg-poll] rate limited: {}; retrying after {}s",
                                e,
                                retry_after_secs
                            );
                            consecutive_poll_errors = 0;
                            if tg_tx
                                .send(TgPollEvent::Log(LogEvent::network(
                                    "telegram",
                                    &format!(
                                        "poll rate limited; retrying after {}s",
                                        retry_after_secs
                                    ),
                                    false,
                                )))
                                .is_err()
                            {
                                break;
                            }
                            if tg_tx
                                .send(TgPollEvent::Batch {
                                    batch: PollBatch {
                                        messages: Vec::new(),
                                        next_cursor: cursor,
                                    },
                                    ack: None,
                                })
                                .is_err()
                            {
                                break;
                            }
                            if !sleep_with_poll_activity(retry_after, &tg_tx, cursor) {
                                break;
                            }
                            continue;
                        }
                        log::error!("[tg-poll] error: {}", e);
                        consecutive_poll_errors = consecutive_poll_errors.saturating_add(1);
                        let error = e.to_string();
                        if should_log_poll_error(consecutive_poll_errors) {
                            let detail = poll_error_log_detail(consecutive_poll_errors, &error);
                            if tg_tx
                                .send(TgPollEvent::Log(LogEvent::network(
                                    "telegram", &detail, false,
                                )))
                                .is_err()
                            {
                                break;
                            }
                        }
                        if should_recover_after_poll_errors(consecutive_poll_errors)
                            && tg_tx
                                .send(TgPollEvent::RecoverTransport {
                                    reason: poll_error_log_detail(consecutive_poll_errors, &error),
                                })
                                .is_err()
                        {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_secs(5));
                    }
                }
            }
        })
        .expect("failed to spawn tg-poll thread");

    // ---- Main loop ----
    let mut consecutive_failures: u8 = 0;
    let mut last_status_update = now_ms();
    let mut pause_until: Option<std::time::Instant> = None;
    let mut wifi_info = fmt_network(wifi_ok, None, &creds.wifi_ssid);
    let mut last_tg_activity = std::time::Instant::now();
    let mut persisted_tg_cursor = initial_cursor;
    let mut pending_tg_batch: Option<(PollBatch, SyncSender<()>)> = None;
    let mut cursor_retry_after: Option<std::time::Instant> = None;
    let mut low_signal_alerted = false;
    let mut low_heap_alerted = false;
    let mut last_operator = String::new();
    let mut last_signal_state: Option<(bool, bool)> = None;
    let mut last_registered: Option<bool> = None;
    let mut registration_query_failed = false;
    let mut last_wifi_reconnect_attempt: Option<std::time::Instant> = None;
    const SMS_SWEEP_INTERVAL_MS: u32 = 5 * 60 * 1000;
    const NOTIFICATION_CHECK_INTERVAL_MS: u32 = 10 * 60 * 1000;
    const FAULT_LOG_INTERVAL_MS: u32 = 5 * 60 * 1000;
    const PDN_EVENT_LOG_INTERVAL_MS: u32 = 60 * 60 * 1000;
    const SWEEP_ERROR_LOG_INTERVAL_MS: u32 = 60 * 60 * 1000;
    let mut last_sms_sweep = now_ms();
    let mut last_notification_check = now_ms();
    let mut last_sms_sweep_error: Option<String> = None;
    let mut last_sms_sweep_error_log: Option<u32> = None;
    let mut last_sms_malformed_count = 0;
    let mut last_sms_malformed_log: Option<u32> = None;
    let mut last_sms_batch_failure: Option<u32> = None;
    let mut pending_notifications: Vec<PendingNotification> = Vec::new();
    let mut pending_deletions: VecDeque<PendingDeletion> = VecDeque::new();
    let mut pending_sms_reads: VecDeque<PendingSmsRead> = VecDeque::new();
    let mut pending_modem_diagnostics = ModemDiagnostics::default();
    let mut last_modem_diagnostics_log: Option<u32> = None;
    let mut last_pdn_event_log: Option<u32> = None;
    // +CMT direct delivery is two lines: header then raw PDU hex.
    // This flag is set when the header arrives so the next poll_urc() line
    // is treated as the PDU rather than a new URC.
    let mut cmt_pdu_pending = false;

    loop {
        let now = now_ms();
        let uptime_ms = elapsed_since(boot_ms, now);
        let log_timestamp = log_clock.timestamp(uptime_ms);
        let mut consumed_slots = Vec::new();

        // Kick the hardware watchdog
        unsafe {
            // SAFETY: The current main task was registered during startup.
            esp_idf_sys::esp_task_wdt_reset();
        }

        if drain_telegram_send_events(&tg_send_event_rx, &mut log, &log_clock, uptime_ms) && wifi_ok
        {
            esp_idf_hal::reset::restart();
        }
        if wifi_ok
            && !commands_registered
            && command_registration.is_none()
            && last_command_registration_attempt
                .is_none_or(|attempt| attempt.elapsed() >= std::time::Duration::from_secs(60))
        {
            last_command_registration_attempt = Some(std::time::Instant::now());
            match messenger.try_register_commands(&registry.command_list()) {
                Ok(receipt) => command_registration = Some(receipt),
                Err(error) => record_event(
                    &mut log,
                    &log_clock,
                    uptime_ms,
                    LogEvent::network(
                        "telegram",
                        &format!("register commands enqueue failed: {}", error),
                        false,
                    ),
                ),
            }
        }
        if let Some(receipt) = command_registration.as_ref() {
            match receipt.try_recv() {
                Ok(Ok(())) => {
                    commands_registered = true;
                    command_registration = None;
                }
                Ok(Err(error)) => {
                    record_event(
                        &mut log,
                        &log_clock,
                        uptime_ms,
                        LogEvent::network(
                            "telegram",
                            &format!("register commands failed: {}", error),
                            false,
                        ),
                    );
                    command_registration = None;
                }
                Err(TryRecvError::Disconnected) => command_registration = None,
                Err(TryRecvError::Empty) => {}
            }
        }

        if messenger.active_for().is_some_and(|elapsed| {
            elapsed
                > smsgate::im::telegram::telegram_restart_after()
                    + std::time::Duration::from_secs(90)
        }) {
            record_event(
                &mut log,
                &log_clock,
                uptime_ms,
                LogEvent::network("telegram", "outbound worker stalled; rebooting", false),
            );
            esp_idf_hal::reset::restart();
        }
        if pending_notifications.iter().any(|pending| {
            pending.queued_at.elapsed()
                > smsgate::im::telegram::telegram_restart_after()
                    + std::time::Duration::from_secs(90)
        }) {
            record_event(
                &mut log,
                &log_clock,
                uptime_ms,
                LogEvent::network(
                    "telegram",
                    "outbound receipt stalled; rebooting with stored SMS retained",
                    false,
                ),
            );
            esp_idf_hal::reset::restart();
        }

        let mut index = 0;
        while index < pending_notifications.len() {
            let result = match pending_notifications[index].receipt.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => {
                    index += 1;
                    continue;
                }
                Err(TryRecvError::Disconnected) => Err(MessengerError::Disconnected),
            };
            let completed = pending_notifications.swap_remove(index);
            match (completed.item, result) {
                (DeferredNotification::Sms(prepared), Ok(message_id)) => {
                    record_forward_success(
                        &prepared.sms,
                        message_id,
                        &mut router,
                        &mut log,
                        &mut *store,
                        &log_timestamp,
                    );
                    pending_deletions.extend(prepared.slots.into_iter().map(PendingDeletion::new));
                }
                (DeferredNotification::Sms(prepared), Err(error)) => {
                    record_forward_failure(&prepared.sms, &error, &mut log, &log_timestamp);
                }
                (DeferredNotification::Call(notification), result) => {
                    let mut event = notification.log_event();
                    if let Err(error) = result {
                        event.forwarded = false;
                        event.body_preview = "incoming call; Telegram notification failed".into();
                        record_event(
                            &mut log,
                            &log_clock,
                            uptime_ms,
                            LogEvent::network(
                                "telegram",
                                &format!("call notification send failed: {}", error),
                                false,
                            ),
                        );
                    }
                    record_event(&mut log, &log_clock, uptime_ms, event);
                }
                (DeferredNotification::Plain { context, .. }, Err(error)) => {
                    record_event(
                        &mut log,
                        &log_clock,
                        uptime_ms,
                        LogEvent::network(
                            "telegram",
                            &format!("{} notification failed: {}", context, error),
                            false,
                        ),
                    );
                }
                (DeferredNotification::Plain { .. }, Ok(_)) => {}
            }
        }

        if let Some(mut deletion) = pending_deletions.pop_front() {
            if deletion
                .retry_after
                .is_some_and(|after| std::time::Instant::now() < after)
            {
                pending_deletions.push_back(deletion);
            } else {
                if !delete_consumed_slot(
                    &deletion.slot,
                    &modem,
                    &mut consumed_slots,
                    &mut sms_delete_error_logs,
                    &mut log,
                    &log_timestamp,
                ) {
                    deletion.retry_after =
                        Some(std::time::Instant::now() + std::time::Duration::from_secs(30));
                    pending_deletions.push_back(deletion);
                }
            }
        }

        // Auto-resume after timed /pause
        if let Some(until) = pause_until {
            if std::time::Instant::now() >= until {
                pause_until = None;
                let _ = smsgate::persist::save_bool(
                    &mut *store,
                    smsgate::persist::keys::FWD_ENABLED,
                    true,
                );
                queue_plain_notification(
                    &mut ready_notifications,
                    smsgate::i18n::resume_ok(),
                    "pause resume",
                    &mut log,
                    &log_timestamp,
                );
                log::info!("[main] pause expired — forwarding re-enabled");
                record_event(
                    &mut log,
                    &log_clock,
                    uptime_ms,
                    LogEvent::user("/pause", "pause expired; forwarding re-enabled", true),
                );
            }
        }

        // Update modem status every 30 s.
        // Skip while cmt_pdu_pending: send_at() drains the UART buffer and would
        // consume the PDU line that belongs to the pending +CMT delivery.
        if elapsed_since(last_status_update, now) > 30_000 && !cmt_pdu_pending {
            modem_status = lock!(modem).update_status();
            last_status_update = now;
            let signal_state = (
                modem_status.csq == smsgate::modem::CSQ_UNKNOWN,
                modem_status.csq_error.is_some(),
            );
            if last_signal_state != Some(signal_state) {
                if signal_state.0 {
                    let detail = modem_status
                        .csq_error
                        .as_deref()
                        .map(|error| format!("CSQ unavailable: {}", error))
                        .unwrap_or_else(|| "modem reported CSQ=99".into());
                    record_event(
                        &mut log,
                        &log_clock,
                        uptime_ms,
                        LogEvent::network("signal", &detail, false),
                    );
                } else if last_signal_state.is_some() {
                    record_event(
                        &mut log,
                        &log_clock,
                        uptime_ms,
                        LogEvent::network(
                            "signal",
                            &format!("CSQ available: {}", modem_status.csq),
                            true,
                        ),
                    );
                }
                last_signal_state = Some(signal_state);
            }
            if let Some(error) = modem_status.registration_error.as_deref() {
                if !registration_query_failed {
                    record_event(
                        &mut log,
                        &log_clock,
                        uptime_ms,
                        LogEvent::network(
                            "registration",
                            &format!("registration query unavailable: {}", error),
                            false,
                        ),
                    );
                }
                registration_query_failed = true;
            } else {
                if registration_query_failed {
                    record_event(
                        &mut log,
                        &log_clock,
                        uptime_ms,
                        LogEvent::network("registration", "registration query recovered", true),
                    );
                    registration_query_failed = false;
                }
                if last_registered != Some(modem_status.registered) {
                    if !modem_status.registered || last_registered.is_some() {
                        record_event(
                            &mut log,
                            &log_clock,
                            uptime_ms,
                            LogEvent::network(
                                "registration",
                                if modem_status.registered {
                                    "network registration restored"
                                } else {
                                    "network registration unavailable"
                                },
                                modem_status.registered,
                            ),
                        );
                    }
                    last_registered = Some(modem_status.registered);
                }
            }
            if !log_clock.is_synced() {
                sync_log_clock_from_modem(&modem, &mut log_clock, &mut log, boot_ms, false);
            }

            // Association alone is not enough: Telegram also needs a usable
            // network interface (for example, after a DHCP lease is lost).
            let network_up = wifi.is_up().unwrap_or(false);
            if network_up && !wifi_ok {
                last_tg_activity = std::time::Instant::now();
                record_event(
                    &mut log,
                    &log_clock,
                    uptime_ms,
                    LogEvent::network("wifi", "network interface restored", true),
                );
                last_wifi_reconnect_attempt = None;
            }
            wifi_ok = network_up;

            // Refresh WiFi RSSI
            let rssi = if wifi_ok {
                wifi.wifi()
                    .get_rssi()
                    .ok()
                    .filter(|rssi| (-127..0).contains(rssi))
            } else {
                None
            };
            wifi_info = fmt_network(wifi_ok, rssi, &creds.wifi_ssid);

            // Low-heap alert
            if let Some(alert) = check_low_heap() {
                if !low_heap_alerted {
                    log::warn!("[main] low heap alert");
                    queue_plain_notification(
                        &mut ready_notifications,
                        alert,
                        "low heap",
                        &mut log,
                        &log_timestamp,
                    );
                    low_heap_alerted = true;
                }
            } else {
                low_heap_alerted = false;
            }

            // CSQ low-signal alert (threshold: CSQ ≤ 5, roughly < −103 dBm)
            const CSQ_WEAK: u8 = 5;
            if modem_status.csq != smsgate::modem::CSQ_UNKNOWN {
                if modem_status.csq <= CSQ_WEAK && !low_signal_alerted {
                    low_signal_alerted = true;
                    queue_plain_notification(
                        &mut ready_notifications,
                        smsgate::i18n::low_signal(modem_status.csq),
                        "low signal",
                        &mut log,
                        &log_timestamp,
                    );
                    record_event(
                        &mut log,
                        &log_clock,
                        uptime_ms,
                        LogEvent::network(
                            "signal",
                            &format!("low CSQ {}", modem_status.csq),
                            false,
                        ),
                    );
                } else if modem_status.csq > CSQ_WEAK && low_signal_alerted {
                    low_signal_alerted = false;
                    queue_plain_notification(
                        &mut ready_notifications,
                        smsgate::i18n::signal_restored(modem_status.csq),
                        "signal restored",
                        &mut log,
                        &log_timestamp,
                    );
                    record_event(
                        &mut log,
                        &log_clock,
                        uptime_ms,
                        LogEvent::network(
                            "signal",
                            &format!("restored CSQ {}", modem_status.csq),
                            true,
                        ),
                    );
                }
            }

            // Operator change alert (skip the initial "" → "SomeOp" transition)
            if !modem_status.operator.is_empty()
                && !last_operator.is_empty()
                && modem_status.operator != last_operator
            {
                queue_plain_notification(
                    &mut ready_notifications,
                    smsgate::i18n::operator_changed(&last_operator, &modem_status.operator),
                    "operator changed",
                    &mut log,
                    &log_timestamp,
                );
                record_event(
                    &mut log,
                    &log_clock,
                    uptime_ms,
                    LogEvent::network(
                        "operator",
                        &format!("{} -> {}", last_operator, modem_status.operator),
                        true,
                    ),
                );
            }
            if !modem_status.operator.is_empty() {
                last_operator.clone_from(&modem_status.operator);
            }

            // WiFi watchdog: recover both a failed initial connection and a
            // station that later loses its association or DHCP lease.
            if !wifi_ok
                && last_wifi_reconnect_attempt
                    .is_none_or(|attempt| attempt.elapsed() >= std::time::Duration::from_secs(60))
            {
                last_wifi_reconnect_attempt = Some(std::time::Instant::now());
                log::warn!("[wifi] disconnected — reconnecting");
                record_event(
                    &mut log,
                    &log_clock,
                    uptime_ms,
                    LogEvent::network("wifi", "disconnected", false),
                );
                let reconnected = if wifi.is_started().unwrap_or(false) {
                    reconnect_wifi(&mut wifi)
                } else {
                    start_wifi_once(&mut wifi, &creds.wifi_ssid, &creds.wifi_pass)
                };
                if reconnected {
                    wifi_ok = true;
                    last_wifi_reconnect_attempt = None;
                    log::info!("[wifi] reconnected OK");
                    record_event(
                        &mut log,
                        &log_clock,
                        uptime_ms,
                        LogEvent::network("wifi", "reconnected", true),
                    );
                } else {
                    wifi_ok = false;
                    log::error!("[wifi] reconnect failed — will retry next cycle");
                    record_event(
                        &mut log,
                        &log_clock,
                        uptime_ms,
                        LogEvent::network("wifi", "reconnect failed", false),
                    );
                }
            }

            let stale_elapsed = last_tg_activity.elapsed();
            if wifi_ok && should_restart_after_stale_poll(stale_elapsed) {
                let mins = (stale_elapsed.as_secs() / 60) as u32;
                log::error!("[main] tg-poll stale for {} min — rebooting", mins);
                record_event(
                    &mut log,
                    &log_clock,
                    uptime_ms,
                    LogEvent::network(
                        "telegram",
                        &format!("poll stale for {} min; rebooting", mins),
                        false,
                    ),
                );
                esp_idf_hal::reset::restart();
            }
        }

        let mut pending_sms = Vec::new();
        let mut pending_sms_errors: Vec<(String, u16, String)> = Vec::new();
        let mut direct_pdus = Vec::new();
        let mut call_notifications = Vec::new();

        // Poll URCs (non-blocking); hold one lock only for modem operations.
        let modem_diagnostics = {
            let mut md = lock!(modem);
            // Bound one pass so a noisy modem cannot starve SMS forwarding,
            // Telegram event handling, or the main-task watchdog.
            for _ in 0..64 {
                let Some(urc) = md.poll_urc() else {
                    break;
                };
                if urc.starts_with("+CGEV:") {
                    log::debug!("[main] URC: {:?}", urc);
                } else {
                    log::info!("[main] URC: {:?}", urc);
                }

                // +CMT two-line protocol: header sets the flag, next line is the PDU.
                // Direct delivery has no modem slot — nothing to delete afterwards.
                if cmt_pdu_pending {
                    cmt_pdu_pending = false;
                    direct_pdus.push(urc.trim().to_string());
                    continue;
                }

                match parse_urc(&urc) {
                    Urc::NewSms { mem, index } => {
                        if !pending_sms_reads
                            .iter()
                            .any(|pending| pending.mem == mem && pending.index == index)
                        {
                            const MAX_PENDING_SMS_READS: usize = 128;
                            if pending_sms_reads.len() >= MAX_PENDING_SMS_READS && mem == "SM" {
                                // ME slots are recovered by the periodic sweep; an SM
                                // notification has no such recovery path on this modem.
                                if let Some(position) = pending_sms_reads
                                    .iter()
                                    .position(|pending| pending.mem == "ME")
                                {
                                    pending_sms_reads.remove(position);
                                }
                            }
                            if pending_sms_reads.len() < MAX_PENDING_SMS_READS {
                                pending_sms_reads.push_back(PendingSmsRead {
                                    mem,
                                    index,
                                    attempts: 0,
                                    retry_after: None,
                                });
                            } else {
                                let detail = if mem == "SM" {
                                    "SM read queue full; no automatic SM sweep configured"
                                } else {
                                    "SMS read queue full; ME storage sweep needed"
                                };
                                pending_sms_errors.push((mem, index, detail.into()));
                            }
                        }
                    }
                    Urc::SmsDelivery => {
                        cmt_pdu_pending = true; // next poll_urc() line is the raw PDU
                    }
                    _ => {
                        if let Some(notification) = call_handler.handle_urc_deferred(&urc, &mut *md)
                        {
                            call_notifications.push(notification);
                        }
                    }
                }
            }
            if let Some(notification) = call_handler.tick_deferred(&mut *md) {
                call_notifications.push(notification);
            }
            md.take_diagnostics()
        };

        pending_modem_diagnostics.accumulate(modem_diagnostics);
        if !pending_modem_diagnostics.receive_faults_empty()
            && last_modem_diagnostics_log
                .is_none_or(|previous| elapsed_since(previous, now) >= FAULT_LOG_INTERVAL_MS)
        {
            let faults = pending_modem_diagnostics;
            pending_modem_diagnostics.dropped_urcs = 0;
            pending_modem_diagnostics.dropped_response_lines = 0;
            pending_modem_diagnostics.overlong_lines = 0;
            last_modem_diagnostics_log = Some(now);
            record_event(
                &mut log,
                &log_clock,
                uptime_ms,
                LogEvent::network(
                    "modem UART",
                    &format!(
                        "receive faults: URCs dropped={}, response lines dropped={}, overlong lines={}",
                        faults.dropped_urcs,
                        faults.dropped_response_lines,
                        faults.overlong_lines
                    ),
                    false,
                ),
            );
        }
        if !pending_modem_diagnostics.pdn_events_empty()
            && last_pdn_event_log
                .is_none_or(|previous| elapsed_since(previous, now) >= PDN_EVENT_LOG_INTERVAL_MS)
        {
            let activations = pending_modem_diagnostics.pdn_activations;
            let deactivations = pending_modem_diagnostics.pdn_deactivations;
            pending_modem_diagnostics.pdn_activations = 0;
            pending_modem_diagnostics.pdn_deactivations = 0;
            last_pdn_event_log = Some(now);
            record_event(
                &mut log,
                &log_clock,
                uptime_ms,
                LogEvent::network(
                    "modem PDN",
                    &format!(
                        "context events: activated={}, deactivated={}",
                        activations, deactivations
                    ),
                    deactivations == 0,
                ),
            );
        }

        for (mem, index, error) in pending_sms_errors {
            log::error!(
                "[main] SMS read failed at {} slot {}: {}",
                mem,
                index,
                error
            );
            record_event(
                &mut log,
                &log_clock,
                uptime_ms,
                LogEvent::new(
                    LogKind::Sms,
                    &format!("{}:{}", mem, index),
                    &format!("read failed; slot retained: {}", error),
                    false,
                ),
            );
        }

        for notification in call_notifications {
            if let Some(error) = notification.hang_up_error() {
                record_event(
                    &mut log,
                    &log_clock,
                    uptime_ms,
                    LogEvent::new(
                        LogKind::Call,
                        "modem",
                        &format!("hang-up failed: {}", error),
                        false,
                    ),
                );
            }
            if reserve_ephemeral_notification_slot(
                &mut ready_notifications,
                &mut log,
                &log_timestamp,
            ) {
                ready_notifications.push_front(DeferredNotification::Call(notification));
            } else {
                record_event(
                    &mut log,
                    &log_clock,
                    uptime_ms,
                    LogEvent::network("telegram", "call notification queue full", false),
                );
            }
        }

        for pdu in direct_pdus {
            if !reserve_ephemeral_notification_slot(
                &mut ready_notifications,
                &mut log,
                &log_timestamp,
            ) {
                record_event(
                    &mut log,
                    &log_clock,
                    uptime_ms,
                    LogEvent::new(
                        LogKind::Sms,
                        "direct",
                        "notification queue full; direct SMS not retained",
                        false,
                    ),
                );
                continue;
            }
            let preparation =
                prepare_pdu_hex_with_slot(&pdu, 0, None, &mut log, &mut concat, &log_timestamp);
            queue_sms_preparation(
                preparation,
                &mut ready_notifications,
                &mut pending_deletions,
                &mut router,
                &mut log,
                &mut messenger,
                &mut *store,
                &log_timestamp,
            );
        }

        // Service URCs and queue unrepeatable notifications before a possibly
        // slow CPMS/CMGR exchange. Read at most one stored slot per pass.
        if let Some(index) = pending_sms_reads.iter().position(|pending| {
            pending
                .retry_after
                .is_none_or(|after| std::time::Instant::now() >= after)
        }) {
            let mut pending = pending_sms_reads
                .remove(index)
                .expect("pending SMS index checked");
            let result = {
                let mut md = lock!(modem);
                read_new_sms_pdu(&pending.mem, pending.index, &mut *md)
            };
            match result {
                Ok(stored) => pending_sms.push(stored),
                Err(error) => {
                    pending.attempts = pending.attempts.saturating_add(1);
                    record_event(
                        &mut log,
                        &log_clock,
                        uptime_ms,
                        LogEvent::new(
                            LogKind::Sms,
                            &format!("{}:{}", pending.mem, pending.index),
                            &format!(
                                "read failed (attempt {}); slot retained: {}",
                                pending.attempts, error
                            ),
                            false,
                        ),
                    );
                    if pending.attempts < 8 || pending.mem == "SM" {
                        let delay = if pending.attempts < 8 { 30 } else { 5 * 60 };
                        pending.retry_after =
                            Some(std::time::Instant::now() + std::time::Duration::from_secs(delay));
                        pending_sms_reads.push_back(pending);
                    }
                }
            }
        }

        for sms in pending_sms {
            if notification_slot_in_flight(
                &sms.storage_slot(),
                &ready_notifications,
                &pending_notifications,
            ) || deletion_slot_pending(&sms.storage_slot(), &pending_deletions)
                || !reserve_critical_notification_slot(&mut ready_notifications)
            {
                continue;
            }
            let preparation = prepare_stored_sms(sms, &mut log, &mut concat, &log_timestamp);
            queue_sms_preparation(
                preparation,
                &mut ready_notifications,
                &mut pending_deletions,
                &mut router,
                &mut log,
                &mut messenger,
                &mut *store,
                &log_timestamp,
            );
        }

        // Recover stored messages whose +CMTI was lost or whose first read
        // failed. Never hold the modem lock while forwarding to Telegram.
        if pending_sweep_indices.is_empty()
            && elapsed_since(last_sms_sweep, now) >= SMS_SWEEP_INTERVAL_MS
        {
            last_sms_sweep = now;
            let sweep_result = {
                let mut md = lock!(modem);
                scan_stored_sms_indices("ME", &mut *md)
            };
            match sweep_result {
                Ok(scan) => {
                    if last_sms_sweep_error.take().is_some() {
                        record_event(
                            &mut log,
                            &log_clock,
                            uptime_ms,
                            LogEvent::new(LogKind::Sms, "ME", "storage sweep recovered", true),
                        );
                    }
                    if scan.malformed_entries > 0
                        && (scan.malformed_entries != last_sms_malformed_count
                            || last_sms_malformed_log.is_none_or(|previous| {
                                elapsed_since(previous, now) >= SWEEP_ERROR_LOG_INTERVAL_MS
                            }))
                    {
                        record_event(
                            &mut log,
                            &log_clock,
                            uptime_ms,
                            LogEvent::new(
                                LogKind::Sms,
                                "ME",
                                &format!(
                                    "storage sweep skipped {} malformed entries",
                                    scan.malformed_entries
                                ),
                                false,
                            ),
                        );
                        last_sms_malformed_log = Some(now);
                    }
                    if scan.malformed_entries == 0 && last_sms_malformed_count > 0 {
                        record_event(
                            &mut log,
                            &log_clock,
                            uptime_ms,
                            LogEvent::new(LogKind::Sms, "ME", "storage list recovered", true),
                        );
                    }
                    last_sms_malformed_count = scan.malformed_entries;
                    // A previously delivered slot may have disappeared while
                    // CMGD was failing. Do not keep its retry in RAM forever.
                    if scan.malformed_entries == 0 {
                        pending_deletions.retain(|deletion| {
                            deletion.slot.mem != "ME"
                                || scan.indices.binary_search(&deletion.slot.index).is_ok()
                        });
                    }
                    pending_sweep_indices.extend(scan.indices);
                }
                Err(e) => {
                    let detail = e.to_string();
                    let should_log = last_sms_sweep_error.as_deref() != Some(detail.as_str())
                        || last_sms_sweep_error_log.is_none_or(|previous| {
                            elapsed_since(previous, now) >= SWEEP_ERROR_LOG_INTERVAL_MS
                        });
                    if should_log {
                        log::error!("[main] periodic SMS sweep failed: {}", detail);
                        record_event(
                            &mut log,
                            &log_clock,
                            uptime_ms,
                            LogEvent::new(
                                LogKind::Sms,
                                "ME",
                                &format!("periodic sweep failed: {}", detail),
                                false,
                            ),
                        );
                        last_sms_sweep_error_log = Some(now);
                    }
                    last_sms_sweep_error = Some(detail);
                }
            }
        }

        // Drain at most four stored slots per pass so call URCs and Telegram
        // commands get a turn between backlog batches. No network operation
        // runs while the modem mutex is held.
        const SMS_SWEEP_BATCH_SIZE: usize = 4;
        const SMS_SWEEP_BATCH_RETRY_MS: u32 = 30_000;
        if !pending_sweep_indices.is_empty()
            && (ready_notifications.len() < MAX_READY_NOTIFICATIONS
                || ready_notifications
                    .iter()
                    .any(|item| matches!(item, DeferredNotification::Plain { .. })))
            && last_sms_batch_failure
                .is_none_or(|previous| elapsed_since(previous, now) >= SMS_SWEEP_BATCH_RETRY_MS)
        {
            let indices: Vec<u16> = pending_sweep_indices
                .drain(..pending_sweep_indices.len().min(SMS_SWEEP_BATCH_SIZE))
                .collect();
            let batch = {
                let mut md = lock!(modem);
                read_stored_sms_batch("ME", &indices, &mut *md)
            };
            match batch {
                Ok(batch) => {
                    last_sms_batch_failure = None;
                    for (index, result) in batch.entries {
                        match result {
                            Ok(sms)
                                if !consumed_slots.contains(&sms.storage_slot())
                                    && !deletion_slot_pending(
                                        &sms.storage_slot(),
                                        &pending_deletions,
                                    )
                                    && !notification_slot_in_flight(
                                        &sms.storage_slot(),
                                        &ready_notifications,
                                        &pending_notifications,
                                    ) =>
                            {
                                if !reserve_critical_notification_slot(&mut ready_notifications) {
                                    pending_sweep_indices.push_front(index);
                                    continue;
                                }
                                let timestamp =
                                    log_clock.timestamp(elapsed_since(boot_ms, now_ms()));
                                let preparation =
                                    prepare_stored_sms(sms, &mut log, &mut concat, &timestamp);
                                queue_sms_preparation(
                                    preparation,
                                    &mut ready_notifications,
                                    &mut pending_deletions,
                                    &mut router,
                                    &mut log,
                                    &mut messenger,
                                    &mut *store,
                                    &timestamp,
                                );
                            }
                            Ok(_) => {}
                            Err(error) => {
                                record_event(
                                    &mut log,
                                    &log_clock,
                                    elapsed_since(boot_ms, now_ms()),
                                    LogEvent::new(
                                        LogKind::Sms,
                                        &format!("ME:{}", index),
                                        &format!("sweep read failed; slot retained: {}", error),
                                        false,
                                    ),
                                );
                            }
                        }
                    }
                }
                Err(error) => {
                    for index in indices.into_iter().rev() {
                        pending_sweep_indices.push_front(index);
                    }
                    last_sms_batch_failure = Some(now);
                    record_event(
                        &mut log,
                        &log_clock,
                        elapsed_since(boot_ms, now_ms()),
                        LogEvent::new(
                            LogKind::Sms,
                            "ME",
                            &format!("sweep batch delayed; slots retained: {}", error),
                            false,
                        ),
                    );
                }
            }
        }

        if elapsed_since(last_notification_check, now) >= NOTIFICATION_CHECK_INTERVAL_MS {
            last_notification_check = now;
            for event in refresh_modem_notifications(&modem) {
                record_event(&mut log, &log_clock, uptime_ms, event);
            }
        }

        // Collect any Telegram messages delivered by the polling thread
        let mut telegram_recovery_reason = None;
        let mut tg_messages: Vec<smsgate::im::InboundMessage> = {
            let mut batch = Vec::new();
            let mut channel_active = false;
            for _ in 0..MAX_TG_POLL_EVENTS_PER_PASS {
                match tg_rx.try_recv() {
                    Ok(TgPollEvent::Batch { batch: polled, ack }) => {
                        channel_active = true;
                        if let Some(ack) = ack {
                            // The poll task will not fetch a later update until
                            // this batch is checkpointed and acknowledged.
                            pending_tg_batch = Some((polled, ack));
                            break;
                        }
                        batch.extend(polled.messages);
                    }
                    Ok(TgPollEvent::Log(event)) => {
                        channel_active = true;
                        record_event(&mut log, &log_clock, uptime_ms, event);
                    }
                    Ok(TgPollEvent::RecoverTransport { reason }) => {
                        channel_active = true;
                        if telegram_recovery_reason.is_none() {
                            telegram_recovery_reason = Some(reason);
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        log::error!("[main] tg-poll thread died — rebooting");
                        record_event(
                            &mut log,
                            &log_clock,
                            uptime_ms,
                            LogEvent::network("telegram", "poll thread died; rebooting", false),
                        );
                        esp_idf_hal::reset::restart();
                    }
                }
            }
            if channel_active {
                log::debug!("[main] drained Telegram batch: {} message(s)", batch.len());
                last_tg_activity = std::time::Instant::now();
            }
            batch
        };

        let mut tg_ack = None;
        if pending_tg_batch.is_some()
            && cursor_retry_after.is_none_or(|after| std::time::Instant::now() >= after)
        {
            let next_cursor = pending_tg_batch
                .as_ref()
                .map(|(batch, _)| batch.next_cursor)
                .unwrap_or(persisted_tg_cursor);
            match smsgate::bridge::poller::checkpoint_cursor(
                &mut *store,
                &mut persisted_tg_cursor,
                next_cursor,
            ) {
                Ok(()) => {
                    cursor_retry_after = None;
                    let (batch, ack) = pending_tg_batch.take().expect("pending batch checked");
                    log::info!("[main] Telegram cursor persisted: {}", next_cursor);
                    tg_messages.extend(batch.messages);
                    tg_ack = Some(ack);
                }
                Err(error) => {
                    cursor_retry_after =
                        Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
                    log::error!("[main] Telegram cursor persistence failed: {}", error);
                    record_event(
                        &mut log,
                        &log_clock,
                        uptime_ms,
                        LogEvent::network(
                            "telegram",
                            &format!("cursor persistence failed: {}", error),
                            false,
                        ),
                    );
                }
            }
        }

        if let Some(reason) = telegram_recovery_reason {
            recover_wifi_after_telegram_failure(
                &reason, &mut wifi, &mut log, &log_clock, uptime_ms, &wifi_info, wifi_ok,
            );
        }

        // Dispatch commands and replies, update cursor in NVS
        if !tg_messages.is_empty() {
            let document_count = tg_messages.iter().filter(|m| m.document.is_some()).count();
            log::info!(
                "[main] processing Telegram batch: messages={} documents={}",
                tg_messages.len(),
                document_count
            );
            let latest_ota_cursor = smsgate::ota::latest_ota_document_cursor(&tg_messages);
            for msg in tg_messages.iter().filter(|m| m.document.is_some()) {
                if let Some(document) = msg.document.as_ref() {
                    log::info!(
                        "[main] Telegram document message: cursor={} text_len={} file_name={} mime={} size={:?}",
                        msg.cursor,
                        msg.text.len(),
                        document.file_name.as_deref().unwrap_or("<none>"),
                        document.mime_type.as_deref().unwrap_or("<none>"),
                        document.file_size
                    );
                    if smsgate::ota::is_ota_caption(&msg.text)
                        && latest_ota_cursor.is_some()
                        && Some(msg.cursor) != latest_ota_cursor
                    {
                        let name = document.file_name.as_deref().unwrap_or("firmware image");
                        log::warn!(
                            "[ota] ignoring stale OTA document: cursor={} latest_cursor={:?} file_name={}",
                            msg.cursor,
                            latest_ota_cursor,
                            name
                        );
                        let _ = send_ota_message(
                            &mut messenger,
                            "ignored_stale",
                            &smsgate::i18n::ota_ignored_stale(name),
                        );
                        drain_tg_send!();
                        continue;
                    }
                    handle_ota_document(
                        &msg.text,
                        document,
                        &mut messenger,
                        &creds.bot_token,
                        &mut log,
                        &log_clock,
                        boot_ms,
                        &tg_send_event_rx,
                    );
                    drain_tg_send!();
                }
            }
            let mut dispatch_messages = Vec::new();
            for msg in tg_messages {
                if msg.document.is_none() && msg.callback.is_none() {
                    if let Some(request) = parse_hidden_at_command(&msg.text) {
                        let (reply, succeeded, detail) = match request {
                            Ok(suffix) => {
                                let reply = {
                                    let mut md = lock!(modem);
                                    execute_at_command(suffix, &mut *md)
                                };
                                let detail = if reply.succeeded {
                                    "manual AT command completed"
                                } else {
                                    "manual AT command failed"
                                };
                                (reply.text, reply.succeeded, detail)
                            }
                            Err(error) => (error.reply().to_string(), false, "AT command rejected"),
                        };
                        record_event(
                            &mut log,
                            &log_clock,
                            elapsed_since(boot_ms, now_ms()),
                            LogEvent::user("/at", detail, succeeded),
                        );
                        queue_plain_notification(
                            &mut ready_notifications,
                            reply,
                            "AT reply",
                            &mut log,
                            &log_timestamp,
                        );
                        continue;
                    }
                }
                dispatch_messages.push(msg);
            }
            if dispatch_messages.iter().any(|m| m.document.is_none()) {
                // SAFETY: These ESP-IDF getters return scalar heap statistics.
                let free_heap = unsafe { esp_idf_sys::esp_get_free_heap_size() };
                // SAFETY: This getter also takes no pointers or mutable arguments.
                let min_free_heap = unsafe { esp_idf_sys::esp_get_minimum_free_heap_size() };
                let dispatch = {
                    let mut command_responder = QueuedCommandResponder::new(&messenger);
                    poll_and_dispatch(
                        &dispatch_messages,
                        &mut command_responder,
                        &mut sender,
                        &router,
                        &registry,
                        &mut *store,
                        &log,
                        &modem_status,
                        uptime_ms,
                        free_heap,
                        min_free_heap,
                        &wifi_info,
                    )
                };
                match dispatch {
                    Ok(outcome) => {
                        drain_tg_send!();
                        consecutive_failures = 0;
                        for event in outcome.events {
                            record_event(&mut log, &log_clock, uptime_ms, event);
                        }
                        if let Some(mins) = outcome.pause_mins {
                            // Cap at 1 week to prevent Duration overflow on pathological input.
                            const MAX_PAUSE_MINS: u64 = 7 * 24 * 60;
                            let secs = (mins as u64).min(MAX_PAUSE_MINS) * 60;
                            pause_until = Some(
                                std::time::Instant::now() + std::time::Duration::from_secs(secs),
                            );
                            log::info!("[main] pause timer set for {} min", mins);
                        }
                        if outcome.restart_requested {
                            log::info!("[main] restart requested via /restart command");
                            if let Some(reply) = outcome.restart_reply {
                                if let Err(error) = messenger.send_message(&reply) {
                                    log::warn!("[main] restart reply failed: {}", error);
                                }
                            }
                            esp_idf_hal::reset::restart();
                        }
                    }
                    Err(e) => {
                        drain_tg_send!();
                        consecutive_failures += 1;
                        log::error!("[main] send failed ({}): {}", consecutive_failures, e);
                        record_event(
                            &mut log,
                            &log_clock,
                            uptime_ms,
                            LogEvent::network(
                                "telegram",
                                &format!("dispatch send failed x{}: {}", consecutive_failures, e),
                                false,
                            ),
                        );
                        if consecutive_failures >= creds.max_failures_before_reboot {
                            log::error!("[main] max failures reached — rebooting");
                            esp_idf_hal::reset::restart();
                        }
                    }
                }
            }
        }

        // Only now may the poll task advance the Telegram server-side offset.
        // The NVS checkpoint was written before command side effects, so an
        // intentional reboot does not replay /restart or /ota.
        if let Some(ack) = tg_ack {
            let _ = ack.send(());
        }

        let drain = {
            let mut md = lock!(modem);
            sender.drain_once(&mut *md)
        };

        match &drain {
            DrainOutcome::Sent { phone } => {
                queue_plain_notification(
                    &mut ready_notifications,
                    smsgate::i18n::sms_sent_ok(phone),
                    "outbound SMS success",
                    &mut log,
                    &log_timestamp,
                );
                record_event(
                    &mut log,
                    &log_clock,
                    uptime_ms,
                    LogEvent::user("/send", &format!("SMS sent to {}", phone), true),
                );
            }
            DrainOutcome::Dropped { phone } => {
                queue_plain_notification(
                    &mut ready_notifications,
                    smsgate::i18n::sms_failed(phone),
                    "outbound SMS failure",
                    &mut log,
                    &log_timestamp,
                );
                record_event(
                    &mut log,
                    &log_clock,
                    uptime_ms,
                    LogEvent::user("/send", &format!("SMS failed to {}", phone), false),
                );
            }
            _ => {}
        }

        while wifi_ok && pending_notifications.len() < MAX_PENDING_NOTIFICATIONS {
            // Pause new SMS forwards if too many delivered slots still need
            // deletion, but keep call and operational notifications flowing.
            let next_index = if pending_deletions.len() >= MAX_PENDING_DELETIONS {
                ready_notifications
                    .iter()
                    .position(|item| !matches!(item, DeferredNotification::Sms(_)))
            } else {
                Some(0)
            };
            let Some((next_index, next)) = next_index
                .and_then(|index| ready_notifications.get(index).map(|item| (index, item)))
            else {
                break;
            };
            let (text, format) = match next {
                DeferredNotification::Sms(prepared) => {
                    (sms_forward_text(&prepared.sms), MessageFormat::Html)
                }
                DeferredNotification::Call(notification) => {
                    (notification.text.clone(), MessageFormat::Plain)
                }
                DeferredNotification::Plain { text, .. } => (text.clone(), MessageFormat::Plain),
            };
            match messenger.try_send_message_with_format_owned(text, format) {
                Ok(receipt) => {
                    let item = ready_notifications
                        .remove(next_index)
                        .expect("checked notification index");
                    pending_notifications.push(PendingNotification {
                        item,
                        receipt,
                        queued_at: std::time::Instant::now(),
                    });
                }
                Err(MessengerError::Timeout(_)) => break,
                Err(error) => {
                    record_event(
                        &mut log,
                        &log_clock,
                        uptime_ms,
                        LogEvent::network(
                            "telegram",
                            &format!("outbound worker unavailable: {}", error),
                            false,
                        ),
                    );
                    esp_idf_hal::reset::restart();
                }
            }
        }

        // Skip sleep when drain_once did real work (AT exchange already took ~200ms);
        // otherwise yield to keep URC latency under 100 ms without busy-looping.
        if !drain.attempted() {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}

#[cfg(feature = "esp32")]
#[allow(clippy::too_many_arguments)]
fn queue_sms_preparation(
    preparation: SmsPreparation,
    ready: &mut VecDeque<DeferredNotification>,
    deletions: &mut VecDeque<PendingDeletion>,
    router: &mut ReplyRouter,
    log: &mut LogRing,
    messenger: &mut dyn MessageSink,
    store: &mut dyn Store,
    timestamp: &str,
) {
    match preparation {
        SmsPreparation::Retain => {}
        SmsPreparation::Consumed(slots) => {
            deletions.extend(slots.into_iter().map(PendingDeletion::new));
        }
        SmsPreparation::Ready(prepared) => {
            if load_bool(store, keys::FWD_ENABLED) == Some(false)
                || is_blocked(&prepared.sms.sender, store)
            {
                // forward_sms records the intentional drop without issuing an IM request.
                let _ = forward_sms(&prepared.sms, messenger, router, log, store, timestamp);
                deletions.extend(prepared.slots.into_iter().map(PendingDeletion::new));
            } else {
                let priority_end = ready
                    .iter()
                    .position(|item| matches!(item, DeferredNotification::Plain { .. }))
                    .unwrap_or(ready.len());
                ready.insert(priority_end, DeferredNotification::Sms(prepared));
            }
        }
    }
}

#[cfg(feature = "esp32")]
fn queue_plain_notification(
    ready: &mut VecDeque<DeferredNotification>,
    text: impl Into<String>,
    context: &'static str,
    log: &mut LogRing,
    timestamp: &str,
) {
    if ready.len() >= MAX_READY_NOTIFICATIONS {
        log.push(
            LogEvent::network(
                "telegram",
                &format!("{} notification queue full", context),
                false,
            )
            .at(timestamp),
        );
    } else {
        ready.push_back(DeferredNotification::Plain {
            text: text.into(),
            context,
        });
    }
}

#[cfg(feature = "esp32")]
fn reserve_critical_notification_slot(ready: &mut VecDeque<DeferredNotification>) -> bool {
    if ready.len() >= MAX_READY_NOTIFICATIONS {
        if let Some(index) = ready
            .iter()
            .rposition(|item| matches!(item, DeferredNotification::Plain { .. }))
        {
            ready.remove(index);
        }
    }
    ready.len() < MAX_READY_NOTIFICATIONS
}

#[cfg(feature = "esp32")]
fn reserve_ephemeral_notification_slot(
    ready: &mut VecDeque<DeferredNotification>,
    log: &mut LogRing,
    timestamp: &str,
) -> bool {
    if reserve_critical_notification_slot(ready) {
        return true;
    }
    // A stored SMS remains in the modem until Telegram confirms delivery;
    // sweeping it later is safer than losing an unrepeatable call or Class 0
    // notification while the outbound queue is full.
    if let Some(index) = ready.iter().rposition(
        |item| matches!(item, DeferredNotification::Sms(prepared) if !prepared.slots.is_empty()),
    ) {
        ready.remove(index);
        log.push(
            LogEvent::network(
                "telegram",
                "stored SMS deferred for call or direct SMS; modem slot retained",
                false,
            )
            .at(timestamp),
        );
    }
    ready.len() < MAX_READY_NOTIFICATIONS
}

#[cfg(feature = "esp32")]
fn notification_slot_in_flight(
    slot: &StorageSlot,
    ready: &VecDeque<DeferredNotification>,
    pending: &[PendingNotification],
) -> bool {
    let includes_slot = |item: &DeferredNotification| match item {
        DeferredNotification::Sms(prepared) => prepared
            .slots
            .iter()
            .any(|candidate| candidate.mem == slot.mem && candidate.index == slot.index),
        DeferredNotification::Call(_) => false,
        DeferredNotification::Plain { .. } => false,
    };
    ready.iter().any(&includes_slot) || pending.iter().any(|item| includes_slot(&item.item))
}

#[cfg(feature = "esp32")]
fn deletion_slot_pending(slot: &StorageSlot, pending: &VecDeque<PendingDeletion>) -> bool {
    pending
        .iter()
        .any(|candidate| candidate.slot.mem == slot.mem && candidate.slot.index == slot.index)
}

#[cfg(feature = "esp32")]
fn build_registry(help_text: &str) -> CommandRegistry {
    let mut r = CommandRegistry::new();
    r.register(Box::new(HelpCommand {
        help_text: help_text.to_string(),
    }));
    r.register(Box::new(StatusCommand));
    r.register(Box::new(SendCommand));
    r.register(Box::new(LogCommand));
    r.register(Box::new(BlockCommand));
    r.register(Box::new(BlockListCommand));
    r.register(Box::new(UnblockCommand));
    r.register(Box::new(PauseCommand));
    r.register(Box::new(ResumeCommand));
    r.register(Box::new(RestartCommand));
    r
}

#[cfg(feature = "esp32")]
fn build_telegram_messenger(token: String, chat_id: i64) -> anyhow::Result<TelegramMessenger> {
    Ok(TelegramMessenger::new(
        TelegramHttpClient::new(None)?,
        token,
        chat_id,
    ))
}

#[cfg(feature = "esp32")]
fn telegram_poll_timeout_secs(poll_interval_ms: u32) -> u32 {
    (poll_interval_ms / 1000).clamp(1, 30)
}

#[cfg(feature = "esp32")]
fn sleep_with_poll_activity(
    duration: std::time::Duration,
    tx: &PollEventSender,
    cursor: i64,
) -> bool {
    const HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(30);
    const WATCHDOG_TICK: std::time::Duration = std::time::Duration::from_secs(5);

    let started = std::time::Instant::now();
    let mut last_heartbeat = started;
    loop {
        unsafe {
            // SAFETY: This resets only the current registered poll task.
            let _ = esp_idf_sys::esp_task_wdt_reset();
        }
        let remaining = duration.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return true;
        }
        std::thread::sleep(remaining.min(WATCHDOG_TICK));
        if last_heartbeat.elapsed() >= HEARTBEAT {
            last_heartbeat = std::time::Instant::now();
            if tx
                .send(TgPollEvent::Batch {
                    batch: PollBatch {
                        messages: Vec::new(),
                        next_cursor: cursor,
                    },
                    ack: None,
                })
                .is_err()
            {
                return false;
            }
        }
    }
}

#[cfg(feature = "esp32")]
fn build_log_ring() -> LogRing {
    match smsgate::log_ring::open_flash_log_ring("log_ring") {
        Ok(log) => {
            log::info!("[log] flash-backed log ring mounted");
            log
        }
        Err(e) => {
            log::warn!("[log] flash log unavailable: {} — using RAM log", e);
            LogRing::new()
        }
    }
}

#[cfg(feature = "esp32")]
fn record_event(log: &mut LogRing, clock: &LogClock, uptime_ms: u32, event: LogEvent) {
    let timestamp = clock.timestamp(uptime_ms);
    log.push(event.at(&timestamp));
}

#[cfg(feature = "esp32")]
fn refresh_modem_notifications(
    modem: &std::sync::Arc<std::sync::Mutex<dyn ModemPort + Send>>,
) -> Vec<LogEvent> {
    let (cnmi_query, cnmi_restore, cnmi_verify, clip_set) = {
        let mut md = lock!(modem);
        let query = md.send_at("+CNMI?");
        let configured = matches!(&query, Ok(response) if response.ok && cnmi_store_notifications_enabled(&response.body));
        let restore = (!configured).then(|| md.send_at("+CNMI=2,1,0,0,0"));
        let verify = restore
            .as_ref()
            .filter(|result| at_command_failure(result).is_none())
            .map(|_| md.send_at("+CNMI?"));
        let clip = md.send_at("+CLIP=1");
        (query, restore, verify, clip)
    };

    let mut events = Vec::new();
    if let Some(result) = cnmi_restore {
        match at_command_failure(&result) {
            Some(detail) => {
                log::error!("[main] CNMI reassert failed: {}", detail);
                events.push(LogEvent::network(
                    "modem SMS",
                    &format!("CNMI reassert failed: {}", detail),
                    false,
                ));
            }
            None => match cnmi_verify.as_ref() {
                Some(Ok(response))
                    if response.ok && cnmi_store_notifications_enabled(&response.body) =>
                {
                    let reason = at_command_failure(&cnmi_query)
                        .map(|detail| format!(" after query error: {}", detail))
                        .unwrap_or_else(|| " after setting drift".into());
                    log::warn!("[main] CNMI restored{}", reason);
                    events.push(LogEvent::network(
                        "modem SMS",
                        &format!("CNMI restored{}", reason),
                        true,
                    ));
                }
                Some(result) => {
                    let detail = at_command_failure(result)
                        .unwrap_or_else(|| "unexpected notification configuration".into());
                    log::error!("[main] CNMI verification failed: {}", detail);
                    events.push(LogEvent::network(
                        "modem SMS",
                        &format!("CNMI verification failed: {}", detail),
                        false,
                    ));
                }
                None => {}
            },
        }
    }
    if let Some(detail) = at_command_failure(&clip_set) {
        log::error!("[main] CLIP reassert failed: {}", detail);
        events.push(LogEvent::network(
            "modem call",
            &format!("CLIP reassert failed: {}", detail),
            false,
        ));
    }
    events
}

#[cfg(feature = "esp32")]
fn at_command_failure(result: &Result<AtResponse, ModemError>) -> Option<String> {
    match result {
        Ok(response) if response.ok => None,
        Ok(response) => Some(
            response
                .body
                .lines()
                .find(|line| {
                    *line == "ERROR"
                        || line.starts_with("+CME ERROR:")
                        || line.starts_with("+CMS ERROR:")
                })
                .unwrap_or("modem rejected command")
                .chars()
                .take(80)
                .collect(),
        ),
        Err(error) => Some(error.to_string()),
    }
}

#[cfg(feature = "esp32")]
fn delete_consumed_slot(
    slot: &StorageSlot,
    modem: &std::sync::Arc<std::sync::Mutex<dyn ModemPort + Send>>,
    consumed_slots: &mut Vec<StorageSlot>,
    delete_error_logs: &mut Vec<(StorageSlot, u32)>,
    log: &mut LogRing,
    log_timestamp: &str,
) -> bool {
    const MAX_DELETE_ERROR_LOGS: usize = 64;
    const DELETE_ERROR_LOG_INTERVAL_MS: u32 = 60 * 60 * 1000;
    let result = {
        let mut md = lock!(modem);
        delete_verified_sms_slot(slot, &mut *md)
    };
    // A slow modem read and delete can span multiple AT commands.
    unsafe {
        // SAFETY: This resets only the current registered main task's watchdog.
        let _ = esp_idf_sys::esp_task_wdt_reset();
    }
    match result {
        Ok(VerifiedDeletion::Deleted) => {
            consumed_slots.push(slot.clone());
            delete_error_logs.retain(|(logged_slot, _)| logged_slot != slot);
            true
        }
        Ok(VerifiedDeletion::SlotChanged) => {
            consumed_slots.push(slot.clone());
            delete_error_logs.retain(|(logged_slot, _)| logged_slot != slot);
            log.push(
                LogEvent::new(
                    LogKind::Sms,
                    &format!("{}:{}", slot.mem, slot.index),
                    "delivered slot changed before deletion; new message retained",
                    false,
                )
                .at(log_timestamp),
            );
            true
        }
        Err(error) => {
            let now = now_ms();
            let logged = delete_error_logs
                .iter_mut()
                .find(|(logged_slot, _)| logged_slot == slot);
            let should_log = logged.as_ref().is_none_or(|(_, previous)| {
                elapsed_since(*previous, now) >= DELETE_ERROR_LOG_INTERVAL_MS
            });
            if should_log {
                log::error!(
                    "[main] SMS delete failed at {} slot {}: {}",
                    slot.mem,
                    slot.index,
                    error
                );
                log.push(
                    LogEvent::new(
                        LogKind::Sms,
                        &format!("{}:{}", slot.mem, slot.index),
                        &format!("delete failed; slot retained for retry: {}", error),
                        false,
                    )
                    .at(log_timestamp),
                );
                if let Some((_, previous)) = logged {
                    *previous = now;
                } else {
                    if delete_error_logs.len() >= MAX_DELETE_ERROR_LOGS {
                        delete_error_logs.remove(0);
                    }
                    delete_error_logs.push((slot.clone(), now));
                }
            }
            false
        }
    }
}

#[cfg(feature = "esp32")]
fn drain_telegram_send_events(
    rx: &std::sync::mpsc::Receiver<TelegramSendEvent>,
    log: &mut LogRing,
    clock: &LogClock,
    uptime_ms: u32,
) -> bool {
    let mut restart = false;
    loop {
        match rx.try_recv() {
            Ok(TelegramSendEvent::Log(event)) => record_event(log, clock, uptime_ms, event),
            Ok(TelegramSendEvent::Restart(event)) => {
                record_event(log, clock, uptime_ms, event);
                restart = true;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => break,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                record_event(
                    log,
                    clock,
                    uptime_ms,
                    LogEvent::network("telegram", "send worker event channel closed", false),
                );
                restart = true;
                break;
            }
        }
    }
    restart
}

#[cfg(feature = "esp32")]
fn sync_log_clock_from_modem(
    modem: &std::sync::Arc<std::sync::Mutex<dyn smsgate::modem::ModemPort + Send>>,
    clock: &mut LogClock,
    log: &mut LogRing,
    boot_ms: u32,
    log_failure: bool,
) {
    let uptime_ms = elapsed_since(boot_ms, now_ms());
    let result = {
        let mut md = lock!(modem);
        md.query_network_time()
    };
    match result {
        Ok(time) => {
            clock.sync_from_network(uptime_ms, time);
            record_event(
                log,
                clock,
                uptime_ms,
                LogEvent::system("time", &format!("synced from modem: {}", time.format())),
            );
            log::info!("[time] log clock synced from modem: {}", time.format());
        }
        Err(e) if log_failure => {
            record_event(
                log,
                clock,
                uptime_ms,
                LogEvent::new(
                    smsgate::log_ring::LogKind::System,
                    "time",
                    &format!("modem time unavailable: {}", e),
                    false,
                ),
            );
            log::warn!("[time] modem time unavailable: {}", e);
        }
        Err(e) => {
            log::debug!("[time] modem time still unavailable: {}", e);
        }
    }
}

#[cfg(feature = "esp32")]
fn recover_wifi_after_telegram_failure(
    reason: &str,
    wifi: &mut esp_idf_svc::wifi::BlockingWifi<esp_idf_svc::wifi::EspWifi<'static>>,
    log: &mut LogRing,
    log_clock: &LogClock,
    uptime_ms: u32,
    wifi_info: &str,
    wifi_ok: bool,
) {
    if !wifi_ok || wifi.is_up().unwrap_or(false) {
        return;
    }

    log::warn!(
        "[wifi] reconnecting after Telegram poll failure: {}; network={}",
        reason,
        wifi_info
    );
    record_event(
        log,
        log_clock,
        uptime_ms,
        LogEvent::network(
            "wifi",
            &format!(
                "reconnect after Telegram poll failure: {}; network={}",
                reason, wifi_info
            ),
            false,
        ),
    );

    if reconnect_wifi(wifi) {
        log::info!("[wifi] reconnected after Telegram poll failure");
        record_event(
            log,
            log_clock,
            uptime_ms,
            LogEvent::network("wifi", "reconnected after Telegram poll failure", true),
        );
        return;
    }

    log::error!(
        "[wifi] reconnect after Telegram poll failure failed: {}; network={}",
        reason,
        wifi_info
    );
    record_event(
        log,
        log_clock,
        uptime_ms,
        LogEvent::network(
            "wifi",
            &format!(
                "reconnect failed after Telegram poll failure: {}; network={}",
                reason, wifi_info
            ),
            false,
        ),
    );
}

#[cfg(feature = "esp32")]
#[allow(clippy::too_many_arguments)] // OTA uses main-owned clients and flash state synchronously.
fn handle_ota_document(
    caption: &str,
    document: &smsgate::im::InboundDocument,
    messenger: &mut dyn smsgate::im::MessageSink,
    bot_token: &str,
    log: &mut LogRing,
    log_clock: &LogClock,
    boot_ms: u32,
    tg_send_event_rx: &std::sync::mpsc::Receiver<TelegramSendEvent>,
) {
    macro_rules! drain_ota_send {
        () => {
            if drain_telegram_send_events(
                tg_send_event_rx,
                log,
                log_clock,
                elapsed_since(boot_ms, now_ms()),
            ) {
                esp_idf_hal::reset::restart();
            }
        };
    }

    log::info!(
        "[ota] document handler entered: caption_len={} file_name={} mime={} size={:?}",
        caption.len(),
        document.file_name.as_deref().unwrap_or("<none>"),
        document.mime_type.as_deref().unwrap_or("<none>"),
        document.file_size
    );
    log::debug!("[ota] document caption raw: {}", caption);
    if !smsgate::ota::is_ota_caption(caption) {
        log::info!("[ota] document ignored: caption is not /ota");
        return;
    }
    log::info!("[ota] caption accepted");
    let name = document.file_name.as_deref().unwrap_or("firmware image");
    let progress_message_id = send_ota_message(
        messenger,
        "starting",
        &smsgate::i18n::ota_starting(name, document.file_size),
    );
    drain_ota_send!();
    record_event(
        log,
        log_clock,
        elapsed_since(boot_ms, now_ms()),
        LogEvent::ota("telegram", &format!("OTA started: {}", name), true),
    );

    log::info!("[ota] creating WiFi Telegram HTTP client");
    let mut http = match TelegramHttpClient::new(None) {
        Ok(http) => http,
        Err(e) => {
            log::error!("[ota] HTTP client init failed: {}", e);
            let _ = send_ota_message(
                messenger,
                "http_init_failed",
                &smsgate::i18n::ota_failed(&e.to_string()),
            );
            drain_ota_send!();
            record_event(
                log,
                log_clock,
                elapsed_since(boot_ms, now_ms()),
                LogEvent::ota("telegram", &format!("OTA HTTP init failed: {}", e), false),
            );
            return;
        }
    };

    const OTA_PROGRESS_STEP_BYTES: usize = 128 * 1024;
    let mut last_reported = 0usize;
    log::info!("[ota] starting Telegram document update");
    let result =
        smsgate::ota::perform_telegram_update(&mut http, bot_token, document, |written, total| {
            let complete = total.is_some_and(|total| written >= total);
            if complete || written.saturating_sub(last_reported) >= OTA_PROGRESS_STEP_BYTES {
                last_reported = written;
                log::info!(
                    "[ota] progress report: written={} total={:?}",
                    written,
                    total
                );
                if let Some(message_id) = progress_message_id {
                    edit_ota_message(
                        messenger,
                        "progress",
                        message_id,
                        &smsgate::i18n::ota_progress(written, total),
                    );
                    drain_ota_send!();
                }
            }
        });

    match result {
        Ok(()) => {
            log::info!("[ota] update complete; notifying and restarting");
            if let Some(message_id) = progress_message_id {
                edit_ota_message(
                    messenger,
                    "complete",
                    message_id,
                    smsgate::i18n::ota_complete(),
                );
            } else {
                let _ = send_ota_message(messenger, "complete", smsgate::i18n::ota_complete());
            }
            drain_ota_send!();
            record_event(
                log,
                log_clock,
                elapsed_since(boot_ms, now_ms()),
                LogEvent::ota("telegram", "OTA complete; rebooting", true),
            );
            std::thread::sleep(std::time::Duration::from_millis(500));
            esp_idf_hal::reset::restart();
        }
        Err(e) => {
            log::error!("[ota] update failed: {}", e);
            let failed = smsgate::i18n::ota_failed(&e.to_string());
            if let Some(message_id) = progress_message_id {
                edit_ota_message(messenger, "failed", message_id, &failed);
            } else {
                let _ = send_ota_message(messenger, "failed", &failed);
            }
            drain_ota_send!();
            record_event(
                log,
                log_clock,
                elapsed_since(boot_ms, now_ms()),
                LogEvent::ota("telegram", &format!("OTA failed: {}", e), false),
            );
        }
    }
}

#[cfg(feature = "esp32")]
fn send_ota_message(
    messenger: &mut dyn smsgate::im::MessageSink,
    stage: &str,
    text: &str,
) -> Option<smsgate::im::MessageId> {
    match messenger.send_message(text) {
        Ok(message_id) => {
            log::info!(
                "[ota] Telegram notify sent: stage={} message_id={}",
                stage,
                message_id
            );
            Some(message_id)
        }
        Err(e) => {
            log::warn!("[ota] Telegram notify failed: stage={} error={}", stage, e);
            None
        }
    }
}

#[cfg(feature = "esp32")]
fn edit_ota_message(
    messenger: &mut dyn smsgate::im::MessageSink,
    stage: &str,
    message_id: smsgate::im::MessageId,
    text: &str,
) {
    match messenger.edit_message(message_id, text) {
        Ok(()) => log::info!(
            "[ota] Telegram notify edited: stage={} message_id={}",
            stage,
            message_id
        ),
        Err(e) => log::warn!(
            "[ota] Telegram notify edit failed: stage={} message_id={} error={}",
            stage,
            message_id,
            e
        ),
    }
}

#[cfg(feature = "esp32")]
fn fmt_network(wifi_ok: bool, rssi: Option<i32>, ssid: &str) -> String {
    if !wifi_ok {
        return smsgate::i18n::wifi_unavailable().to_string();
    }
    match rssi {
        Some(r) => format!("{} ({} dBm)", ssid, r),
        None => format!("{} (--)", ssid),
    }
}

#[cfg(feature = "esp32")]
fn now_ms() -> u32 {
    (esp_idf_svc::systime::EspSystemTime.now().as_millis() & 0xFFFF_FFFF) as u32
}

#[cfg(feature = "esp32")]
const LOW_HEAP_THRESHOLD: u32 = 20 * 1024;

#[cfg(feature = "esp32")]
fn check_low_heap() -> Option<String> {
    // SAFETY: The ESP-IDF getter returns a scalar heap statistic.
    let free = unsafe { esp_idf_sys::esp_get_free_heap_size() };
    if free < LOW_HEAP_THRESHOLD {
        Some(smsgate::i18n::low_heap(free))
    } else {
        None
    }
}

#[cfg(feature = "esp32")]
fn setup_wifi(
    wifi: &mut esp_idf_svc::wifi::BlockingWifi<esp_idf_svc::wifi::EspWifi<'static>>,
    ssid: &str,
    pass: &str,
) -> anyhow::Result<()> {
    use std::time::Duration;

    configure_wifi(wifi, ssid, pass)?;

    const ATTEMPTS: u32 = 5;
    for attempt in 1..=ATTEMPTS {
        if reconnect_wifi(wifi) {
            log::info!("[wifi] connected (attempt {}/{})", attempt, ATTEMPTS);
            return Ok(());
        }
        log::warn!("[wifi] attempt {}/{} failed", attempt, ATTEMPTS);
        let _ = wifi.disconnect();
        if attempt < ATTEMPTS {
            std::thread::sleep(Duration::from_secs(3));
        }
    }
    anyhow::bail!("WiFi failed after {} attempts", ATTEMPTS);
}

#[cfg(feature = "esp32")]
fn configure_wifi(
    wifi: &mut esp_idf_svc::wifi::BlockingWifi<esp_idf_svc::wifi::EspWifi<'static>>,
    ssid: &str,
    pass: &str,
) -> anyhow::Result<()> {
    use esp_idf_svc::wifi::{AuthMethod, ClientConfiguration, Configuration};

    let config = Configuration::Client(ClientConfiguration {
        ssid: ssid
            .try_into()
            .map_err(|_| anyhow::anyhow!("SSID too long"))?,
        password: pass
            .try_into()
            .map_err(|_| anyhow::anyhow!("Password too long"))?,
        auth_method: AuthMethod::WPA2Personal,
        ..Default::default()
    });
    wifi.set_configuration(&config)?;
    wifi.start()?;
    Ok(())
}

#[cfg(feature = "esp32")]
fn start_wifi_once(
    wifi: &mut esp_idf_svc::wifi::BlockingWifi<esp_idf_svc::wifi::EspWifi<'static>>,
    ssid: &str,
    pass: &str,
) -> bool {
    match configure_wifi(wifi, ssid, pass) {
        Ok(()) => reconnect_wifi(wifi),
        Err(error) => {
            log::warn!("[wifi] start failed: {}", error);
            false
        }
    }
}

/// Reconnect an already-started BlockingWifi that has lost its AP association.
/// Does not call start() or set_configuration() — assumes the driver is already
/// running with the correct config from the initial setup_wifi() call.
#[cfg(feature = "esp32")]
fn reconnect_wifi(
    wifi: &mut esp_idf_svc::wifi::BlockingWifi<esp_idf_svc::wifi::EspWifi<'static>>,
) -> bool {
    if wifi.is_connected().unwrap_or(false) {
        let _ = wifi.disconnect();
    }
    if wifi.connect().is_ok() && wifi.wait_netif_up().is_ok() {
        log::info!("[wifi] reconnect OK");
        true
    } else {
        log::warn!("[wifi] reconnect attempt failed");
        false
    }
}

/// Interactive serial setup on first boot (or after NVS erase).
/// Prompts for credentials over the serial console, writes them to NVS,
/// then reboots so the device starts normally with the new credentials.
/// Never returns.
#[cfg(feature = "esp32")]
fn serial_provision(nvs_partition: &esp_idf_svc::nvs::EspDefaultNvsPartition) -> ! {
    use std::io::BufRead;

    println!("\n\n=== smsgate first-boot setup ===");
    println!("Enter each field and press Enter. Leave blank to accept the compile-time default (if any).\n");

    let read = || -> String {
        let mut s = String::new();
        std::io::stdin().lock().read_line(&mut s).ok();
        s.trim().to_string()
    };

    let mut creds = RuntimeConfig::default();

    println!("WiFi SSID:");
    let value = read();
    if !value.is_empty() {
        creds.wifi_ssid = value;
    }
    println!("WiFi Password:");
    let value = read();
    if !value.is_empty() {
        creds.wifi_pass = value;
    }
    println!("Telegram Bot Token:");
    let value = read();
    if !value.is_empty() {
        creds.bot_token = value;
    }
    println!("Telegram Chat ID (integer):");
    let value = read();
    if !value.is_empty() {
        creds.chat_id = value.parse().unwrap_or(0);
    }
    println!("SIM PIN (4-8 digits, leave blank if disabled):");
    let value = read();
    if !value.is_empty() {
        creds.sim_pin = value;
    }
    println!("Max Telegram send failures before reboot:");
    let value = read();
    if let Ok(v) = value.parse() {
        creds.max_failures_before_reboot = v;
    }
    println!("Telegram poll interval in milliseconds:");
    let value = read();
    if let Ok(v) = value.parse() {
        creds.poll_interval_ms = v;
    }
    if creds.save(nvs_partition).is_ok() {
        println!("\nRuntime config saved. Rebooting…");
    } else {
        println!("\nERROR: NVS write failed. Rebooting — please try again.");
    }

    std::thread::sleep(std::time::Duration::from_millis(300));
    esp_idf_hal::reset::restart();
}
