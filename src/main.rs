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
        sms_handler::{handle_new_sms, process_pdu_hex, sweep_one_storage},
    },
    commands::{builtin::*, CommandRegistry},
    config::Config,
    creds::RuntimeCreds,
    im::{
        telegram::{http::TelegramHttpClient, TelegramMessenger},
        MessageSink, MessageSource,
    },
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

    log::info!("smsgate starting…");

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
    if !wifi_ok {
        log::warn!("[wifi] failed after retries");
        if Config::CELLULAR_FALLBACK && !creds.apn.is_empty() {
            log::info!("[main] cellular fallback: attaching PDP context");
            qhttp::attach_pdp(
                &mut *lock!(modem),
                &creds.apn,
                &creds.apn_user,
                &creds.apn_pass,
            )
            .expect("PDP attach failed");
        } else {
            panic!(
                "no WiFi and no cellular fallback (set modem.cellular_fallback + modem.apn, or fix WiFi)"
            );
        }
    }
    let mut wifi = wifi; // keep WiFi driver alive; also used for reconnect on drop

    // ---- IM (Telegram) ----
    let mut messenger = if wifi_ok {
        TelegramMessenger::new_wifi(
            TelegramHttpClient::new(None).expect("TLS init failed"),
            creds.bot_token.clone(),
            creds.chat_id,
        )
    } else {
        TelegramMessenger::new_modem(modem.clone(), creds.bot_token.clone(), creds.chat_id)
    };

    // ---- Subsystems ----
    let mut sender = SmsSender::new();
    let mut router = ReplyRouter::new();
    router.load(&*store);
    let mut log = LogRing::new();
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
        let mut md = lock!(modem);
        let _ = md.send_at("+CPMS=\"ME\",\"ME\",\"ME\"");
        log::info!("[main] sweeping ME storage…");
        sweep_one_storage(
            "ME",
            &mut *md,
            &mut router,
            &mut log,
            &mut concat,
            &mut messenger,
            &mut *store,
        );
    }

    log::info!("smsgate ready");
    let _ = messenger.send_message(smsgate::i18n::started());

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
    std::thread::Builder::new()
        .name("tg-poll".into())
        .stack_size(16 * 1024)
        .spawn(move || {
            let mut poll_messenger = if wifi_ok {
                TelegramMessenger::new_wifi(
                    TelegramHttpClient::new(None).expect("tg-poll: TLS init failed"),
                    tg_token_poll,
                    tg_chat_id_poll,
                )
            } else {
                TelegramMessenger::new_modem(modem_tg, tg_token_poll, tg_chat_id_poll)
            };
            let poll_secs = if wifi_ok {
                (Config::POLL_INTERVAL_MS / 1000).max(1)
            } else {
                5u32
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
    let boot_ms = now_ms();
    let mut consecutive_failures: u8 = 0;
    let mut last_status_update = now_ms();
    let mut pause_until: Option<std::time::Instant> = None;
    let mut wifi_info = fmt_wifi(wifi_ok, None, &creds.wifi_ssid);
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
            }
        }

        // Update modem status every 30 s.
        // Skip while cmt_pdu_pending: send_at() drains the UART buffer and would
        // consume the PDU line that belongs to the pending +CMT delivery.
        if elapsed_since(last_status_update, now) > 30_000 && !cmt_pdu_pending {
            modem_status = lock!(modem).update_status();
            last_status_update = now;

            // Refresh WiFi RSSI
            let rssi = if wifi_ok {
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
            wifi_info = fmt_wifi(wifi_ok, rssi, &creds.wifi_ssid);

            // Low-heap alert
            check_low_heap(&mut messenger);

            // CSQ low-signal alert (threshold: CSQ ≤ 5, roughly < −103 dBm)
            const CSQ_WEAK: u8 = 5;
            if modem_status.csq != smsgate::modem::CSQ_UNKNOWN {
                if modem_status.csq <= CSQ_WEAK && !low_signal_alerted {
                    low_signal_alerted = true;
                    let _ = messenger.send_message(&smsgate::i18n::low_signal(modem_status.csq));
                } else if modem_status.csq > CSQ_WEAK && low_signal_alerted {
                    low_signal_alerted = false;
                    let _ =
                        messenger.send_message(&smsgate::i18n::signal_restored(modem_status.csq));
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
            }
            if !modem_status.operator.is_empty() {
                last_operator.clone_from(&modem_status.operator);
            }

            // WiFi watchdog: reconnect if the station lost its association.
            // This is the primary recovery path for "device alive but Telegram dead"
            // after a router reboot or DHCP expiry.  Only attempted in WiFi mode;
            // in cellular fallback mode wifi_ok is false and wifi is not used.
            if wifi_ok && !wifi.is_connected().unwrap_or(false) {
                log::warn!("[wifi] disconnected — reconnecting");
                let reconnected = reconnect_wifi(&mut wifi);
                if reconnected {
                    log::info!("[wifi] reconnected OK");
                } else {
                    log::error!("[wifi] reconnect failed — will retry next cycle");
                }
            }

            const POLL_STALE_SECS: u64 = 300;
            let stale_secs = last_tg_activity.elapsed().as_secs();
            if stale_secs >= POLL_STALE_SECS && !tg_stale_alerted {
                tg_stale_alerted = true;
                let mins = (stale_secs / 60) as u32;
                log::warn!("[main] tg-poll stale for {} min", mins);
                let _ = messenger.send_message(&smsgate::i18n::poll_thread_stale(mins));
            }
        }

        // Poll URCs (non-blocking); hold one lock for URC + tick (avoid nested lock with tg thread).
        {
            let mut md = lock!(modem);
            while let Some(urc) = md.poll_urc() {
                log::info!("[main] URC: {:?}", urc);

                // +CMT two-line protocol: header sets the flag, next line is the PDU.
                // Direct delivery has no modem slot — nothing to delete afterwards.
                if cmt_pdu_pending {
                    cmt_pdu_pending = false;
                    process_pdu_hex(
                        urc.trim(),
                        0,
                        &mut router,
                        &mut log,
                        &mut concat,
                        &mut messenger,
                        &mut *store,
                    );
                    continue;
                }

                match parse_urc(&urc) {
                    Urc::NewSms { mem, index } => {
                        handle_new_sms(
                            &mem,
                            index,
                            &mut *md,
                            &mut router,
                            &mut log,
                            &mut concat,
                            &mut messenger,
                            &mut *store,
                        );
                    }
                    Urc::SmsDelivery => {
                        cmt_pdu_pending = true; // next poll_urc() line is the raw PDU
                    }
                    _ => {
                        call_handler.handle_urc(&urc, &mut *md, &mut messenger, &mut sender);
                    }
                }
            }
            call_handler.tick(&mut *md, &mut messenger, &mut sender);
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
                        esp_idf_hal::reset::restart();
                    }
                }
            }
            if channel_active {
                last_tg_activity = std::time::Instant::now();
                tg_stale_alerted = false;
            }
            batch
        };

        // Dispatch commands and replies, update cursor in NVS
        if !tg_messages.is_empty() {
            if let Some(new_cursor) = tg_messages.iter().map(|m| m.cursor).max() {
                let _ = smsgate::persist::save_i64(
                    &mut *store,
                    smsgate::persist::keys::IM_CURSOR,
                    new_cursor,
                );
            }
            let free_heap = unsafe { esp_idf_sys::esp_get_free_heap_size() };
            match poll_and_dispatch(
                &tg_messages,
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
                Ok((restart, maybe_pause)) => {
                    consecutive_failures = 0;
                    if let Some(mins) = maybe_pause {
                        // Cap at 1 week to prevent Duration overflow on pathological input.
                        const MAX_PAUSE_MINS: u64 = 7 * 24 * 60;
                        let secs = (mins as u64).min(MAX_PAUSE_MINS) * 60;
                        pause_until =
                            Some(std::time::Instant::now() + std::time::Duration::from_secs(secs));
                        log::info!("[main] pause timer set for {} min", mins);
                    }
                    if restart {
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

        let drain = {
            let mut md = lock!(modem);
            sender.drain_once(&mut *md)
        };

        match &drain {
            DrainOutcome::Sent { phone } => {
                let _ = messenger.send_message(&smsgate::i18n::sms_sent_ok(phone));
            }
            DrainOutcome::Dropped { phone } => {
                let _ = messenger.send_message(&smsgate::i18n::sms_failed(phone));
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
    r.register(Box::new(UnblockCommand));
    r.register(Box::new(PauseCommand));
    r.register(Box::new(ResumeCommand));
    r.register(Box::new(RestartCommand));
    r
}

#[cfg(feature = "esp32")]
fn fmt_wifi(wifi_ok: bool, rssi: Option<i32>, ssid: &str) -> String {
    if !wifi_ok {
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
