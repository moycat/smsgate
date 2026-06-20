//! ESP32 NVS-backed Store implementation.

use super::{Store, StoreError};
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};

/// ESP32 NVS-backed persistent store.
pub struct NvsStore {
    nvs: EspNvs<NvsDefault>,
}

impl NvsStore {
    pub fn new(partition: EspDefaultNvsPartition) -> anyhow::Result<Self> {
        let nvs = EspNvs::new(partition, "smsgate", true)?;
        Ok(NvsStore { nvs })
    }
}

impl Store for NvsStore {
    fn load(&self, key: &str) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; 8192];
        match self.nvs.get_blob(key, &mut buf) {
            Ok(Some(slice)) => Some(slice.to_vec()),
            _ => None,
        }
    }

    fn save(&mut self, key: &str, data: &[u8]) -> Result<(), StoreError> {
        self.nvs
            .set_blob(key, data)
            .map_err(|e| StoreError::Nvs(e.to_string()))
    }

    fn delete(&mut self, key: &str) -> Result<(), StoreError> {
        self.nvs
            .remove(key)
            .map(|_| ())
            .map_err(|e| StoreError::Nvs(e.to_string()))
    }

    fn clear_all(&mut self) -> Result<(), StoreError> {
        for key in &[
            super::keys::IM_CURSOR,
            super::keys::REPLY_MAP,
            super::keys::BLOCK_LIST,
            super::keys::FWD_ENABLED,
        ] {
            self.nvs
                .remove(key)
                .map(|_| ())
                .map_err(|e| StoreError::Nvs(e.to_string()))?;
        }
        Ok(())
    }
}
