//! Key-value persistence abstraction.

pub mod mem;
#[cfg(feature = "esp32")]
pub mod nvs;

use thiserror::Error;

/// Errors from the persistence layer.
#[derive(Debug, Error)]
pub enum StoreError {
    #[error("serialisation error: {0}")]
    Serde(String),
    #[error("NVS error: {0}")]
    Nvs(String),
}

/// Abstracts key-value persistence (NVS on device; in-memory in tests).
pub trait Store {
    /// Load raw bytes for `key`. Returns None if the key is absent.
    fn load(&self, key: &str) -> Option<Vec<u8>>;
    /// Save raw bytes for `key`.
    fn save(&mut self, key: &str, data: &[u8]) -> Result<(), StoreError>;
}

// ---------------------------------------------------------------------------
// Typed helpers on top of the raw Store trait
// ---------------------------------------------------------------------------

/// NVS keys — exactly four.
pub mod keys {
    pub const IM_CURSOR: &str = "im_cursor";
    pub const REPLY_MAP: &str = "reply_map";
    pub const BLOCK_LIST: &str = "block_list";
    pub const FWD_ENABLED: &str = "fwd_enabled";
}

/// Load an i64 from the store.
pub fn load_i64(store: &dyn Store, key: &str) -> Option<i64> {
    let bytes = store.load(key)?;
    if bytes.len() < 8 {
        return None;
    }
    Some(i64::from_le_bytes(bytes[..8].try_into().ok()?))
}

/// Save an i64 to the store.
pub fn save_i64(store: &mut dyn Store, key: &str, val: i64) -> Result<(), StoreError> {
    store.save(key, &val.to_le_bytes())
}

/// Load a bool (1 byte) from the store.
pub fn load_bool(store: &dyn Store, key: &str) -> Option<bool> {
    let bytes = store.load(key)?;
    Some(bytes.first().copied().unwrap_or(0) != 0)
}

/// Save a bool to the store.
pub fn save_bool(store: &mut dyn Store, key: &str, val: bool) -> Result<(), StoreError> {
    store.save(key, &[val as u8])
}
