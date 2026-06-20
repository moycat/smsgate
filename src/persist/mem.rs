//! In-memory Store implementation — used in tests.

use super::{Store, StoreError};
use std::collections::HashMap;

/// In-memory key-value store. Each test gets a fresh instance.
#[derive(Default, Debug, Clone)]
pub struct MemStore {
    data: HashMap<String, Vec<u8>>,
}

impl MemStore {
    pub fn new() -> Self {
        MemStore {
            data: HashMap::new(),
        }
    }
}

impl Store for MemStore {
    fn load(&self, key: &str) -> Option<Vec<u8>> {
        self.data.get(key).cloned()
    }

    fn save(&mut self, key: &str, data: &[u8]) -> Result<(), StoreError> {
        self.data.insert(key.to_string(), data.to_vec());
        Ok(())
    }

    fn delete(&mut self, key: &str) -> Result<(), StoreError> {
        self.data.remove(key);
        Ok(())
    }

    fn clear_all(&mut self) -> Result<(), StoreError> {
        self.data.clear();
        Ok(())
    }
}
