//! Runtime credentials — NVS-backed, with compile-time fallback.
//!
//! ## Priority
//! 1. NVS namespace `"smsgcfg"` (written by the serial provisioning flow).
//! 2. Compile-time defaults from `Config` (for the build-from-source workflow).
//!
//! Hardware pin numbers (`UART_TX`, `UART_RX`, `PWRKEY_PIN`, …) are not affected:
//! they are wired to the board and remain compile-time constants.
//!
//! ## Provisioning path
//! If `RuntimeCreds::is_provisioned()` returns `false` at boot, `main.rs` drops
//! into an interactive serial setup that writes credentials to NVS then reboots.
//! To reset credentials, erase the NVS partition:
//!   `espflash erase-region 0x9000 0x6000`

use crate::config::Config;

/// Fully-resolved runtime credentials.
#[derive(Debug, Clone)]
pub struct RuntimeCreds {
    pub wifi_ssid: String,
    pub wifi_pass: String,
    pub bot_token: String,
    pub chat_id: i64,
    pub apn: String,
    pub apn_user: String,
    pub apn_pass: String,
}

impl Default for RuntimeCreds {
    fn default() -> Self {
        RuntimeCreds {
            wifi_ssid: Config::WIFI_SSID.to_string(),
            wifi_pass: Config::WIFI_PASSWORD.to_string(),
            bot_token: Config::BOT_TOKEN.to_string(),
            chat_id: Config::CHAT_ID,
            apn: Config::MODEM_APN.to_string(),
            apn_user: Config::MODEM_APN_USER.to_string(),
            apn_pass: Config::MODEM_APN_PASS.to_string(),
        }
    }
}

impl RuntimeCreds {
    /// Minimum to operate: non-empty bot token **and** non-zero chat_id.
    /// WiFi SSID absence is allowed (cellular-only setups).
    pub fn is_provisioned(&self) -> bool {
        !self.bot_token.is_empty() && self.chat_id != 0
    }
}

// ── ESP32 NVS persistence ─────────────────────────────────────────────────────

#[cfg(feature = "esp32")]
const CREDS_NS: &str = "smsgcfg";

#[cfg(feature = "esp32")]
mod keys {
    pub const WIFI_SSID: &str = "wifi_ssid";
    pub const WIFI_PASS: &str = "wifi_pass";
    pub const BOT_TOKEN: &str = "bot_token";
    pub const CHAT_ID: &str = "chat_id";
    pub const APN: &str = "apn";
    pub const APN_USER: &str = "apn_user";
    pub const APN_PASS: &str = "apn_pass";
}

#[cfg(feature = "esp32")]
impl RuntimeCreds {
    /// Load from NVS, falling back to compile-time defaults for any absent key.
    pub fn load(partition: &esp_idf_svc::nvs::EspDefaultNvsPartition) -> Self {
        use esp_idf_svc::nvs::EspNvs;

        let mut c = Self::default();
        // Graceful: if the namespace cannot be opened, just use defaults.
        let Ok(nvs) = EspNvs::new(partition.clone(), CREDS_NS, true) else {
            return c;
        };

        let load = |key: &str| -> Option<String> {
            let mut buf = [0u8; 512];
            let bytes = nvs.get_blob(key, &mut buf).ok()??;
            let s = std::str::from_utf8(bytes).ok()?;
            if s.is_empty() {
                None
            } else {
                Some(s.to_string())
            }
        };

        if let Some(v) = load(keys::WIFI_SSID) {
            c.wifi_ssid = v;
        }
        if let Some(v) = load(keys::WIFI_PASS) {
            c.wifi_pass = v;
        }
        if let Some(v) = load(keys::BOT_TOKEN) {
            c.bot_token = v;
        }
        if let Some(v) = load(keys::CHAT_ID) {
            if let Ok(id) = v.parse::<i64>() {
                c.chat_id = id;
            }
        }
        if let Some(v) = load(keys::APN) {
            c.apn = v;
        }
        if let Some(v) = load(keys::APN_USER) {
            c.apn_user = v;
        }
        if let Some(v) = load(keys::APN_PASS) {
            c.apn_pass = v;
        }

        c
    }

    /// Persist all fields to NVS. Returns `true` on full success.
    pub fn save(&self, partition: &esp_idf_svc::nvs::EspDefaultNvsPartition) -> bool {
        use esp_idf_svc::nvs::EspNvs;

        let Ok(nvs) = EspNvs::new(partition.clone(), CREDS_NS, true) else {
            return false;
        };
        nvs.set_blob(keys::WIFI_SSID, self.wifi_ssid.as_bytes())
            .is_ok()
            && nvs
                .set_blob(keys::WIFI_PASS, self.wifi_pass.as_bytes())
                .is_ok()
            && nvs
                .set_blob(keys::BOT_TOKEN, self.bot_token.as_bytes())
                .is_ok()
            && nvs
                .set_blob(keys::CHAT_ID, self.chat_id.to_string().as_bytes())
                .is_ok()
            && nvs.set_blob(keys::APN, self.apn.as_bytes()).is_ok()
            && nvs
                .set_blob(keys::APN_USER, self.apn_user.as_bytes())
                .is_ok()
            && nvs
                .set_blob(keys::APN_PASS, self.apn_pass.as_bytes())
                .is_ok()
    }
}
