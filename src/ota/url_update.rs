//! WiFi HTTP(S) download-to-flash adapter.

use super::download::{
    expected_size, open_download, stream_download, DownloadError, DownloadUrl, EspDownloadClient,
};
use super::{next_update_slot_size, OtaError};
use esp_idf_svc::ota::EspOta;

/// Fetch a firmware URL and switch the boot slot only after full image validation.
pub fn perform_url_update<F>(url: &DownloadUrl, mut on_progress: F) -> Result<(), OtaError>
where
    F: FnMut(usize, Option<usize>),
{
    let slot_size = next_update_slot_size()?;
    let mut client = EspDownloadClient::new();
    // Resolve redirects and validate the response before erasing the inactive slot.
    let mut response = open_download(&mut client, url).map_err(download_error)?;
    let total = expected_size(&response, slot_size).map_err(download_error)?;
    log::info!(
        "[ota] URL response ready: size={:?} slot_size={}",
        total,
        slot_size
    );

    let mut ota = EspOta::new().map_err(|error| OtaError::Flash(error.to_string()))?;
    let mut update = match total {
        Some(size) => ota.initiate_update_with_known_size(size),
        None => ota.initiate_update(),
    }
    .map_err(|error| OtaError::Flash(format!("initiate: {error}")))?;

    let mut written = 0usize;
    stream_download(&mut response, slot_size, |chunk| {
        update
            .write(chunk)
            .map_err(|error| DownloadError::Write(error.to_string()))?;
        written += chunk.len();
        // SAFETY: ESP-IDF resets only the current task's watchdog subscription.
        unsafe {
            let _ = esp_idf_sys::esp_task_wdt_reset();
        }
        on_progress(written, total);
        Ok(())
    })
    .map_err(download_error)?;

    // EspOtaUpdate aborts on drop if any download or write failed. complete()
    // verifies the ESP image and changes the boot slot only after success.
    update
        .complete()
        .map_err(|error| OtaError::Flash(format!("complete: {error}")))?;
    log::info!("[ota] URL update flashed {} bytes", written);
    Ok(())
}

fn download_error(error: DownloadError) -> OtaError {
    match error {
        DownloadError::Write(error) => OtaError::Flash(format!("write: {error}")),
        error => OtaError::Http(error.to_string()),
    }
}
