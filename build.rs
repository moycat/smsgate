use std::path::Path;

fn main() {
    // embuild: required for esp-idf-sys link patches
    embuild::build::CfgArgs::output_propagated("ESP_IDF").ok();
    embuild::build::LinkArgs::output_propagated("ESP_IDF").ok();

    // Instruct Cargo to rerun this script if config.toml changes.
    println!("cargo:rerun-if-changed=config.toml");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo::rustc-check-cfg=cfg(locale_zh)");

    let config_path = Path::new("config.toml");
    if !config_path.exists() {
        println!(
            "cargo:warning=config.toml not found. \
             Copy config.toml.example to config.toml and fill in your credentials."
        );
        // Emit empty-string placeholders so the crate still compiles; the
        // device will fail at runtime when it tries to connect.
        emit_empty_defaults();
        emit_git_commit();
        return;
    }

    let config_str = std::fs::read_to_string(config_path).expect("Failed to read config.toml");

    let config: toml::Table = config_str.parse().expect("config.toml is not valid TOML");

    let get = |section: &str, key: &str| -> String {
        config
            .get(section)
            .and_then(|s| s.get(key))
            .and_then(|v| {
                v.as_str()
                    .map(|s| s.to_string())
                    .or_else(|| v.as_integer().map(|i| i.to_string()))
                    .or_else(|| v.as_float().map(|f| f.to_string()))
                    .or_else(|| v.as_bool().map(|b| b.to_string()))
            })
            .unwrap_or_default()
    };

    println!("cargo:rustc-env=CFG_WIFI_SSID={}", get("wifi", "ssid"));
    println!(
        "cargo:rustc-env=CFG_WIFI_PASSWORD={}",
        get("wifi", "password")
    );
    println!("cargo:rustc-env=CFG_IM_BACKEND={}", get("im", "backend"));
    println!(
        "cargo:rustc-env=CFG_IM_BOT_TOKEN={}",
        get("im", "bot_token")
    );
    println!("cargo:rustc-env=CFG_IM_CHAT_ID={}", get("im", "chat_id"));
    println!(
        "cargo:rustc-env=CFG_MODEM_UART_TX={}",
        get("modem", "uart_tx")
    );
    println!(
        "cargo:rustc-env=CFG_MODEM_UART_RX={}",
        get("modem", "uart_rx")
    );
    println!(
        "cargo:rustc-env=CFG_MODEM_UART_BAUD={}",
        get("modem", "uart_baud")
    );
    println!(
        "cargo:rustc-env=CFG_MODEM_PWRKEY={}",
        get("modem", "pwrkey")
    );
    let cellular_data = config
        .get("modem")
        .and_then(|m| m.get("cellular_data"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    println!("cargo:rustc-env=CFG_MODEM_CELLULAR_DATA={}", cellular_data);
    let cellular_fallback = config
        .get("modem")
        .and_then(|m| m.get("cellular_fallback"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    println!(
        "cargo:rustc-env=CFG_CELLULAR_FALLBACK={}",
        cellular_fallback
    );
    println!("cargo:rustc-env=CFG_MODEM_APN={}", get("modem", "apn"));
    println!(
        "cargo:rustc-env=CFG_MODEM_APN_USER={}",
        get("modem", "apn_user")
    );
    println!(
        "cargo:rustc-env=CFG_MODEM_APN_PASS={}",
        get("modem", "apn_pass")
    );
    println!(
        "cargo:rustc-env=CFG_BRIDGE_MAX_FAILURES={}",
        get("bridge", "max_failures_before_reboot")
    );
    println!(
        "cargo:rustc-env=CFG_BRIDGE_POLL_INTERVAL_MS={}",
        get("bridge", "poll_interval_ms")
    );
    println!(
        "cargo:rustc-env=CFG_BRIDGE_WATCHDOG_SEC={}",
        get("bridge", "watchdog_timeout_sec")
    );

    if get("ui", "locale") == "zh" {
        println!("cargo:rustc-cfg=locale_zh");
    }

    emit_git_commit();
}

fn emit_git_commit() {
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=CFG_GIT_COMMIT={}", commit);
    // Rerun on branch switch (.git/HEAD) or new commit on current branch
    // (.git/refs/heads/<branch> or .git/packed-refs after gc).
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads/main");
    println!("cargo:rerun-if-changed=.git/packed-refs");
}

fn emit_empty_defaults() {
    for key in &[
        "CFG_WIFI_SSID",
        "CFG_WIFI_PASSWORD",
        "CFG_IM_BACKEND",
        "CFG_IM_BOT_TOKEN",
        "CFG_IM_CHAT_ID",
    ] {
        println!("cargo:rustc-env={}=", key);
    }
    println!("cargo:rustc-env=CFG_MODEM_UART_TX=26");
    println!("cargo:rustc-env=CFG_MODEM_UART_RX=27");
    println!("cargo:rustc-env=CFG_MODEM_UART_BAUD=115200");
    println!("cargo:rustc-env=CFG_MODEM_PWRKEY=4");
    println!("cargo:rustc-env=CFG_MODEM_CELLULAR_DATA=false");
    println!("cargo:rustc-env=CFG_CELLULAR_FALLBACK=false");
    println!("cargo:rustc-env=CFG_MODEM_APN=");
    println!("cargo:rustc-env=CFG_MODEM_APN_USER=");
    println!("cargo:rustc-env=CFG_MODEM_APN_PASS=");
    println!("cargo:rustc-env=CFG_BRIDGE_MAX_FAILURES=8");
    println!("cargo:rustc-env=CFG_BRIDGE_POLL_INTERVAL_MS=3000");
    println!("cargo:rustc-env=CFG_BRIDGE_WATCHDOG_SEC=120");
}
