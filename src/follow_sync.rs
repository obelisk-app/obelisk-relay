use nostr_sdk::prelude::*;
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;
use tracing::{debug, info, warn};

const FOLLOW_RELAYS: &[&str] = &[
    "wss://relay.damus.io",
    "wss://nos.lol",
    "wss://purplepag.es",
];

const WHITELIST_FOLLOWS_FILE: &str = "whitelist_follows.json";

/// Fetch kind-3 contact lists for the given reference accounts from public relays,
/// extract all followed pubkeys, and return the union.
pub async fn sync_follows(reference_pubkeys: &[PublicKey]) -> Result<Vec<PublicKey>, String> {
    if reference_pubkeys.is_empty() {
        return Ok(Vec::new());
    }

    info!(
        "Syncing follows for {} reference accounts",
        reference_pubkeys.len()
    );

    // Ensure a TLS crypto provider is available for outbound WSS connections.
    // The relay builder may install one for the main relay, but this client is independent.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let client = Client::default();

    for relay_url in FOLLOW_RELAYS {
        if let Err(e) = client.add_relay(*relay_url).await {
            warn!("Failed to add relay {}: {}", relay_url, e);
        }
    }

    client.connect().await;

    // Give relays a moment to connect
    tokio::time::sleep(Duration::from_secs(2)).await;

    let mut all_follows = HashSet::new();

    // Fetch kind-3 events for each reference account
    let filter = Filter::new()
        .kind(Kind::ContactList)
        .authors(reference_pubkeys.to_vec());

    match tokio::time::timeout(
        Duration::from_secs(15),
        client.fetch_events(filter, Duration::from_secs(10)),
    )
    .await
    {
        Ok(Ok(events)) => {
            // For each author, find the most recent kind-3 event
            let mut latest: std::collections::HashMap<PublicKey, (Timestamp, &Event)> =
                std::collections::HashMap::new();

            for event in events.iter() {
                let existing = latest.get(&event.pubkey);
                if existing.is_none() || existing.unwrap().0 < event.created_at {
                    latest.insert(event.pubkey, (event.created_at, event));
                }
            }

            for (author, (_, event)) in &latest {
                let mut count = 0usize;
                for tag in event.tags.iter() {
                    if let Some(tag_kind) = tag.as_slice().first() {
                        if tag_kind == "p" {
                            if let Some(pk_hex) = tag.as_slice().get(1) {
                                if let Ok(pk) = PublicKey::from_hex(pk_hex.as_str()) {
                                    all_follows.insert(pk);
                                    count += 1;
                                }
                            }
                        }
                    }
                }
                debug!(
                    "Reference account {} follows {} pubkeys",
                    author.to_hex(),
                    count
                );
            }
        }
        Ok(Err(e)) => {
            warn!("Failed to fetch contact lists: {}", e);
        }
        Err(_) => {
            warn!("Timeout fetching contact lists");
        }
    }

    // Disconnect cleanly
    let _ = client.disconnect().await;

    info!(
        "Follow sync complete: {} unique follows from {} reference accounts",
        all_follows.len(),
        reference_pubkeys.len()
    );

    Ok(all_follows.into_iter().collect())
}

/// Load follow-derived pubkeys from persisted file.
pub fn load_follow_derived(config_dir: &Path) -> Vec<PublicKey> {
    let path = config_dir.join(WHITELIST_FOLLOWS_FILE);
    if !path.exists() {
        return Vec::new();
    }

    match std::fs::read_to_string(&path) {
        Ok(contents) => match serde_json::from_str::<Vec<String>>(&contents) {
            Ok(hex_keys) => {
                let pubkeys: Vec<PublicKey> = hex_keys
                    .iter()
                    .filter_map(|hex| PublicKey::from_hex(hex).ok())
                    .collect();
                info!(
                    "Loaded {} follow-derived whitelist entries from {}",
                    pubkeys.len(),
                    path.display()
                );
                pubkeys
            }
            Err(e) => {
                warn!("Failed to parse {}: {}", path.display(), e);
                Vec::new()
            }
        },
        Err(e) => {
            warn!("Failed to read {}: {}", path.display(), e);
            Vec::new()
        }
    }
}

/// Persist follow-derived pubkeys to config/whitelist_follows.json.
pub fn persist_follow_derived(
    pubkeys: &[PublicKey],
    config_dir: &Path,
) -> Result<(), std::io::Error> {
    let hex_keys: Vec<String> = pubkeys.iter().map(|pk| pk.to_hex()).collect();
    let json = serde_json::to_string_pretty(&hex_keys)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    let path = config_dir.join(WHITELIST_FOLLOWS_FILE);
    std::fs::write(&path, json)?;
    info!(
        "Persisted {} follow-derived whitelist entries to {}",
        hex_keys.len(),
        path.display()
    );
    Ok(())
}
