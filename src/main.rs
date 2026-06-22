//! smsgate — composition root.
//!
//! `anyhow` is intentionally limited to this file: it handles the startup
//! sequence where ergonomic error propagation matters and heap allocation is
//! acceptable. All inner modules use concrete `thiserror`-derived error types.

#[cfg(feature = "esp32")]
use smsgate::{
    boards::{ta7670x::TA7670X, Board},
    bridge::{
        call_handler::CallHandler,
        poller::poll_and_dispatch,
        reply_router::ReplyRouter,
        sms_handler::{delete_sms_slot, process_pdu_hex, read_new_sms_pdu, read_stored_sms},
    },
    commands::{builtin::*, CommandRegistry},
    config::Config,
    creds::RuntimeCreds,
    im::{
        telegram::{http::TelegramHttpClient, worker::TelegramSendWorker, TelegramMessenger},
        MessageSink, MessageSource,
    },
    log_clock::LogClock,
    log_ring::LogEvent,
    log_ring::LogRing,
    modem::{
        a76xx::qhttp,
        urc::{parse_urc, Urc},
    },
    persist::nvs::NvsStore,
    sms::concat::ConcatReassembler,
    sms::sender::{DrainOutcome, SmsSender},
    timer::elapsed_since,
};

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
fn main() {
    esp_idf_sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    let starting_message = smsgate::ota::format_starting_message(Config::GIT_COMMIT);
    log::info!("{}", starting_message);
    log::info!(
        "[boot] {}",
        smsgate::ota::running_slot_summary(Config::GIT_COMMIT)
    );
    let boot_ms = now_ms();

    // ---- Board init ----
    let mut peripherals = esp_idf_hal::peripherals::Peripherals::take().unwrap();
    let board = TA7670X;
    board.init(&mut peripherals).expect("board init failed");
    let modem = board
        .build_modem_port(&mut peripherals)
        .expect("modem init failed");

    // ---- NVS store (fall back to MemStore on NVS failure) ----
    let nvs_partition = esp_idf_svc::nvs::EspDefaultNvsPartition::take().unwrap();

    // ---- Runtime credentials (NVS "smsgcfg" > compile-time defaults) ----
    let creds = RuntimeCreds::load(&nvs_partition);
    if !creds.is_provisioned() {
        log::warn!("[main] not provisioned — entering serial setup");
        serial_provision(&nvs_partition);
    }

    let nvs_failed: bool;
    let mut store: Box<dyn smsgate::persist::Store> = match NvsStore::new(nvs_partition) {
        Ok(nvs) => {
            nvs_failed = false;
            Box::new(nvs)
        }
        Err(e) => {
            nvs_failed = true;
            log::error!("[main] NVS init failed: {} — using volatile MemStore", e);
            Box::new(smsgate::persist::mem::MemStore::default())
        }
    };

    let mut log = build_log_ring();
    let mut log_clock = LogClock::new();
    record_event(
        &mut log,
        &log_clock,
        elapsed_since(boot_ms, now_ms()),
        LogEvent::system("boot", &starting_message),
    );
    sync_log_clock_from_modem(&modem, &mut log_clock, &mut log, boot_ms, true);

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
    let wifi_ok = setup_wifi(&mut wifi, &creds.wifi_ssid, &creds.wifi_pass).is_ok();
    let mut using_cellular = false;
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
        if Config::CELLULAR_FALLBACK && !creds.apn.is_empty() {
            log::info!("[main] cellular fallback: attaching PDP context");
            match qhttp::attach_pdp(
                &mut *lock!(modem),
                &creds.apn,
                &creds.apn_user,
                &creds.apn_pass,
            ) {
                Ok(()) => {
                    record_event(
                        &mut log,
                        &log_clock,
                        elapsed_since(boot_ms, now_ms()),
                        LogEvent::network("cellular", "fallback PDP attached", true),
                    );
                }
                Err(e) => {
                    record_event(
                        &mut log,
                        &log_clock,
                        elapsed_since(boot_ms, now_ms()),
                        LogEvent::network(
                            "cellular",
                            &format!("fallback attach failed: {}", e),
                            false,
                        ),
                    );
                    panic!("PDP attach failed: {}", e);
                }
            }
            using_cellular = true;
        } else {
            panic!(
                "no WiFi and no cellular fallback (set modem.cellular_fallback + modem.apn, or fix WiFi)"
            );
        }
    }
    let mut wifi = wifi; // keep WiFi driver alive; also used for reconnect on drop

    // ---- IM (Telegram) ----
    let transport_cellular =
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(using_cellular));
    let mut messenger = TelegramSendWorker::spawn(
        modem.clone(),
        creds.bot_token.clone(),
        creds.chat_id,
        transport_cellular.clone(),
    );

    // ---- Subsystems ----
    let mut sender = SmsSender::new();
    let mut router = ReplyRouter::new();
    router.load(&*store);
    let mut concat = ConcatReassembler::new();
    let mut call_handler = CallHandler::new();
    let mut modem_status = smsgate::modem::ModemStatus::default();

    // ---- Command registry ----
    // Two-pass: first pass generates help text, second bakes it into HelpCommand.
    let help_text = build_registry("").help_text();
    let registry = build_registry(&help_text);

    // Register bot commands with Telegram
    if let Err(e) = messenger.register_commands(&registry.command_list()) {
        log::warn!("[main] register_commands failed: {} — continuing", e);
    }

    // Alert if NVS init failed (now that we have a messenger to send the notification)
    if nvs_failed {
        let _ = messenger.send_message(smsgate::i18n::nvs_fail());
        record_event(
            &mut log,
            &log_clock,
            elapsed_since(boot_ms, now_ms()),
            LogEvent::system("nvs", "init failed; using volatile MemStore"),
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
    {
        let stored = {
            let mut md = lock!(modem);
            let _ = md.send_at("+CPMS=\"ME\",\"ME\",\"ME\"");
            log::info!("[main] sweeping ME storage…");
            read_stored_sms("ME", &mut *md)
        };
        for sms in stored {
            let index = sms.index;
            let mem = sms.mem.clone();
            let delete = smsgate::bridge::sms_handler::process_stored_sms(
                sms,
                &mut router,
                &mut log,
                &mut concat,
                &mut messenger,
                &mut *store,
            );
            if delete {
                let mut md = lock!(modem);
                delete_sms_slot(index, &mut *md);
            } else {
                log::warn!(
                    "[main] sweep forward failed — SMS stays at {} slot {}",
                    mem,
                    index
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
    let _ = messenger.send_message(smsgate::i18n::started());
    match smsgate::ota::confirm_running() {
        Ok(()) => log::info!("[main] OTA running slot marked valid"),
        Err(e) => log::warn!("[main] OTA confirm skipped: {}", e),
    }

    // Subscribe main task to the Task WDT (120s timeout).
    // The WDT fires if esp_task_wdt_reset() is not called within the timeout.
    unsafe {
        esp_idf_sys::esp_task_wdt_add(std::ptr::null_mut());
    }

    // ---- Telegram polling thread ----
    // Runs getUpdates (long-poll) independently so the main loop is never blocked
    // waiting for the network. The channel delivers batches of inbound messages.
    let initial_cursor =
        smsgate::persist::load_i64(&*store, smsgate::persist::keys::IM_CURSOR).unwrap_or(0);
    let (tg_tx, tg_rx) = std::sync::mpsc::channel::<Vec<smsgate::im::InboundMessage>>();
    let modem_tg = modem.clone();
    let tg_token_poll = creds.bot_token.clone();
    let tg_chat_id_poll = creds.chat_id;
    let tg_transport_cellular = transport_cellular.clone();
    std::thread::Builder::new()
        .name("tg-poll".into())
        .stack_size(16 * 1024)
        .spawn(move || {
            let mut poll_cellular = tg_transport_cellular.load(std::sync::atomic::Ordering::SeqCst);
            let mut poll_messenger = loop {
                match build_telegram_messenger(
                    poll_cellular,
                    modem_tg.clone(),
                    tg_token_poll.clone(),
                    tg_chat_id_poll,
                ) {
                    Ok(m) => break m,
                    Err(e) => {
                        log::error!("[tg-poll] messenger init failed: {}", e);
                        std::thread::sleep(std::time::Duration::from_secs(5));
                        poll_cellular =
                            tg_transport_cellular.load(std::sync::atomic::Ordering::SeqCst);
                    }
                }
            };
            let mut cursor = initial_cursor;
            const HEARTBEAT_INTERVAL: u8 = 20;
            let mut heartbeat_counter: u8 = 0;
            // Subscribe this thread to the Task WDT (same 120 s timeout as main).
            // If poll() hangs indefinitely the WDT fires and reboots the device.
            unsafe {
                esp_idf_sys::esp_task_wdt_add(std::ptr::null_mut());
            }
            loop {
                unsafe {
                    esp_idf_sys::esp_task_wdt_reset();
                }
                let desired_cellular =
                    tg_transport_cellular.load(std::sync::atomic::Ordering::SeqCst);
                if desired_cellular != poll_cellular {
                    match build_telegram_messenger(
                        desired_cellular,
                        modem_tg.clone(),
                        tg_token_poll.clone(),
                        tg_chat_id_poll,
                    ) {
                        Ok(m) => {
                            poll_messenger = m;
                            poll_cellular = desired_cellular;
                            heartbeat_counter = 0;
                            log::info!(
                                "[tg-poll] switched to {} transport",
                                if poll_cellular { "cellular" } else { "WiFi" }
                            );
                        }
                        Err(e) => {
                            log::error!("[tg-poll] transport switch failed: {}", e);
                            std::thread::sleep(std::time::Duration::from_secs(5));
                            continue;
                        }
                    }
                }
                let poll_secs = telegram_poll_timeout_secs(poll_cellular);
                match poll_messenger.poll(cursor, poll_secs) {
                    Ok(msgs) if !msgs.is_empty() => {
                        heartbeat_counter = 0;
                        cursor = msgs.iter().map(|m| m.cursor).max().unwrap_or(cursor);
                        if tg_tx.send(msgs).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {
                        heartbeat_counter += 1;
                        if heartbeat_counter >= HEARTBEAT_INTERVAL {
                            heartbeat_counter = 0;
                            if tg_tx.send(Vec::new()).is_err() {
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        log::error!("[tg-poll] error: {}", e);
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
    let mut wifi_info = fmt_network(using_cellular, wifi_ok, None, &creds.wifi_ssid);
    let mut last_tg_activity = std::time::Instant::now();
    let mut tg_stale_alerted = false;
    let mut low_signal_alerted = false;
    let mut last_operator = String::new();
    // +CMT direct delivery is two lines: header then raw PDU hex.
    // This flag is set when the header arrives so the next poll_urc() line
    // is treated as the PDU rather than a new URC.
    let mut cmt_pdu_pending = false;

    loop {
        let now = now_ms();
        let uptime_ms = elapsed_since(boot_ms, now);

        // Kick the hardware watchdog
        unsafe {
            esp_idf_sys::esp_task_wdt_reset();
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
                let _ = messenger.send_message(smsgate::i18n::resume_ok());
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
            if !log_clock.is_synced() {
                sync_log_clock_from_modem(&modem, &mut log_clock, &mut log, boot_ms, false);
            }

            // Refresh WiFi RSSI
            let rssi = if wifi_ok && !using_cellular {
                unsafe {
                    let mut ap: esp_idf_sys::wifi_ap_record_t = std::mem::zeroed();
                    if esp_idf_sys::esp_wifi_sta_get_ap_info(&mut ap) == esp_idf_sys::ESP_OK {
                        Some(ap.rssi as i32)
                    } else {
                        None
                    }
                }
            } else {
                None
            };
            wifi_info = fmt_network(using_cellular, wifi_ok, rssi, &creds.wifi_ssid);

            // Low-heap alert
            check_low_heap(&mut messenger);

            // CSQ low-signal alert (threshold: CSQ ≤ 5, roughly < −103 dBm)
            const CSQ_WEAK: u8 = 5;
            if modem_status.csq != smsgate::modem::CSQ_UNKNOWN {
                if modem_status.csq <= CSQ_WEAK && !low_signal_alerted {
                    low_signal_alerted = true;
                    let _ = messenger.send_message(&smsgate::i18n::low_signal(modem_status.csq));
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
                    let _ =
                        messenger.send_message(&smsgate::i18n::signal_restored(modem_status.csq));
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
                let _ = messenger.send_message(&smsgate::i18n::operator_changed(
                    &last_operator,
                    &modem_status.operator,
                ));
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

            // WiFi watchdog: reconnect if the station lost its association.
            // This is the primary recovery path for "device alive but Telegram dead"
            // after a router reboot or DHCP expiry.  Only attempted in WiFi mode;
            // in cellular fallback mode wifi_ok is false and wifi is not used.
            if !using_cellular && wifi_ok && !wifi.is_connected().unwrap_or(false) {
                log::warn!("[wifi] disconnected — reconnecting");
                record_event(
                    &mut log,
                    &log_clock,
                    uptime_ms,
                    LogEvent::network("wifi", "disconnected", false),
                );
                let reconnected = reconnect_wifi(&mut wifi);
                if reconnected {
                    log::info!("[wifi] reconnected OK");
                    record_event(
                        &mut log,
                        &log_clock,
                        uptime_ms,
                        LogEvent::network("wifi", "reconnected", true),
                    );
                } else {
                    log::error!("[wifi] reconnect failed — will retry next cycle");
                    record_event(
                        &mut log,
                        &log_clock,
                        uptime_ms,
                        LogEvent::network("wifi", "reconnect failed", false),
                    );
                    let switched = try_enable_cellular_fallback(
                        "WiFi disconnected",
                        &modem,
                        &creds,
                        &mut using_cellular,
                        &transport_cellular,
                    );
                    if switched {
                        wifi_info = fmt_network(using_cellular, wifi_ok, None, &creds.wifi_ssid);
                        record_event(
                            &mut log,
                            &log_clock,
                            uptime_ms,
                            LogEvent::network(
                                "cellular",
                                "fallback enabled after WiFi disconnect",
                                true,
                            ),
                        );
                    }
                }
            }

            const POLL_STALE_SECS: u64 = 300;
            let stale_secs = last_tg_activity.elapsed().as_secs();
            if stale_secs >= POLL_STALE_SECS && !tg_stale_alerted {
                tg_stale_alerted = true;
                let mins = (stale_secs / 60) as u32;
                log::warn!("[main] tg-poll stale for {} min", mins);
                record_event(
                    &mut log,
                    &log_clock,
                    uptime_ms,
                    LogEvent::network("telegram", &format!("poll stale for {} min", mins), false),
                );
                if !using_cellular {
                    let switched = try_enable_cellular_fallback(
                        "Telegram polling stale",
                        &modem,
                        &creds,
                        &mut using_cellular,
                        &transport_cellular,
                    );
                    if switched {
                        wifi_info = fmt_network(using_cellular, wifi_ok, None, &creds.wifi_ssid);
                        record_event(
                            &mut log,
                            &log_clock,
                            uptime_ms,
                            LogEvent::network(
                                "cellular",
                                "fallback enabled after Telegram poll stale",
                                true,
                            ),
                        );
                    }
                }
                let _ = messenger.send_message(&smsgate::i18n::poll_thread_stale(mins));
            }
        }

        let mut pending_sms = Vec::new();
        let mut direct_pdus = Vec::new();
        let mut call_notifications = Vec::new();

        // Poll URCs (non-blocking); hold one lock only for modem operations.
        {
            let mut md = lock!(modem);
            while let Some(urc) = md.poll_urc() {
                log::info!("[main] URC: {:?}", urc);

                // +CMT two-line protocol: header sets the flag, next line is the PDU.
                // Direct delivery has no modem slot — nothing to delete afterwards.
                if cmt_pdu_pending {
                    cmt_pdu_pending = false;
                    direct_pdus.push(urc.trim().to_string());
                    continue;
                }

                match parse_urc(&urc) {
                    Urc::NewSms { mem, index } => {
                        if let Some(stored) = read_new_sms_pdu(&mem, index, &mut *md) {
                            pending_sms.push(stored);
                        }
                    }
                    Urc::SmsDelivery => {
                        cmt_pdu_pending = true; // next poll_urc() line is the raw PDU
                    }
                    _ => {
                        if let Some(text) = call_handler.handle_urc_deferred(&urc, &mut *md) {
                            call_notifications.push(text);
                        }
                    }
                }
            }
            if let Some(text) = call_handler.tick_deferred(&mut *md) {
                call_notifications.push(text);
            }
        }

        for text in call_notifications {
            let _ = messenger.send_message(&text);
        }

        for pdu in direct_pdus {
            process_pdu_hex(
                &pdu,
                0,
                &mut router,
                &mut log,
                &mut concat,
                &mut messenger,
                &mut *store,
            );
        }

        for sms in pending_sms {
            let index = sms.index;
            let mem = sms.mem.clone();
            let delete = smsgate::bridge::sms_handler::process_stored_sms(
                sms,
                &mut router,
                &mut log,
                &mut concat,
                &mut messenger,
                &mut *store,
            );
            if delete {
                let mut md = lock!(modem);
                delete_sms_slot(index, &mut *md);
            } else {
                log::warn!(
                    "[main] forward failed — SMS stays at mem={} slot={}",
                    mem,
                    index
                );
            }
        }

        // Collect any Telegram messages delivered by the polling thread
        let tg_messages: Vec<smsgate::im::InboundMessage> = {
            let mut batch = Vec::new();
            let mut channel_active = false;
            loop {
                match tg_rx.try_recv() {
                    Ok(msgs) => {
                        channel_active = true;
                        batch.extend(msgs);
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
                tg_stale_alerted = false;
            }
            batch
        };

        // Dispatch commands and replies, update cursor in NVS
        if !tg_messages.is_empty() {
            let document_count = tg_messages.iter().filter(|m| m.document.is_some()).count();
            log::info!(
                "[main] processing Telegram batch: messages={} documents={}",
                tg_messages.len(),
                document_count
            );
            if let Some(new_cursor) = tg_messages.iter().map(|m| m.cursor).max() {
                let _ = smsgate::persist::save_i64(
                    &mut *store,
                    smsgate::persist::keys::IM_CURSOR,
                    new_cursor,
                );
                log::info!(
                    "[main] Telegram cursor persisted before dispatch: {}",
                    new_cursor
                );
            }
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
                        continue;
                    }
                    handle_ota_document(
                        &msg.text,
                        document,
                        &mut messenger,
                        &creds.bot_token,
                        using_cellular,
                        &mut log,
                        &log_clock,
                        boot_ms,
                    );
                }
            }
            let dispatch_messages: Vec<smsgate::im::InboundMessage> = tg_messages
                .iter()
                .filter(|m| m.document.is_none())
                .cloned()
                .collect();
            if !dispatch_messages.is_empty() {
                let free_heap = unsafe { esp_idf_sys::esp_get_free_heap_size() };
                match poll_and_dispatch(
                    &dispatch_messages,
                    &mut messenger,
                    &mut sender,
                    &router,
                    &registry,
                    &mut *store,
                    &log,
                    &modem_status,
                    uptime_ms,
                    free_heap,
                    &wifi_info,
                ) {
                    Ok(outcome) => {
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
                            let _ = messenger.send_message(smsgate::i18n::rebooting());
                            esp_idf_hal::reset::restart();
                        }
                    }
                    Err(e) => {
                        consecutive_failures += 1;
                        log::error!("[main] send failed ({}): {}", consecutive_failures, e);
                        if consecutive_failures >= Config::MAX_FAILURES {
                            log::error!("[main] max failures reached — rebooting");
                            esp_idf_hal::reset::restart();
                        }
                    }
                }
            }
        }

        let drain = {
            let mut md = lock!(modem);
            sender.drain_once(&mut *md)
        };

        match &drain {
            DrainOutcome::Sent { phone } => {
                let _ = messenger.send_message(&smsgate::i18n::sms_sent_ok(phone));
                record_event(
                    &mut log,
                    &log_clock,
                    uptime_ms,
                    LogEvent::user("/send", &format!("SMS sent to {}", phone), true),
                );
            }
            DrainOutcome::Dropped { phone } => {
                let _ = messenger.send_message(&smsgate::i18n::sms_failed(phone));
                record_event(
                    &mut log,
                    &log_clock,
                    uptime_ms,
                    LogEvent::user("/send", &format!("SMS failed to {}", phone), false),
                );
            }
            _ => {}
        }

        // Skip sleep when drain_once did real work (AT exchange already took ~200ms);
        // otherwise yield to keep URC latency under 100 ms without busy-looping.
        if !drain.attempted() {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
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
fn build_telegram_messenger(
    use_cellular: bool,
    modem: std::sync::Arc<std::sync::Mutex<dyn smsgate::modem::ModemPort + Send>>,
    token: String,
    chat_id: i64,
) -> anyhow::Result<TelegramMessenger> {
    if use_cellular {
        Ok(TelegramMessenger::new_modem(modem, token, chat_id))
    } else {
        Ok(TelegramMessenger::new_wifi(
            TelegramHttpClient::new(None)?,
            token,
            chat_id,
        ))
    }
}

#[cfg(feature = "esp32")]
fn telegram_poll_timeout_secs(use_cellular: bool) -> u32 {
    if use_cellular {
        5
    } else {
        (Config::POLL_INTERVAL_MS / 1000).clamp(1, 30)
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
fn try_enable_cellular_fallback(
    reason: &str,
    modem: &std::sync::Arc<std::sync::Mutex<dyn smsgate::modem::ModemPort + Send>>,
    creds: &RuntimeCreds,
    using_cellular: &mut bool,
    transport_cellular: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> bool {
    if *using_cellular {
        return true;
    }
    if !Config::CELLULAR_FALLBACK || creds.apn.is_empty() {
        log::warn!(
            "[main] cellular fallback disabled or APN empty; staying on WiFi after {}",
            reason
        );
        return false;
    }

    log::warn!("[main] enabling cellular fallback after {}", reason);
    let attach = {
        let mut md = lock!(modem);
        qhttp::attach_pdp(&mut *md, &creds.apn, &creds.apn_user, &creds.apn_pass)
    };
    if let Err(e) = attach {
        log::error!("[main] cellular fallback attach failed: {}", e);
        return false;
    }

    *using_cellular = true;
    transport_cellular.store(true, std::sync::atomic::Ordering::SeqCst);
    true
}

#[cfg(feature = "esp32")]
fn handle_ota_document(
    caption: &str,
    document: &smsgate::im::InboundDocument,
    messenger: &mut dyn smsgate::im::MessageSink,
    bot_token: &str,
    using_cellular: bool,
    log: &mut LogRing,
    log_clock: &LogClock,
    boot_ms: u32,
) {
    log::info!(
        "[ota] document handler entered: caption_len={} file_name={} mime={} size={:?} using_cellular={}",
        caption.len(),
        document.file_name.as_deref().unwrap_or("<none>"),
        document.mime_type.as_deref().unwrap_or("<none>"),
        document.file_size,
        using_cellular
    );
    log::debug!("[ota] document caption raw: {}", caption);
    if !smsgate::ota::is_ota_caption(caption) {
        log::info!("[ota] document ignored: caption is not /ota");
        return;
    }
    log::info!("[ota] caption accepted");
    if using_cellular {
        log::warn!("[ota] rejected because Telegram transport is cellular");
        let _ = send_ota_message(
            messenger,
            "wifi_required",
            smsgate::i18n::ota_wifi_required(),
        );
        record_event(
            log,
            log_clock,
            elapsed_since(boot_ms, now_ms()),
            LogEvent::ota("telegram", "OTA rejected on cellular transport", false),
        );
        return;
    }

    let name = document.file_name.as_deref().unwrap_or("firmware image");
    let progress_message_id = send_ota_message(
        messenger,
        "starting",
        &smsgate::i18n::ota_starting(name, document.file_size),
    );
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
            let complete = total.map_or(false, |total| written >= total);
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
fn fmt_network(use_cellular: bool, wifi_ok: bool, rssi: Option<i32>, ssid: &str) -> String {
    if use_cellular || !wifi_ok {
        return "cellular (Telegram via modem)".to_string();
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
fn check_low_heap(messenger: &mut dyn smsgate::im::MessageSink) {
    let free = unsafe { esp_idf_sys::esp_get_free_heap_size() };
    if free < LOW_HEAP_THRESHOLD {
        log::warn!("[main] low heap: {} bytes", free);
        let _ = messenger.send_message(&smsgate::i18n::low_heap(free));
    }
}

#[cfg(feature = "esp32")]
fn setup_wifi(
    wifi: &mut esp_idf_svc::wifi::BlockingWifi<esp_idf_svc::wifi::EspWifi<'static>>,
    ssid: &str,
    pass: &str,
) -> anyhow::Result<()> {
    use esp_idf_svc::wifi::{AuthMethod, ClientConfiguration, Configuration};
    use std::time::Duration;

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

    const ATTEMPTS: u32 = 5;
    for attempt in 1..=ATTEMPTS {
        let ok = wifi.connect().is_ok() && wifi.wait_netif_up().is_ok();
        if ok {
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

/// Reconnect an already-started BlockingWifi that has lost its AP association.
/// Does not call start() or set_configuration() — assumes the driver is already
/// running with the correct config from the initial setup_wifi() call.
#[cfg(feature = "esp32")]
fn reconnect_wifi(
    wifi: &mut esp_idf_svc::wifi::BlockingWifi<esp_idf_svc::wifi::EspWifi<'static>>,
) -> bool {
    use std::time::Duration;
    let _ = wifi.disconnect();
    std::thread::sleep(Duration::from_secs(1));
    for attempt in 1u32..=5 {
        if wifi.connect().is_ok() && wifi.wait_netif_up().is_ok() {
            log::info!("[wifi] reconnect OK (attempt {})", attempt);
            return true;
        }
        log::warn!("[wifi] reconnect attempt {}/5 failed", attempt);
        let _ = wifi.disconnect();
        if attempt < 5 {
            std::thread::sleep(Duration::from_secs(3));
        }
    }
    false
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

    println!("WiFi SSID:");
    let wifi_ssid = read();
    println!("WiFi Password:");
    let wifi_pass = read();
    println!("Telegram Bot Token:");
    let bot_token = read();
    println!("Telegram Chat ID (integer):");
    let chat_id_str = read();
    let chat_id: i64 = chat_id_str.parse().unwrap_or(0);
    println!("APN (leave blank if not using cellular fallback):");
    let apn = read();
    println!("APN Username (leave blank if none):");
    let apn_user = read();
    println!("APN Password (leave blank if none):");
    let apn_pass = read();

    let creds = smsgate::creds::RuntimeCreds {
        wifi_ssid,
        wifi_pass,
        bot_token,
        chat_id,
        apn,
        apn_user,
        apn_pass,
    };

    if creds.save(nvs_partition) {
        println!("\nCredentials saved. Rebooting…");
    } else {
        println!("\nERROR: NVS write failed. Rebooting — please try again.");
    }

    std::thread::sleep(std::time::Duration::from_millis(300));
    esp_idf_hal::reset::restart();
}
