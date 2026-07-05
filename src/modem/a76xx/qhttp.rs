//! Quectel `AT+QHTTP*` HTTPS POST for Telegram Bot API (modem-side TLS + TCP).
//!
//! Requires an active PDP context (`AT+QIACT`) on context id 1.

#[cfg(feature = "esp32")]
use super::A76xxModem;
#[cfg(feature = "esp32")]
use crate::modem::AtTransport;
use crate::modem::{ModemError, ModemPort};
#[cfg(feature = "esp32")]
use std::time::Duration;

#[cfg(feature = "esp32")]
const TELEGRAM_HOST_PATH: &str = "https://api.telegram.org";
#[cfg(feature = "esp32")]
const QHTTP_TIMEOUT_SECS: u16 = 30;
#[cfg(feature = "esp32")]
const QHTTP_LOCAL_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(feature = "esp32")]
const QHTTP_SETTLE_DELAY: Duration = Duration::from_millis(300);

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
    let mut out = String::with_capacity(at_quote_escaped_len(s));
    push_at_quote_escaped(&mut out, s);
    out
}

#[cfg(feature = "testing")]
pub fn escape_at_quotes_for_test(s: &str) -> String {
    escape_at_quotes(s)
}

fn at_quote_escaped_len(s: &str) -> usize {
    let mut len = s.len();
    for byte in s.bytes() {
        if byte == b'\\' || byte == b'"' {
            len += 1;
        }
    }
    len
}

fn push_at_quote_escaped(out: &mut String, s: &str) {
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            ch => out.push(ch),
        }
    }
}

/// POST JSON to `https://api.telegram.org` + `path` (path includes `/bot…/method`).
#[cfg(feature = "esp32")]
pub fn post_json(modem: &mut A76xxModem, path: &str, json: &str) -> Result<String, ModemError> {
    match post_json_once(modem, path, json) {
        Ok(body) => Ok(body),
        Err(first) => {
            log::warn!("[qhttp] POST failed: {}; repairing PDP", first);
            repair_pdp(modem).map_err(|repair| {
                ModemError::AtError(format!("{}; PDP repair failed: {}", first, repair))
            })?;
            Err(first)
        }
    }
}

#[cfg(feature = "esp32")]
fn post_json_once(modem: &mut A76xxModem, path: &str, json: &str) -> Result<String, ModemError> {
    let url = format!("{}{}", TELEGRAM_HOST_PATH, path);

    // Bind HTTP stack to PDP context 1; enable TLS for HTTPS.
    let _ = modem.send_at("+QHTTPCFG=\"contextid\",1");
    let _ = modem.send_at("+QHTTPCFG=\"responseheader\",0");
    let _ = modem.send_at("+QHTTPCFG=\"sslctxid\",1");
    let _ = modem.send_at("+QSSLCFG=\"sslversion\",1,4");
    let _ = modem.send_at("+QSSLCFG=\"seclevel\",1,2");

    let url_len = url.len();
    let cmd = qhttp_url_command(url_len);
    modem
        .port_mut()
        .send_at_connect_payload_with_timeout(&cmd, &url, QHTTP_LOCAL_TIMEOUT)?;

    let body_len = json.len();
    let post_cmd = qhttp_post_command(body_len);
    modem
        .port_mut()
        .send_at_connect_payload_with_timeout(&post_cmd, json, QHTTP_LOCAL_TIMEOUT)?;

    std::thread::sleep(QHTTP_SETTLE_DELAY);

    let read_cmd = qhttp_read_command();
    let r = modem
        .port_mut()
        .send_at_with_timeout(&read_cmd, QHTTP_LOCAL_TIMEOUT)?;
    let text = extract_json_object(&r.body)?;
    if !text.is_empty() {
        return Ok(text);
    }

    // Some firmware returns payload only after a second read
    let r2 = modem
        .port_mut()
        .send_at_with_timeout(&read_cmd, QHTTP_LOCAL_TIMEOUT)?;
    extract_json_object(&r2.body)
}

#[cfg(feature = "esp32")]
fn qhttp_url_command(url_len: usize) -> String {
    format!("+QHTTPURL={},{}", url_len, QHTTP_TIMEOUT_SECS)
}

#[cfg(feature = "esp32")]
fn qhttp_post_command(body_len: usize) -> String {
    format!(
        "+QHTTPPOST={},{},{}",
        body_len, QHTTP_TIMEOUT_SECS, QHTTP_TIMEOUT_SECS
    )
}

#[cfg(feature = "esp32")]
fn qhttp_read_command() -> String {
    format!("+QHTTPREAD={}", QHTTP_TIMEOUT_SECS)
}

#[cfg(feature = "esp32")]
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

#[cfg(feature = "esp32")]
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
