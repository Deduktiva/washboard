//! Storage for basic-auth passwords.
//!
//! The project database never holds passwords. On macOS the implementation is the Keychain
//! (generic password, service [`KEYCHAIN_SERVICE`], account [`SecretKey::account`]); tests and
//! Linux use [`MemorySecretStore`].
//!
//! Planned additions (WP-PROJECT): `KeychainSecretStore` behind `cfg(target_os = "macos")`
//! using the `security-framework` crate.

use std::collections::HashMap;
use std::sync::Mutex;

use thiserror::Error;

use crate::model::{ProjectId, ServerId};

pub const KEYCHAIN_SERVICE: &str = "at.deduktiva.washboard";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SecretKey {
    pub project: ProjectId,
    pub server: ServerId,
}

impl SecretKey {
    /// Keychain account name: `<project-uuid>/<server-uuid>`.
    pub fn account(&self) -> String {
        format!("{}/{}", self.project, self.server)
    }
}

#[derive(Debug, Error)]
#[error("secret store: {0}")]
pub struct SecretError(pub String);

pub trait SecretStore: Send + Sync {
    fn get(&self, key: &SecretKey) -> Result<Option<String>, SecretError>;
    fn set(&self, key: &SecretKey, secret: &str) -> Result<(), SecretError>;
    fn delete(&self, key: &SecretKey) -> Result<(), SecretError>;
}

#[derive(Debug, Default)]
pub struct MemorySecretStore {
    inner: Mutex<HashMap<SecretKey, String>>,
}

impl MemorySecretStore {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<SecretKey, String>>, SecretError> {
        self.inner
            .lock()
            .map_err(|_| SecretError("poisoned lock".into()))
    }
}

impl SecretStore for MemorySecretStore {
    fn get(&self, key: &SecretKey) -> Result<Option<String>, SecretError> {
        Ok(self.lock()?.get(key).cloned())
    }

    fn set(&self, key: &SecretKey, secret: &str) -> Result<(), SecretError> {
        self.lock()?.insert(*key, secret.to_owned());
        Ok(())
    }

    fn delete(&self, key: &SecretKey) -> Result<(), SecretError> {
        self.lock()?.remove(key);
        Ok(())
    }
}
