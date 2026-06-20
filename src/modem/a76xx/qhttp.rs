//! Quectel `AT+QHTTP*` HTTPS POST for Telegram Bot API (modem-side TLS + TCP).
//!
//! Requires an active PDP context (`AT+QIACT`) on context id 1.

use super::A76xxModem;
use crate::modem::{AtTransport, ModemError, ModemPort};
use std::time::Duration;

const TELEGRAM_HOST_PATH: &str = "https://api.telegram.org";

/// Attach PDP context 1 (IPv4) using Quectel `AT+QICSGP` / `AT+QIACT`.
/// Takes `dyn ModemPort` so this can be called with any modem handle,
/// including the type-erased `Arc<Mutex<dyn ModemPort + Send>>` in `main.rs`.
pub fn attach_pdp(
    modem: &mut dyn ModemPort,
    apn: &str,
    apn_user: &str,
    apn_pass: &str,
) -> Result<(), ModemError> {
    let r = modem.send_at("+CGATT=1")?;
    if !r.ok {
        log::warn!("[qhttp] CGATT=1: {}", r.body.trim());
    }

    let _ = modem.send_at("+QIDEACT=1");

    let qicsgp = format!(
        "+QICSGP=1,1,\"{}\",\"{}\",\"{}\",1",
        escape_at_quotes(apn),
        escape_at_quotes(apn_user),
        escape_at_quotes(apn_pass)
    );
    let r = modem.send_at(&qicsgp)?;
    if !r.ok {
        return Err(ModemError::AtError(format!("QICSGP: {}", r.body)));
    }

    let r = modem.send_at("+QIACT=1")?;
    if !r.ok {
        return Err(ModemError::AtError(format!("QIACT: {}", r.body)));
    }
    log::info!("[qhttp] PDP context 1 active");
    Ok(())
}

fn escape_at_quotes(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// POST JSON to `https://api.telegram.org` + `path` (path includes `/bot…/method`).
pub fn post_json(modem: &mut A76xxModem, path: &str, json: &str) -> Result<String, ModemError> {
    match post_json_once(modem, path, json) {
        Ok(body) => Ok(body),
        Err(first) => {
            log::warn!("[qhttp] POST failed: {}; repairing PDP and retrying", first);
            repair_pdp(modem)?;
            post_json_once(modem, path, json)
        }
    }
}

fn post_json_once(modem: &mut A76xxModem, path: &str, json: &str) -> Result<String, ModemError> {
    let url = format!("{}{}", TELEGRAM_HOST_PATH, path);

    // Bind HTTP stack to PDP context 1; enable TLS for HTTPS.
    let _ = modem.send_at("+QHTTPCFG=\"contextid\",1");
    let _ = modem.send_at("+QHTTPCFG=\"responseheader\",0");
    let _ = modem.send_at("+QHTTPCFG=\"sslctxid\",1");
    let _ = modem.send_at("+QSSLCFG=\"sslversion\",1,4");
    let _ = modem.send_at("+QSSLCFG=\"seclevel\",1,2");

    let url_len = url.len();
    let cmd = format!("+QHTTPURL={},80", url_len);
    modem.port_mut().send_at_connect_payload(&cmd, &url)?;

    let body_len = json.len();
    let post_cmd = format!("+QHTTPPOST={},80,80", body_len);
    modem.port_mut().send_at_connect_payload(&post_cmd, json)?;

    std::thread::sleep(Duration::from_millis(300));

    let r = modem.send_at("+QHTTPREAD=80")?;
    let text = extract_json_object(&r.body)?;
    if !text.is_empty() {
        return Ok(text);
    }

    // Some firmware returns payload only after a second read
    let r2 = modem.send_at("+QHTTPREAD=80")?;
    extract_json_object(&r2.body)
}

fn repair_pdp(modem: &mut A76xxModem) -> Result<(), ModemError> {
    let _ = modem.send_at("+CGATT=1");
    let _ = modem.send_at("+QIDEACT=1");
    let r = modem.send_at("+QIACT=1")?;
    if r.ok {
        log::info!("[qhttp] PDP context 1 repaired");
        Ok(())
    } else {
        Err(ModemError::AtError(format!(
            "QIACT repair failed: {}",
            r.body.trim()
        )))
    }
}

fn extract_json_object(s: &str) -> Result<String, ModemError> {
    let t = s.trim();
    if let (Some(i), Some(j)) = (t.find('{'), t.rfind('}')) {
        if j >= i {
            return Ok(t[i..=j].to_string());
        }
    }
    if t.is_empty() {
        return Err(ModemError::AtError("empty HTTP body".into()));
    }
    Ok(t.to_string())
}
