use nostr_sdk::prelude::PublicKey;
use parking_lot::RwLock;
use std::path::Path;
use std::sync::Arc;
use tracing::{info, warn};

const BLACKLIST_FILE: &str = "blacklist.json";

/// Thread-safe blacklist of pubkeys that overrides whitelist entries.
#[derive(Debug, Clone)]
pub struct Blacklist {
    inner: Arc<RwLock<Vec<PublicKey>>>,
}

impl Blacklist {
    /// Load blacklist from persisted file, or create empty.
    pub fn new(config_dir: Option<&Path>) -> Self {
        let mut pubkeys = Vec::new();

        if let Some(dir) = config_dir {
            let path = dir.join(BLACKLIST_FILE);
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
                                "Loaded {} blacklisted pubkeys from {}",
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

    /// Check if a pubkey is blacklisted.
    pub fn contains(&self, pk: &PublicKey) -> bool {
        self.inner.read().contains(pk)
    }

    /// List all blacklisted pubkeys.
    pub fn list(&self) -> Vec<PublicKey> {
        self.inner.read().clone()
    }

    /// Add a pubkey to the blacklist. Returns true if added (not duplicate).
    pub fn add(&self, pk: PublicKey) -> bool {
        let mut guard = self.inner.write();
        if guard.contains(&pk) {
            return false;
        }
        guard.push(pk);
        true
    }

    /// Remove a pubkey from the blacklist. Returns true if removed.
    pub fn remove(&self, pk: &PublicKey) -> bool {
        let mut guard = self.inner.write();
        let len_before = guard.len();
        guard.retain(|p| p != pk);
        guard.len() < len_before
    }

    /// Number of blacklisted pubkeys.
    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    /// Persist to config/blacklist.json.
    pub fn persist(&self, config_dir: &Path) -> Result<(), std::io::Error> {
        let hex_keys: Vec<String> = self.inner.read().iter().map(|pk| pk.to_hex()).collect();
        let json = serde_json::to_string_pretty(&hex_keys)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let path = config_dir.join(BLACKLIST_FILE);
        std::fs::write(&path, json)?;
        info!(
            "Persisted {} blacklist entries to {}",
            hex_keys.len(),
            path.display()
        );
        Ok(())
    }
}
