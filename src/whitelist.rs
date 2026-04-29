use crate::blacklist::Blacklist;
use nostr_sdk::prelude::PublicKey;
use parking_lot::RwLock;
use std::path::Path;
use std::sync::Arc;
use tracing::{info, warn};

const RUNTIME_FILE: &str = "whitelist_runtime.json";

/// Thread-safe shared whitelist that supports runtime modifications, persistence,
/// follow-derived entries from reference accounts, and blacklist exclusion.
#[derive(Debug, Clone)]
pub struct Whitelist {
    /// Manually added pubkeys (config + runtime overrides)
    inner: Arc<RwLock<Vec<PublicKey>>>,
    /// Follow-derived pubkeys (from reference account sync)
    follow_derived: Arc<RwLock<Vec<PublicKey>>>,
    /// Blacklist that overrides whitelist membership
    blacklist: Blacklist,
}

impl Whitelist {
    /// Create a new whitelist from initial pubkeys, merging with any persisted runtime overrides.
    pub fn new(initial: Vec<PublicKey>, config_dir: Option<&Path>, blacklist: Blacklist) -> Self {
        let mut pubkeys = initial;

        // Merge runtime overrides if they exist
        if let Some(dir) = config_dir {
            let runtime_path = dir.join(RUNTIME_FILE);
            if runtime_path.exists() {
                match std::fs::read_to_string(&runtime_path) {
                    Ok(contents) => match serde_json::from_str::<Vec<String>>(&contents) {
                        Ok(hex_keys) => {
                            for hex in &hex_keys {
                                if let Ok(pk) = PublicKey::from_hex(hex) {
                                    if !pubkeys.contains(&pk) {
                                        pubkeys.push(pk);
                                    }
                                }
                            }
                            info!(
                                "Loaded {} runtime whitelist overrides from {}",
                                hex_keys.len(),
                                runtime_path.display()
                            );
                        }
                        Err(e) => warn!("Failed to parse {}: {}", runtime_path.display(), e),
                    },
                    Err(e) => warn!("Failed to read {}: {}", runtime_path.display(), e),
                }
            }
        }

        Self {
            inner: Arc::new(RwLock::new(pubkeys)),
            follow_derived: Arc::new(RwLock::new(Vec::new())),
            blacklist,
        }
    }

    /// Check if a pubkey is in the whitelist (manual or follow-derived) and NOT blacklisted.
    pub fn contains(&self, pk: &PublicKey) -> bool {
        if self.blacklist.contains(pk) {
            return false;
        }
        self.inner.read().contains(pk) || self.follow_derived.read().contains(pk)
    }

    /// Check if the whitelist is empty (no restrictions).
    /// Both manual and follow-derived must be empty for no restriction.
    pub fn is_empty(&self) -> bool {
        self.inner.read().is_empty() && self.follow_derived.read().is_empty()
    }

    /// Return a snapshot of all whitelisted pubkeys (manual + follow-derived union), excluding blacklisted.
    pub fn list(&self) -> Vec<PublicKey> {
        let manual = self.inner.read().clone();
        let follows = self.follow_derived.read().clone();
        let mut combined = manual;
        for pk in follows {
            if !combined.contains(&pk) {
                combined.push(pk);
            }
        }
        combined.retain(|pk| !self.blacklist.contains(pk));
        combined
    }

    /// Return only the manually added pubkeys.
    pub fn list_manual(&self) -> Vec<PublicKey> {
        self.inner.read().clone()
    }

    /// Return only the follow-derived pubkeys.
    pub fn list_follow_derived(&self) -> Vec<PublicKey> {
        self.follow_derived.read().clone()
    }

    /// Get a reference to the blacklist.
    pub fn blacklist(&self) -> &Blacklist {
        &self.blacklist
    }

    /// Add a pubkey to the manual whitelist. Returns true if it was added (not already present).
    pub fn add(&self, pk: PublicKey) -> bool {
        let mut guard = self.inner.write();
        if guard.contains(&pk) {
            return false;
        }
        guard.push(pk);
        true
    }

    /// Remove a pubkey from the manual whitelist. Returns true if it was removed.
    pub fn remove(&self, pk: &PublicKey) -> bool {
        let mut guard = self.inner.write();
        let len_before = guard.len();
        guard.retain(|p| p != pk);
        guard.len() < len_before
    }

    /// Number of whitelisted pubkeys (manual + follow-derived, deduplicated, excluding blacklisted).
    pub fn len(&self) -> usize {
        self.list().len()
    }

    /// Replace the entire follow-derived set.
    pub fn set_follow_derived(&self, pubkeys: Vec<PublicKey>) {
        let mut guard = self.follow_derived.write();
        *guard = pubkeys;
    }

    /// Persist the manual whitelist to `config/whitelist_runtime.json`.
    pub fn persist(&self, config_dir: &Path) -> Result<(), std::io::Error> {
        let hex_keys: Vec<String> = self.inner.read().iter().map(|pk| pk.to_hex()).collect();
        let json = serde_json::to_string_pretty(&hex_keys)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let path = config_dir.join(RUNTIME_FILE);
        std::fs::write(&path, json)?;
        info!("Persisted {} whitelist entries to {}", hex_keys.len(), path.display());
        Ok(())
    }
}
