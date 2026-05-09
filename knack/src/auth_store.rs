//! Token storage backed by the OS keyring (macOS Keychain, Windows Credential
//! Manager, Linux libsecret). Multi-account support via named profiles —
//! `knack auth login --account work` writes to a different keyring entry.
//!
//! Tokens never touch disk.

use keyring::Entry;
use serde::{Deserialize, Serialize};

use crate::errors::CliError;

const DEFAULT_ACCOUNT: &str = "default";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredTokens {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix timestamp (seconds) when the access token expires.
    pub expires_at: i64,
}

/// Backend trait so tests can swap in an in-memory store.
pub trait TokenStore {
    fn load(&self, account: &str) -> Result<Option<StoredTokens>, CliError>;
    fn save(&self, account: &str, tokens: &StoredTokens) -> Result<(), CliError>;
    fn clear(&self, account: &str) -> Result<(), CliError>;
}

/// Real keyring-backed store.
#[derive(Debug, Clone)]
pub struct KeyringStore {
    pub service: String,
}

impl KeyringStore {
    pub fn new(service: impl Into<String>) -> Self {
        Self { service: service.into() }
    }

    fn entry(&self, account: &str) -> Result<Entry, CliError> {
        Entry::new(&self.service, account).map_err(CliError::from)
    }
}

impl TokenStore for KeyringStore {
    fn load(&self, account: &str) -> Result<Option<StoredTokens>, CliError> {
        let account = if account.is_empty() { DEFAULT_ACCOUNT } else { account };
        let entry = self.entry(account)?;
        match entry.get_password() {
            Ok(blob) => {
                let tokens: StoredTokens = serde_json::from_str(&blob)?;
                Ok(Some(tokens))
            }
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(CliError::from(e)),
        }
    }

    fn save(&self, account: &str, tokens: &StoredTokens) -> Result<(), CliError> {
        let account = if account.is_empty() { DEFAULT_ACCOUNT } else { account };
        let entry = self.entry(account)?;
        let blob = serde_json::to_string(tokens)?;
        entry.set_password(&blob)?;
        Ok(())
    }

    fn clear(&self, account: &str) -> Result<(), CliError> {
        let account = if account.is_empty() { DEFAULT_ACCOUNT } else { account };
        let entry = self.entry(account)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(CliError::from(e)),
        }
    }
}

/// In-memory store for tests and `--auth-token` overrides where we don't want
/// to write to the real keyring.
#[derive(Debug, Default)]
pub struct MemoryStore {
    inner: std::sync::Mutex<std::collections::HashMap<String, StoredTokens>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl TokenStore for MemoryStore {
    fn load(&self, account: &str) -> Result<Option<StoredTokens>, CliError> {
        let g = self.inner.lock().unwrap();
        Ok(g.get(account).cloned())
    }

    fn save(&self, account: &str, tokens: &StoredTokens) -> Result<(), CliError> {
        let mut g = self.inner.lock().unwrap();
        g.insert(account.to_string(), tokens.clone());
        Ok(())
    }

    fn clear(&self, account: &str) -> Result<(), CliError> {
        let mut g = self.inner.lock().unwrap();
        g.remove(account);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> StoredTokens {
        StoredTokens {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: 1_900_000_000,
        }
    }

    #[test]
    fn memory_store_round_trips() {
        let store = MemoryStore::new();
        assert!(store.load("default").unwrap().is_none());
        store.save("default", &sample()).unwrap();
        assert_eq!(store.load("default").unwrap().unwrap(), sample());
        store.clear("default").unwrap();
        assert!(store.load("default").unwrap().is_none());
    }

    #[test]
    fn memory_store_isolates_accounts() {
        let store = MemoryStore::new();
        let mut t = sample();
        store.save("work", &t).unwrap();
        t.access_token = "personal-token".into();
        store.save("personal", &t).unwrap();
        assert_eq!(store.load("work").unwrap().unwrap().access_token, "a");
        assert_eq!(
            store.load("personal").unwrap().unwrap().access_token,
            "personal-token"
        );
    }
}
