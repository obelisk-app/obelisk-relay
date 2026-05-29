use nostr_sdk::prelude::PublicKey;
use parking_lot::RwLock;
use std::path::Path;
use std::sync::Arc;
use tracing::{info, warn};

const REFERENCE_ACCOUNTS_FILE: &str = "reference_accounts.json";

/// Thread-safe list of reference accounts whose follows are auto-whitelisted.
#[derive(Debug, Clone)]
pub struct ReferenceAccounts {
    inner: Arc<RwLock<Vec<PublicKey>>>,
}

impl ReferenceAccounts {
    /// Load reference accounts from persisted file, or create empty.
    pub fn new(config_dir: Option<&Path>) -> Self {
        let mut pubkeys = Vec::new();

        if let Some(dir) = config_dir {
            let path = dir.join(REFERENCE_ACCOUNTS_FILE);
            if path.exists() {
                match std::fs::read_to_string(&path) {
                    Ok(contents) => match serde_json::from_str::<Vec<String>>(&contents) {
                        Ok(hex_keys) => {
                            for hex in &hex_keys {
                                if let Ok(pk) = PublicKey::from_hex(hex) {
                                    pubkeys.push(pk);
                                }
                            }
                            info!(
                                "Loaded {} reference accounts from {}",
                                pubkeys.len(),
                                path.display()
                            );
                        }
                        Err(e) => warn!("Failed to parse {}: {}", path.display(), e),
                    },
                    Err(e) => warn!("Failed to read {}: {}", path.display(), e),
                }
            }
        }

        Self {
            inner: Arc::new(RwLock::new(pubkeys)),
        }
    }

    /// List all reference accounts.
    pub fn list(&self) -> Vec<PublicKey> {
        self.inner.read().clone()
    }

    /// Add a reference account. Returns true if added (not duplicate).
    pub fn add(&self, pk: PublicKey) -> bool {
        let mut guard = self.inner.write();
        if guard.contains(&pk) {
            return false;
        }
        guard.push(pk);
        true
    }

    /// Remove a reference account. Returns true if removed.
    pub fn remove(&self, pk: &PublicKey) -> bool {
        let mut guard = self.inner.write();
        let len_before = guard.len();
        guard.retain(|p| p != pk);
        guard.len() < len_before
    }

    /// Replace all reference accounts.
    pub fn replace_all(&self, pubkeys: Vec<PublicKey>) {
        let mut guard = self.inner.write();
        *guard = pubkeys;
    }

    /// Persist to config/reference_accounts.json.
    pub fn persist(&self, config_dir: &Path) -> Result<(), std::io::Error> {
        let hex_keys: Vec<String> = self.inner.read().iter().map(|pk| pk.to_hex()).collect();
        let json = serde_json::to_string_pretty(&hex_keys)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let path = config_dir.join(REFERENCE_ACCOUNTS_FILE);
        std::fs::write(&path, json)?;
        info!(
            "Persisted {} reference accounts to {}",
            hex_keys.len(),
            path.display()
        );
        Ok(())
    }

    /// Number of reference accounts.
    pub fn len(&self) -> usize {
        self.inner.read().len()
    }
}
