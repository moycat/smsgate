//! OTA (Over-The-Air) firmware update support.
//!
//! ESP32 builds stream a Telegram file download directly into the inactive
//! ESP-IDF OTA slot. Host builds keep only small pure helpers for tests.

use crate::im::InboundMessage;
#[cfg(feature = "esp32")]
use crate::im::{telegram::http::TelegramHttpClient, InboundDocument};
#[cfg(feature = "esp32")]
use esp_idf_svc::ota::EspOta;
#[cfg(feature = "esp32")]
use std::convert::TryFrom;
#[cfg(feature = "esp32")]
use std::ffi::CStr;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum OtaError {
    #[cfg(feature = "esp32")]
    #[error("Telegram file has no download path")]
    MissingFilePath,
    #[cfg(feature = "esp32")]
    #[error("file size {size} exceeds OTA slot size {slot_size}")]
    ImageTooLarge { size: usize, slot_size: usize },
    #[cfg(feature = "esp32")]
    #[error("file size is too large for this platform: {0}")]
    FileSizeTooLarge(u64),
    #[cfg(feature = "esp32")]
    #[error("HTTP: {0}")]
    Http(String),
    #[cfg(feature = "esp32")]
    #[error("Flash: {0}")]
    Flash(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningSlotSummary<'a> {
    pub slot_label: &'a str,
    pub slot_state: &'a str,
    pub partition_label: &'a str,
    pub partition_address: u32,
    pub partition_size: u32,
    pub firmware_version: &'a str,
    pub firmware_released: &'a str,
    pub build_commit: &'a str,
}

pub fn format_running_slot_summary(summary: &RunningSlotSummary<'_>) -> String {
    format!(
        "running slot={} state={} partition={} offset=0x{:x} size={} version={} released={} build={}",
        summary.slot_label,
        summary.slot_state,
        summary.partition_label,
        summary.partition_address,
        summary.partition_size,
        summary.firmware_version,
        summary.firmware_released,
        summary.build_commit
    )
}

pub fn format_starting_message(firmware_version: &str, build_commit: &str) -> String {
    format!(
        "smsgate starting… version={} build={}",
        firmware_version, build_commit
    )
}

/// Returns true when a Telegram document caption requests OTA.
pub fn is_ota_caption(caption: &str) -> bool {
    let Some(first) = caption.split_whitespace().next() else {
        return false;
    };
    let command = first
        .strip_prefix('/')
        .and_then(|s| s.split('@').next())
        .unwrap_or_default();
    command == "ota"
}

/// Return the newest valid OTA document cursor in a Telegram batch.
///
/// A batch may contain stale firmware files from earlier attempts. Only the
/// newest `/ota` document should be flashed; older ones are considered
/// acknowledged by the persisted cursor and must be ignored.
pub fn latest_ota_document_cursor(messages: &[InboundMessage]) -> Option<i64> {
    messages
        .iter()
        .filter(|message| message.document.is_some() && is_ota_caption(&message.text))
        .map(|message| message.cursor)
        .max()
}

/// Mark the currently running OTA slot as valid.
#[cfg(feature = "esp32")]
pub fn confirm_running() -> Result<(), OtaError> {
    let mut ota = EspOta::new().map_err(|e| OtaError::Flash(e.to_string()))?;
    ota.mark_running_slot_valid()
        .map_err(|e| OtaError::Flash(e.to_string()))
}

/// Returns the version string of the currently running firmware.
#[cfg(feature = "esp32")]
pub fn running_version() -> String {
    match EspOta::new() {
        Ok(ota) => match ota.get_running_slot() {
            Ok(slot) => slot
                .firmware
                .map(|f| f.version.to_string())
                .unwrap_or_else(|| "unknown".into()),
            Err(_) => "unknown".into(),
        },
        Err(_) => "unknown".into(),
    }
}

/// Return a serial-friendly summary of the currently running OTA slot.
#[cfg(feature = "esp32")]
pub fn running_slot_summary(build_commit: &str) -> String {
    let (slot_label, slot_state, firmware_version, firmware_released) = match EspOta::new() {
        Ok(ota) => match ota.get_running_slot() {
            Ok(slot) => {
                let slot_label = slot.label.to_string();
                let slot_state = format!("{:?}", slot.state);
                let (firmware_version, firmware_released) = slot
                    .firmware
                    .map(|firmware| (firmware.version.to_string(), firmware.released.to_string()))
                    .unwrap_or_else(|| ("unknown".into(), "unknown".into()));
                (slot_label, slot_state, firmware_version, firmware_released)
            }
            Err(e) => (
                "unknown".into(),
                format!("error:{e}"),
                "unknown".into(),
                "unknown".into(),
            ),
        },
        Err(e) => (
            "unknown".into(),
            format!("error:{e}"),
            "unknown".into(),
            "unknown".into(),
        ),
    };

    let (partition_label, partition_address, partition_size) =
        running_partition_info().unwrap_or_else(|| ("unknown".into(), 0, 0));

    format_running_slot_summary(&RunningSlotSummary {
        slot_label: &slot_label,
        slot_state: &slot_state,
        partition_label: &partition_label,
        partition_address,
        partition_size,
        firmware_version: &firmware_version,
        firmware_released: &firmware_released,
        build_commit,
    })
}

#[cfg(feature = "esp32")]
fn running_partition_info() -> Option<(String, u32, u32)> {
    // SAFETY: ESP-IDF returns a pointer to a read-only partition descriptor
    // owned by its partition table. It is valid for the program lifetime when
    // non-null.
    let partition = unsafe { esp_idf_sys::esp_ota_get_running_partition() };
    if partition.is_null() {
        return None;
    }

    // SAFETY: Null was checked above. The label is a fixed-size
    // zero-terminated ASCII string per ESP-IDF's `esp_partition_t` contract.
    let partition = unsafe { &*partition };
    let label = unsafe {
        CStr::from_ptr(partition.label.as_ptr())
            .to_string_lossy()
            .into_owned()
    };
    Some((label, partition.address, partition.size))
}

/// Download a Telegram document over WiFi HTTPS and flash it to the next OTA slot.
#[cfg(feature = "esp32")]
pub fn perform_telegram_update<F>(
    http: &mut TelegramHttpClient,
    token: &str,
    document: &InboundDocument,
    mut on_progress: F,
) -> Result<(), OtaError>
where
    F: FnMut(usize, Option<usize>),
{
    log::info!(
        "[ota] resolving Telegram document: file_name={} document_size={:?}",
        document.file_name.as_deref().unwrap_or("<none>"),
        document.file_size
    );
    let file = http
        .get_file(token, &document.file_id)
        .map_err(|e| OtaError::Http(e.to_string()))?;
    let file_path = file.file_path.ok_or(OtaError::MissingFilePath)?;
    let expected_size = document
        .file_size
        .or(file.file_size)
        .map(usize::try_from)
        .transpose()
        .map_err(|_| OtaError::FileSizeTooLarge(document.file_size.or(file.file_size).unwrap()))?;
    log::info!(
        "[ota] Telegram file resolved: path_len={} api_size={:?} expected_size={:?}",
        file_path.len(),
        file.file_size,
        expected_size
    );

    let slot_size = next_update_slot_size()?;
    log::info!(
        "[ota] next update slot: slot_size={} expected_size={:?}",
        slot_size,
        expected_size
    );
    if let Some(size) = expected_size {
        if size > slot_size {
            log::error!(
                "[ota] image too large: size={} slot_size={}",
                size,
                slot_size
            );
            return Err(OtaError::ImageTooLarge { size, slot_size });
        }
    }

    let mut ota = EspOta::new().map_err(|e| OtaError::Flash(e.to_string()))?;
    log::info!("[ota] initiating ESP-IDF OTA update");
    let mut update = match expected_size {
        Some(size) => ota.initiate_update_with_known_size(size),
        None => ota.initiate_update(),
    }
    .map_err(|e| OtaError::Flash(format!("initiate: {}", e)))?;

    let mut written = 0usize;
    let mut flash_error = None;
    log::info!("[ota] starting download-to-flash stream");
    let download = http.download_file(token, &file_path, |chunk| {
        log::debug!(
            "[ota] writing chunk: chunk_len={} written_before={}",
            chunk.len(),
            written
        );
        if let Err(e) = update.write(chunk) {
            log::error!("[ota] flash write failed after {} bytes: {}", written, e);
            flash_error = Some(OtaError::Flash(format!("write: {}", e)));
            anyhow::bail!("flash write failed");
        }
        written += chunk.len();
        // SAFETY: The current thread is subscribed to the task watchdog in
        // main(); resetting it here keeps long flash writes from tripping WDT.
        unsafe {
            esp_idf_sys::esp_task_wdt_reset();
        }
        on_progress(written, expected_size);
        Ok(())
    });

    if let Err(e) = download {
        log::error!(
            "[ota] download-to-flash failed after {} bytes: {}",
            written,
            e
        );
        return Err(flash_error.unwrap_or_else(|| OtaError::Http(e.to_string())));
    }
    log::info!("[ota] download-to-flash stream complete: {} bytes", written);
    if let Some(size) = expected_size {
        if written != size {
            log::error!(
                "[ota] incomplete image: written={} expected={}",
                written,
                size
            );
            return Err(OtaError::Http(format!(
                "incomplete: got {} of {} bytes",
                written, size
            )));
        }
    }

    log::info!("[ota] completing OTA update");
    update
        .complete()
        .map_err(|e| OtaError::Flash(format!("complete: {}", e)))?;
    log::info!("[ota] flashed {} bytes to next slot", written);
    Ok(())
}

#[cfg(feature = "esp32")]
fn next_update_slot_size() -> Result<usize, OtaError> {
    // SAFETY: `esp_ota_get_next_update_partition` returns a pointer owned by
    // ESP-IDF's partition table. It is read-only and valid for the program's
    // lifetime when non-null.
    let partition = unsafe { esp_idf_sys::esp_ota_get_next_update_partition(std::ptr::null()) };
    if partition.is_null() {
        return Err(OtaError::Flash("next OTA partition not found".into()));
    }
    // SAFETY: Null was checked above; reading the partition descriptor is safe.
    Ok(unsafe { (*partition).size as usize })
}
