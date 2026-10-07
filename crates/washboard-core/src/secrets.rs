//! Storage for basic-auth passwords.
//!
//! The project database never holds passwords. On macOS the implementation is the Keychain
//! (generic password, service [`KEYCHAIN_SERVICE`], account [`SecretKey::account`]); tests and
//! Linux use [`MemorySecretStore`]. The macOS store is `KeychainSecretStore`, which only exists
//! when building for macOS.

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

/// The macOS login Keychain, as generic passwords.
///
/// A missing item is not an error: `get` returns `None` and `delete` succeeds, so callers can
/// clear secrets unconditionally (e.g. when a server is deleted or switched to no auth).
#[cfg(target_os = "macos")]
#[derive(Debug, Default, Clone, Copy)]
pub struct KeychainSecretStore;

#[cfg(target_os = "macos")]
mod keychain {
    use security_framework::base::Error as SfError;
    use security_framework::passwords;

    use super::{KEYCHAIN_SERVICE, KeychainSecretStore, SecretError, SecretKey, SecretStore};

    /// `errSecItemNotFound` from `SecBase.h`.
    const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

    fn err(op: &str, e: SfError) -> SecretError {
        let msg = e
            .message()
            .unwrap_or_else(|| format!("OSStatus {}", e.code()));
        SecretError(format!("Keychain {op}: {msg}"))
    }

    impl SecretStore for KeychainSecretStore {
        fn get(&self, key: &SecretKey) -> Result<Option<String>, SecretError> {
            match passwords::get_generic_password(KEYCHAIN_SERVICE, &key.account()) {
                Ok(bytes) => String::from_utf8(bytes)
                    .map(Some)
                    .map_err(|_| SecretError("Keychain item is not valid UTF-8".into())),
                Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(None),
                Err(e) => Err(err("read", e)),
            }
        }

        fn set(&self, key: &SecretKey, secret: &str) -> Result<(), SecretError> {
            // Updates the item in place when it already exists.
            passwords::set_generic_password(KEYCHAIN_SERVICE, &key.account(), secret.as_bytes())
                .map_err(|e| err("write", e))
        }

        fn delete(&self, key: &SecretKey) -> Result<(), SecretError> {
            match passwords::delete_generic_password(KEYCHAIN_SERVICE, &key.account()) {
                Ok(()) => Ok(()),
                Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(()),
                Err(e) => Err(err("delete", e)),
            }
        }
    }
}
