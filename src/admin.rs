use crate::follow_sync;
use crate::server::ServerState;
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use nostr_sdk::prelude::*;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path as StdPath, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tracing::{debug, info, warn};

// --- Types ---

const ADMIN_RUNTIME_FILE: &str = "admin_pubkeys_runtime.json";
const SETTINGS_LOCAL_FILE: &str = "settings.local.yml";
const SETTINGS_DEFAULT_FILE: &str = "settings.yml";
const REFERENCE_ACCOUNTS_FILE: &str = "reference_accounts.json";
const WHITELIST_RUNTIME_FILE: &str = "whitelist_runtime.json";
const WHITELIST_FOLLOWS_FILE: &str = "whitelist_follows.json";
const BLACKLIST_FILE: &str = "blacklist.json";
const SETUP_OWNER_FILE: &str = "setup_owner_pubkey.json";
const BACKUP_DIR: &str = "backups";
const BACKUP_PREFIX: &str = "config-reset-";
const CHALLENGE_TTL: std::time::Duration = std::time::Duration::from_secs(300);
const SESSION_TTL: std::time::Duration = std::time::Duration::from_secs(4 * 3600);
const BACKUP_FILE_NAMES: [&str; 7] = [
    SETTINGS_LOCAL_FILE,
    REFERENCE_ACCOUNTS_FILE,
    WHITELIST_RUNTIME_FILE,
    WHITELIST_FOLLOWS_FILE,
    ADMIN_RUNTIME_FILE,
    BLACKLIST_FILE,
    SETUP_OWNER_FILE,
];

#[derive(Clone)]
#[allow(dead_code)]
pub(crate) struct AdminState {
    admin_pubkeys: Arc<RwLock<Vec<PublicKey>>>,
    sessions: Arc<RwLock<HashMap<String, Session>>>,
    challenges: Arc<RwLock<HashMap<String, ChallengeRecord>>>,
    relay_url: String,
    config_dir: String,
}

pub(crate) struct Session {
    _pubkey: PublicKey,
    expires_at: std::time::Instant,
}

pub(crate) struct ChallengeRecord {
    _challenge: String,
    created_at: std::time::Instant,
}

#[derive(Serialize)]
struct ChallengeResponse {
    challenge: String,
}

#[derive(Deserialize)]
struct AuthRequest {
    signed_event: serde_json::Value,
}

#[derive(Serialize)]
struct AuthResponse {
    token: String,
}

#[derive(Serialize)]
struct SetupStatusResponse {
    needs_setup: bool,
    admin_count: usize,
    relay_url: String,
    whitelisted_count: usize,
    reference_account_count: usize,
    setup_owner_pubkey: Option<String>,
    setup_owner_npub: Option<String>,
}

#[derive(Deserialize)]
struct SetupRequest {
    signed_event: serde_json::Value,
    access_policy: Option<String>,
    add_owner_reference: Option<bool>,
}

#[derive(Serialize)]
struct SetupResponse {
    token: String,
    admin_pubkey: String,
    admin_npub: String,
    whitelisted_owner: bool,
    reference_owner: bool,
}

#[derive(Deserialize)]
struct ConfigResetRequest {
    confirm: String,
}

#[derive(Serialize)]
struct ConfigResetResponse {
    setup_owner_pubkey: String,
    setup_owner_npub: String,
    needs_setup: bool,
    whitelisted_count: usize,
    reference_account_count: usize,
    backup_path: String,
    message: String,
}

#[derive(Deserialize)]
struct AccessSettingsRequest {
    access_policy: String,
    pubkey_rate_limit_per_minute: Option<u32>,
    connection_rate_limit_per_minute: Option<u32>,
    global_rate_limit_per_minute: Option<u32>,
}

#[derive(Serialize)]
struct AccessSettingsResponse {
    access_policy: String,
    pubkey_rate_limit_per_minute: u32,
    connection_rate_limit_per_minute: u32,
    global_rate_limit_per_minute: u32,
    whitelisted_count: usize,
    restart_required: bool,
}

#[derive(Deserialize)]
struct StorageSettingsRequest {
    pruning_enabled: bool,
    retention_days: u32,
    prune_interval_minutes: u32,
    prune_kinds: Vec<u16>,
}

#[derive(Serialize)]
struct StorageSettingsResponse {
    db_path: String,
    db_size_bytes: u64,
    db_file_count: u64,
    pruning_enabled: bool,
    configured_pruning_enabled: bool,
    retention_days: u32,
    prune_interval_minutes: u32,
    prune_kinds: Vec<u16>,
    total_pruned: u64,
    runs: u64,
    last_run_unix: i64,
    restart_required: bool,
}

#[derive(Deserialize)]
struct ObeliskIndexSettingsRequest {
    enabled: bool,
    recent_per_group: u32,
    max_bootstrap_groups: u32,
    max_page_limit: u32,
    bootstrap_requests_per_minute: u32,
    message_requests_per_minute: u32,
    reconcile_interval_minutes: u32,
}

#[derive(Serialize)]
struct ObeliskIndexSettingsResponse {
    enabled: bool,
    active_enabled: bool,
    recent_per_group: u32,
    max_bootstrap_groups: u32,
    max_page_limit: u32,
    bootstrap_requests_per_minute: u32,
    message_requests_per_minute: u32,
    reconcile_interval_minutes: u32,
    restart_required: bool,
}

#[derive(Deserialize)]
struct RestartRelayRequest {
    confirm: String,
}

#[derive(Serialize)]
struct RestartRelayResponse {
    message: String,
    restart_in_ms: u64,
}

#[derive(Serialize)]
struct AdminPubkeyEntry {
    hex: String,
    npub: String,
    current_session: bool,
}

#[derive(Deserialize)]
struct AddAdminPubkeyRequest {
    pubkey: String,
}

#[derive(Serialize)]
struct RelayIdentityResponse {
    relay_name: String,
    relay_description: String,
    relay_url: String,
    relay_pubkey: String,
    restart_required: bool,
}

#[derive(Deserialize)]
struct RelayIdentityRequest {
    relay_name: String,
    relay_description: String,
    relay_url: String,
}

#[derive(Deserialize)]
struct RotateRelayKeyRequest {
    confirm: String,
}

#[derive(Serialize)]
struct RotateRelayKeyResponse {
    relay_pubkey: String,
    restart_required: bool,
    message: String,
}

#[derive(Serialize)]
struct BackupEntry {
    id: String,
    created_unix: u64,
    path: String,
    file_count: u64,
    size_bytes: u64,
    files: Vec<String>,
}

#[derive(Serialize)]
struct BackupDownloadFile {
    name: String,
    content: String,
}

#[derive(Serialize)]
struct BackupDownloadResponse {
    id: String,
    files: Vec<BackupDownloadFile>,
}

#[derive(Deserialize)]
struct RestoreBackupRequest {
    confirm: String,
}

#[derive(Serialize)]
struct RestoreBackupResponse {
    id: String,
    backup_before_restore_path: String,
    admin_count: usize,
    whitelisted_count: usize,
    reference_account_count: usize,
    restart_required: bool,
    message: String,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Deserialize)]
struct AddWhitelistRequest {
    pubkey: String,
}

#[derive(Serialize)]
struct WhitelistEntry {
    hex: String,
    npub: String,
}

#[derive(Serialize)]
struct GroupInfo {
    id: String,
    name: String,
    about: Option<String>,
    picture: Option<String>,
    banner: Option<String>,
    parent: Option<String>,
    channel_kind: Option<String>,
    member_count: usize,
    admin_count: usize,
    private: bool,
    closed: bool,
    broadcast: bool,
    metadata_tags: Vec<Vec<String>>,
}

#[derive(Serialize)]
struct StatsResponse {
    active_connections: usize,
    total_groups: usize,
    total_members: usize,
    whitelisted_count: usize,
    uptime_seconds: u64,
}

#[derive(Serialize)]
pub struct RelayInfoResponse {
    pub name: String,
    pub description: String,
    pub group_count: usize,
    pub supported_nips: Vec<u16>,
}

#[derive(Serialize)]
struct SessionCheckResponse {
    valid: bool,
    pubkey: Option<String>,
}

#[derive(Deserialize)]
struct AddReferenceAccountRequest {
    pubkey: String,
}

#[derive(Serialize)]
struct ReferenceAccountEntry {
    hex: String,
    npub: String,
}

#[derive(Serialize)]
struct SyncFollowsResponse {
    derived_count: usize,
    message: String,
}

#[derive(Deserialize)]
struct AddBlacklistRequest {
    pubkey: String,
}

#[derive(Serialize)]
struct BlacklistEntry {
    hex: String,
    npub: String,
}

#[derive(Serialize)]
struct EventInfo {
    id: String,
    pubkey: String,
    kind: u64,
    content: String,
    created_at: u64,
}

#[derive(Deserialize)]
struct GroupEventsQuery {
    limit: Option<usize>,
    author: Option<String>,
}

#[derive(Serialize)]
struct MemberInfo {
    pubkey: String,
    roles: Vec<String>,
}

// --- Helper: generate random hex ---

fn random_hex(bytes: usize) -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let random_bytes: Vec<u8> = (0..bytes).map(|_| rng.gen()).collect();
    random_bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// --- Auth helpers ---

fn extract_bearer_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.to_string())
}

fn validate_session(admin_state: &AdminState, headers: &HeaderMap) -> Option<PublicKey> {
    let token = extract_bearer_token(headers)?;
    let sessions = admin_state.sessions.read();
    let session = sessions.get(&token)?;
    if session.expires_at > std::time::Instant::now() {
        Some(session._pubkey)
    } else {
        None
    }
}

fn unauthorized() -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(ErrorResponse {
            error: "Unauthorized".to_string(),
        }),
    )
}

fn error_response(
    status: StatusCode,
    error: impl Into<String>,
) -> (StatusCode, Json<ErrorResponse>) {
    (
        status,
        Json(ErrorResponse {
            error: error.into(),
        }),
    )
}

fn parse_pubkey_input(input: &str) -> Result<PublicKey, (StatusCode, Json<ErrorResponse>)> {
    let trimmed = input.trim();
    if trimmed.starts_with("npub") {
        PublicKey::from_bech32(trimmed)
            .map_err(|_| error_response(StatusCode::BAD_REQUEST, "Invalid npub"))
    } else {
        PublicKey::from_hex(trimmed)
            .map_err(|_| error_response(StatusCode::BAD_REQUEST, "Invalid hex pubkey"))
    }
}

fn parse_signed_event(
    signed_event: &serde_json::Value,
) -> Result<Event, (StatusCode, Json<ErrorResponse>)> {
    let json_str = serde_json::to_string(signed_event).unwrap_or_default();
    Event::from_json(&json_str).map_err(|e| {
        warn!("Admin auth: invalid event: {}", e);
        error_response(StatusCode::BAD_REQUEST, "Invalid event")
    })
}

fn verify_auth_event(event: &Event) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    if event.kind != Kind::from(22242) {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Event must be kind 22242",
        ));
    }

    if event.verify().is_err() {
        return Err(error_response(StatusCode::BAD_REQUEST, "Invalid signature"));
    }

    Ok(())
}

fn is_admin(admin_state: &AdminState, pubkey: &PublicKey) -> bool {
    admin_state.admin_pubkeys.read().contains(pubkey)
}

fn consume_challenge(
    admin_state: &AdminState,
    event: &Event,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let challenge_tag = event
        .tags
        .iter()
        .find(|t| t.as_slice().first().map(|s| s.as_str()) == Some("challenge"));

    let challenge = match challenge_tag {
        Some(tag) => match tag.as_slice().get(1) {
            Some(c) => c.to_string(),
            None => {
                return Err(error_response(
                    StatusCode::BAD_REQUEST,
                    "Missing challenge value in tag",
                ));
            }
        },
        None => {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "Missing challenge tag",
            ));
        }
    };

    let mut challenges = admin_state.challenges.write();
    match challenges.remove(&challenge) {
        Some(record) => {
            if record.created_at.elapsed() > CHALLENGE_TTL {
                Err(error_response(StatusCode::BAD_REQUEST, "Challenge expired"))
            } else {
                Ok(())
            }
        }
        None => Err(error_response(
            StatusCode::BAD_REQUEST,
            "Unknown or already used challenge",
        )),
    }
}

fn create_session(admin_state: &AdminState, pubkey: PublicKey) -> String {
    let token = random_hex(32);
    admin_state.sessions.write().insert(
        token.clone(),
        Session {
            _pubkey: pubkey,
            expires_at: std::time::Instant::now() + SESSION_TTL,
        },
    );

    let now = std::time::Instant::now();
    admin_state
        .sessions
        .write()
        .retain(|_, v| v.expires_at > now);

    token
}

fn persist_runtime_admin_pubkeys(
    admin_pubkeys: &[PublicKey],
    config_dir: &StdPath,
) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(config_dir)?;
    let hex_keys: Vec<String> = admin_pubkeys.iter().map(|pk| pk.to_hex()).collect();
    let json = serde_json::to_string_pretty(&hex_keys)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    let path = config_dir.join(ADMIN_RUNTIME_FILE);
    std::fs::write(&path, json)?;
    info!(
        "Persisted {} runtime admin pubkeys to {}",
        hex_keys.len(),
        path.display()
    );
    Ok(())
}

fn read_yaml_scalar(config_dir: &StdPath, key: &str, default_value: &str) -> String {
    for file_name in [SETTINGS_LOCAL_FILE, SETTINGS_DEFAULT_FILE] {
        let path = config_dir.join(file_name);
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };

        for line in contents.lines() {
            let trimmed = line.trim();
            let Some(rest) = trimmed.strip_prefix(key) else {
                continue;
            };
            let Some(value) = rest.trim_start().strip_prefix(':') else {
                continue;
            };
            let value = value.trim().trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }

    default_value.to_string()
}

fn read_yaml_u32(config_dir: &StdPath, key: &str, default_value: u32) -> u32 {
    read_yaml_scalar(config_dir, key, "")
        .parse::<u32>()
        .unwrap_or(default_value)
}

fn read_obelisk_index_scalar(config_dir: &StdPath, key: &str, default_value: &str) -> String {
    for file_name in [SETTINGS_LOCAL_FILE, SETTINGS_DEFAULT_FILE] {
        let path = config_dir.join(file_name);
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let lines: Vec<&str> = contents.lines().collect();
        let Some(start) = lines
            .iter()
            .position(|line| line.trim_start() == "obelisk_index:")
        else {
            continue;
        };

        for line in lines.iter().skip(start + 1) {
            let indent = line.len().saturating_sub(line.trim_start().len());
            let trimmed = line.trim();
            if !trimmed.is_empty() && indent <= 2 && !trimmed.starts_with('#') {
                break;
            }
            let Some(rest) = trimmed.strip_prefix(key) else {
                continue;
            };
            let Some(value) = rest.trim_start().strip_prefix(':') else {
                continue;
            };
            let value = value.trim().trim_matches('"').trim_matches('\'');
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }

    default_value.to_string()
}

fn read_obelisk_index_u32(config_dir: &StdPath, key: &str, default_value: u32) -> u32 {
    read_obelisk_index_scalar(config_dir, key, "")
        .parse::<u32>()
        .unwrap_or(default_value)
}

fn read_obelisk_index_bool(config_dir: &StdPath, key: &str, default_value: bool) -> bool {
    match read_obelisk_index_scalar(
        config_dir,
        key,
        if default_value { "true" } else { "false" },
    )
    .as_str()
    {
        "true" | "True" | "TRUE" => true,
        "false" | "False" | "FALSE" => false,
        _ => default_value,
    }
}

fn yaml_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn upsert_relay_scalar(contents: String, key: &str, value: u32) -> String {
    upsert_relay_value(contents, key, &value.to_string())
}

fn upsert_relay_value(contents: String, key: &str, value: &str) -> String {
    let replacement = format!("  {key}: {value}");
    let mut found = false;
    let mut lines = Vec::new();

    for line in contents.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with(&format!("{key}:")) {
            lines.push(replacement.clone());
            found = true;
        } else {
            lines.push(line.to_string());
        }
    }

    if !found {
        let insert_at = lines
            .iter()
            .position(|line| line.trim_start().starts_with("websocket:"))
            .unwrap_or(lines.len());
        lines.insert(insert_at, replacement);
    }

    let mut next = lines.join("\n");
    next.push('\n');
    next
}

fn upsert_obelisk_index_value(contents: String, key: &str, value: &str) -> String {
    let mut lines: Vec<String> = contents.lines().map(str::to_string).collect();
    let block_start = lines
        .iter()
        .position(|line| line.trim_start() == "obelisk_index:")
        .unwrap_or_else(|| {
            let insert_at = lines
                .iter()
                .position(|line| line.trim_start().starts_with("websocket:"))
                .unwrap_or(lines.len());
            lines.insert(insert_at, "  obelisk_index:".to_string());
            insert_at
        });

    let block_end = lines
        .iter()
        .enumerate()
        .skip(block_start + 1)
        .find_map(|(idx, line)| {
            let indent = line.len().saturating_sub(line.trim_start().len());
            let trimmed = line.trim();
            (!trimmed.is_empty() && indent <= 2 && !trimmed.starts_with('#')).then_some(idx)
        })
        .unwrap_or(lines.len());

    let replacement = format!("    {key}: {value}");
    let mut found = false;
    for line in lines.iter_mut().take(block_end).skip(block_start + 1) {
        if line.trim_start().starts_with(&format!("{key}:")) {
            *line = replacement.clone();
            found = true;
            break;
        }
    }

    if !found {
        lines.insert(block_end, replacement);
    }

    let mut next = lines.join("\n");
    next.push('\n');
    next
}

fn config_bool(config_dir: &StdPath, key: &str, default_value: bool) -> bool {
    match read_yaml_scalar(
        config_dir,
        key,
        if default_value { "true" } else { "false" },
    )
    .as_str()
    {
        "true" | "True" | "TRUE" => true,
        "false" | "False" | "FALSE" => false,
        _ => default_value,
    }
}

fn parse_duration_days(value: &str, default_value: u32) -> u32 {
    let trimmed = value.trim().trim_matches('"').trim_matches('\'');
    if let Some(days) = trimmed.strip_suffix('d') {
        return days.parse::<u32>().unwrap_or(default_value);
    }
    if let Some(hours) = trimmed.strip_suffix('h') {
        return hours
            .parse::<u32>()
            .map(|h| (h / 24).max(1))
            .unwrap_or(default_value);
    }
    trimmed
        .parse::<u64>()
        .map(|seconds| ((seconds / 86_400) as u32).max(1))
        .unwrap_or(default_value)
}

fn parse_duration_minutes(value: &str, default_value: u32) -> u32 {
    let trimmed = value.trim().trim_matches('"').trim_matches('\'');
    if let Some(minutes) = trimmed.strip_suffix('m') {
        return minutes.parse::<u32>().unwrap_or(default_value);
    }
    if let Some(hours) = trimmed.strip_suffix('h') {
        return hours
            .parse::<u32>()
            .map(|h| h.saturating_mul(60))
            .unwrap_or(default_value);
    }
    trimmed
        .parse::<u64>()
        .map(|seconds| ((seconds / 60) as u32).max(1))
        .unwrap_or(default_value)
}

fn read_prune_kinds(config_dir: &StdPath) -> Vec<u16> {
    let raw = read_yaml_scalar(config_dir, "prune_kinds", "");
    if raw.is_empty() {
        return crate::pruner::DEFAULT_PRUNE_KINDS.to_vec();
    }

    let parsed: Vec<u16> = raw
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .filter_map(|item| item.trim().parse::<u16>().ok())
        .filter(|kind| !crate::pruner::NEVER_PRUNE_KINDS.contains(kind))
        .collect();

    if parsed.is_empty() {
        crate::pruner::DEFAULT_PRUNE_KINDS.to_vec()
    } else {
        parsed
    }
}

fn directory_stats(path: &StdPath) -> (u64, u64) {
    let Ok(metadata) = std::fs::metadata(path) else {
        return (0, 0);
    };

    if metadata.is_file() {
        return (metadata.len(), 1);
    }

    let mut size = 0u64;
    let mut files = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                stack.push(entry.path());
            } else if metadata.is_file() {
                size = size.saturating_add(metadata.len());
                files = files.saturating_add(1);
            }
        }
    }
    (size, files)
}

fn read_pubkey_json_file(path: &StdPath) -> Vec<PublicKey> {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(hex_keys) = serde_json::from_str::<Vec<String>>(&contents) else {
        warn!("Failed to parse pubkey list from {}", path.display());
        return Vec::new();
    };

    hex_keys
        .iter()
        .filter_map(|hex| PublicKey::from_hex(hex).ok())
        .collect()
}

fn backup_id_to_dir(
    config_dir: &StdPath,
    id: &str,
) -> Result<PathBuf, (StatusCode, Json<ErrorResponse>)> {
    if !id.starts_with(BACKUP_PREFIX)
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(error_response(StatusCode::BAD_REQUEST, "Invalid backup id"));
    }

    let path = config_dir.join(BACKUP_DIR).join(id);
    if !path.is_dir() {
        return Err(error_response(StatusCode::NOT_FOUND, "Backup not found"));
    }

    Ok(path)
}

fn backup_entry(path: &StdPath) -> Option<BackupEntry> {
    let id = path.file_name()?.to_string_lossy().to_string();
    if !id.starts_with(BACKUP_PREFIX) {
        return None;
    }

    let created_unix = id
        .strip_prefix(BACKUP_PREFIX)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_file() {
                files.push(entry.file_name().to_string_lossy().to_string());
            }
        }
    }
    files.sort();
    let (size_bytes, file_count) = directory_stats(path);

    Some(BackupEntry {
        id,
        created_unix,
        path: path.display().to_string(),
        file_count,
        size_bytes,
        files,
    })
}

fn list_backup_entries(config_dir: &StdPath) -> Vec<BackupEntry> {
    let backup_root = config_dir.join(BACKUP_DIR);
    let Ok(entries) = std::fs::read_dir(backup_root) else {
        return Vec::new();
    };

    let mut backups: Vec<BackupEntry> = entries
        .flatten()
        .filter_map(|entry| {
            let Ok(metadata) = entry.metadata() else {
                return None;
            };
            if metadata.is_dir() {
                backup_entry(&entry.path())
            } else {
                None
            }
        })
        .collect();
    backups.sort_by(|a, b| b.created_unix.cmp(&a.created_unix));
    backups
}

fn apply_runtime_config_from_files(
    state: &ServerState,
    admin_state: &AdminState,
    config_dir: &StdPath,
) {
    let restored_admins = read_pubkey_json_file(&config_dir.join(ADMIN_RUNTIME_FILE));
    *admin_state.admin_pubkeys.write() = restored_admins;

    state.whitelist.replace_manual(read_pubkey_json_file(
        &config_dir.join(WHITELIST_RUNTIME_FILE),
    ));
    state
        .whitelist
        .set_follow_derived(follow_sync::load_follow_derived(config_dir));
    state.reference_accounts.replace_all(read_pubkey_json_file(
        &config_dir.join(REFERENCE_ACCOUNTS_FILE),
    ));
    state
        .whitelist
        .blacklist()
        .replace_all(read_pubkey_json_file(&config_dir.join(BLACKLIST_FILE)));
}

fn relay_identity_response(state: &ServerState, restart_required: bool) -> RelayIdentityResponse {
    let config_dir = StdPath::new(&state.config_dir);
    RelayIdentityResponse {
        relay_name: read_yaml_scalar(config_dir, "relay_name", &state.relay_name),
        relay_description: read_yaml_scalar(
            config_dir,
            "relay_description",
            &state.relay_description,
        ),
        relay_url: read_yaml_scalar(config_dir, "relay_url", &state.relay_url),
        relay_pubkey: state.relay_pubkey.clone(),
        restart_required,
    }
}

fn persist_relay_identity_settings(
    config_dir: &StdPath,
    relay_name: &str,
    relay_description: &str,
    relay_url: &str,
) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(config_dir)?;
    let path = config_dir.join(SETTINGS_LOCAL_FILE);
    let contents = std::fs::read_to_string(&path).unwrap_or_else(|_| "relay:\n".to_string());
    let contents = upsert_relay_value(contents, "relay_name", &yaml_quote(relay_name));
    let contents = upsert_relay_value(
        contents,
        "relay_description",
        &yaml_quote(relay_description),
    );
    let contents = upsert_relay_value(contents, "relay_url", &yaml_quote(relay_url));
    std::fs::write(path, contents)
}

fn persist_relay_secret_key(
    config_dir: &StdPath,
    secret_key_hex: &str,
) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(config_dir)?;
    let path = config_dir.join(SETTINGS_LOCAL_FILE);
    let contents = std::fs::read_to_string(&path).unwrap_or_else(|_| "relay:\n".to_string());
    let contents = upsert_relay_value(contents, "relay_secret_key", &yaml_quote(secret_key_hex));
    std::fs::write(path, contents)
}

fn generate_relay_secret_key() -> (String, PublicKey) {
    loop {
        let secret_key_hex = random_hex(32);
        let Ok(secret_key) = SecretKey::from_hex(&secret_key_hex) else {
            continue;
        };
        let keys = Keys::new(secret_key);
        return (secret_key_hex, keys.public_key());
    }
}

fn storage_settings_response(
    state: &ServerState,
    restart_required: bool,
) -> StorageSettingsResponse {
    let config_dir = StdPath::new(&state.config_dir);
    let retention_days =
        parse_duration_days(&read_yaml_scalar(config_dir, "event_retention", "30d"), 30);
    let prune_interval_minutes =
        parse_duration_minutes(&read_yaml_scalar(config_dir, "prune_interval", "60m"), 60);
    let configured_pruning_enabled = config_bool(config_dir, "enable_event_pruner", false);
    let (db_size_bytes, db_file_count) = directory_stats(StdPath::new(&state.db_path));
    let (total_pruned, runs, last_run_unix) = match &state.pruner_stats {
        Some(s) => (
            s.total_pruned.load(Ordering::Relaxed),
            s.runs.load(Ordering::Relaxed),
            s.last_run_unix.load(Ordering::Relaxed),
        ),
        None => (0, 0, 0),
    };

    StorageSettingsResponse {
        db_path: state.db_path.clone(),
        db_size_bytes,
        db_file_count,
        pruning_enabled: state.pruner_config.is_some(),
        configured_pruning_enabled,
        retention_days,
        prune_interval_minutes,
        prune_kinds: read_prune_kinds(config_dir),
        total_pruned,
        runs,
        last_run_unix,
        restart_required,
    }
}

fn obelisk_index_settings_response(
    state: &ServerState,
    restart_required: bool,
) -> ObeliskIndexSettingsResponse {
    let config_dir = StdPath::new(&state.config_dir);
    let reconcile_interval_minutes = parse_duration_minutes(
        &read_obelisk_index_scalar(config_dir, "reconcile_interval", "5m"),
        5,
    );

    ObeliskIndexSettingsResponse {
        enabled: read_obelisk_index_bool(config_dir, "enabled", true),
        active_enabled: state.obelisk_index.is_some(),
        recent_per_group: read_obelisk_index_u32(config_dir, "recent_per_group", 50),
        max_bootstrap_groups: read_obelisk_index_u32(config_dir, "max_bootstrap_groups", 500),
        max_page_limit: read_obelisk_index_u32(config_dir, "max_page_limit", 100),
        bootstrap_requests_per_minute: read_obelisk_index_u32(
            config_dir,
            "bootstrap_requests_per_minute",
            30,
        ),
        message_requests_per_minute: read_obelisk_index_u32(
            config_dir,
            "message_requests_per_minute",
            120,
        ),
        reconcile_interval_minutes,
        restart_required,
    }
}

fn persist_rate_limit_settings(
    config_dir: &StdPath,
    pubkey_rate_limit: u32,
    connection_rate_limit: u32,
    global_rate_limit: u32,
) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(config_dir)?;
    let path = config_dir.join(SETTINGS_LOCAL_FILE);
    let contents = std::fs::read_to_string(&path).unwrap_or_else(|_| "relay:\n".to_string());
    let contents = upsert_relay_scalar(contents, "pubkey_rate_limit_per_minute", pubkey_rate_limit);
    let contents = upsert_relay_scalar(
        contents,
        "connection_rate_limit_per_minute",
        connection_rate_limit,
    );
    let contents = upsert_relay_scalar(contents, "global_rate_limit_per_minute", global_rate_limit);
    std::fs::write(path, contents)
}

fn persist_obelisk_index_settings(
    config_dir: &StdPath,
    req: &ObeliskIndexSettingsRequest,
) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(config_dir)?;
    let path = config_dir.join(SETTINGS_LOCAL_FILE);
    let contents = std::fs::read_to_string(&path).unwrap_or_else(|_| "relay:\n".to_string());
    let contents = upsert_obelisk_index_value(
        contents,
        "enabled",
        if req.enabled { "true" } else { "false" },
    );
    let contents = upsert_obelisk_index_value(
        contents,
        "recent_per_group",
        &req.recent_per_group.to_string(),
    );
    let contents = upsert_obelisk_index_value(
        contents,
        "max_bootstrap_groups",
        &req.max_bootstrap_groups.to_string(),
    );
    let contents =
        upsert_obelisk_index_value(contents, "max_page_limit", &req.max_page_limit.to_string());
    let contents = upsert_obelisk_index_value(
        contents,
        "bootstrap_requests_per_minute",
        &req.bootstrap_requests_per_minute.to_string(),
    );
    let contents = upsert_obelisk_index_value(
        contents,
        "message_requests_per_minute",
        &req.message_requests_per_minute.to_string(),
    );
    let contents = upsert_obelisk_index_value(
        contents,
        "reconcile_interval",
        &yaml_quote(&format!("{}m", req.reconcile_interval_minutes)),
    );
    std::fs::write(path, contents)
}

fn write_setup_mode_settings(config_dir: &StdPath) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(config_dir)?;
    let relay_secret_key = read_yaml_scalar(config_dir, "relay_secret_key", "");
    if relay_secret_key.len() != 64 || !relay_secret_key.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Missing or invalid relay_secret_key",
        ));
    }

    let relay_url = read_yaml_scalar(config_dir, "relay_url", "wss://relay.example.com");
    let db_path = read_yaml_scalar(config_dir, "db_path", "/app/db");
    let local_addr = read_yaml_scalar(config_dir, "local_addr", "0.0.0.0:8080");

    let contents = format!(
        "relay:\n  relay_secret_key: \"{relay_secret_key}\"\n  relay_url: \"{relay_url}\"\n  db_path: \"{db_path}\"\n  local_addr: \"{local_addr}\"\n\n  whitelisted_pubkeys: []\n  admin_pubkeys: []\n\n  max_subscriptions: 50\n  max_limit: 500\n\n  pubkey_rate_limit_per_minute: 6000\n  connection_rate_limit_per_minute: 12000\n  global_rate_limit_per_minute: 600000\n\n  obelisk_index:\n    enabled: true\n    recent_per_group: 50\n    max_bootstrap_groups: 500\n    max_page_limit: 100\n    bootstrap_requests_per_minute: 30\n    message_requests_per_minute: 120\n    reconcile_interval: \"5m\"\n\n  websocket:\n    max_connection_duration: \"24h\"\n    idle_timeout: \"30m\"\n    max_connections: 300\n"
    );

    std::fs::write(config_dir.join(SETTINGS_LOCAL_FILE), contents)
}

fn persist_setup_owner_pubkey(
    owner: PublicKey,
    config_dir: &StdPath,
) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(config_dir)?;
    let json = serde_json::to_string_pretty(&owner.to_hex())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    std::fs::write(config_dir.join(SETUP_OWNER_FILE), json)
}

fn load_setup_owner_pubkey(config_dir: &StdPath) -> Option<PublicKey> {
    let path = config_dir.join(SETUP_OWNER_FILE);
    let contents = std::fs::read_to_string(path).ok()?;
    let hex = serde_json::from_str::<String>(&contents)
        .unwrap_or_else(|_| contents.trim().trim_matches('"').to_string());
    PublicKey::from_hex(&hex).ok()
}

fn clear_setup_owner_pubkey(config_dir: &StdPath) -> Result<(), std::io::Error> {
    match std::fs::remove_file(config_dir.join(SETUP_OWNER_FILE)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn backup_config_files(config_dir: &StdPath) -> Result<String, std::io::Error> {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let backup_dir = config_dir
        .join("backups")
        .join(format!("config-reset-{unix}"));
    std::fs::create_dir_all(&backup_dir)?;

    for file_name in BACKUP_FILE_NAMES {
        let source = config_dir.join(file_name);
        if source.exists() {
            let _ = std::fs::copy(&source, backup_dir.join(file_name));
        }
    }

    Ok(backup_dir.display().to_string())
}

pub fn load_runtime_admin_pubkeys(config_dir: &StdPath) -> Vec<PublicKey> {
    let path = config_dir.join(ADMIN_RUNTIME_FILE);
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
                    "Loaded {} runtime admin pubkeys from {}",
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

// --- Routes ---

pub fn admin_routes() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/setup/status", get(handle_setup_status))
        .route("/setup", post(handle_setup))
        .route("/config/reset", post(handle_config_reset))
        .route("/config/backups", get(handle_config_backups_list))
        .route(
            "/config/backups/{id}/download",
            get(handle_config_backup_download),
        )
        .route(
            "/config/backups/{id}/restore",
            post(handle_config_backup_restore),
        )
        .route("/restart", post(handle_restart_relay))
        .route(
            "/admin-pubkeys",
            get(handle_admin_pubkeys_list).post(handle_admin_pubkeys_add),
        )
        .route("/admin-pubkeys/{hex}", delete(handle_admin_pubkeys_remove))
        .route(
            "/relay-identity",
            get(handle_relay_identity).post(handle_relay_identity_update),
        )
        .route("/relay-identity/rotate-key", post(handle_relay_key_rotate))
        .route(
            "/access-settings",
            get(handle_access_settings).post(handle_access_settings_update),
        )
        .route(
            "/storage-settings",
            get(handle_storage_settings).post(handle_storage_settings_update),
        )
        .route(
            "/obelisk-index-settings",
            get(handle_obelisk_index_settings).post(handle_obelisk_index_settings_update),
        )
        .route("/challenge", get(handle_challenge))
        .route("/auth", post(handle_auth))
        .route("/session", get(handle_session_check))
        .route("/whitelist", get(handle_whitelist_list))
        .route("/whitelist", post(handle_whitelist_add))
        .route("/whitelist/{hex}", delete(handle_whitelist_remove))
        .route("/retention", get(handle_retention_status))
        .route("/groups", get(handle_groups))
        .route("/groups/{id}", delete(handle_group_delete))
        .route("/stats", get(handle_stats))
        .route(
            "/reference-accounts",
            get(handle_reference_accounts_list).post(handle_reference_accounts_add),
        )
        .route(
            "/reference-accounts/{hex}",
            delete(handle_reference_accounts_remove),
        )
        .route(
            "/reference-accounts/sync",
            post(handle_reference_accounts_sync),
        )
        .route(
            "/blacklist",
            get(handle_blacklist_list).post(handle_blacklist_add),
        )
        .route("/blacklist/{hex}", delete(handle_blacklist_remove))
        .route("/groups/{id}/events", get(handle_group_events))
        .route("/events/{event_id}", delete(handle_event_delete))
        .route(
            "/groups/{id}/members/{pubkey}",
            delete(handle_group_member_remove),
        )
        .route("/groups/{id}/members", get(handle_group_members))
        .route("/users/{pubkey}/events", delete(handle_user_events_delete))
}

pub fn public_api_routes() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/relay-info", get(handle_relay_info))
        .route("/retention", get(handle_retention_status))
}

// --- Handlers ---

async fn handle_challenge(State(state): State<Arc<ServerState>>) -> impl IntoResponse {
    let challenge = random_hex(32);
    let admin_state = get_admin_state(&state);

    admin_state.challenges.write().insert(
        challenge.clone(),
        ChallengeRecord {
            _challenge: challenge.clone(),
            created_at: std::time::Instant::now(),
        },
    );

    // Clean up old challenges (older than 5 minutes)
    let cutoff = std::time::Instant::now() - CHALLENGE_TTL;
    admin_state
        .challenges
        .write()
        .retain(|_, v| v.created_at > cutoff);

    Json(ChallengeResponse { challenge })
}

async fn handle_setup_status(State(state): State<Arc<ServerState>>) -> impl IntoResponse {
    let admin_state = get_admin_state(&state);
    let admin_count = admin_state.admin_pubkeys.read().len();
    let setup_owner = if admin_count == 0 {
        load_setup_owner_pubkey(StdPath::new(&admin_state.config_dir))
    } else {
        None
    };

    Json(SetupStatusResponse {
        needs_setup: admin_count == 0,
        admin_count,
        relay_url: admin_state.relay_url.clone(),
        whitelisted_count: state.whitelist.len(),
        reference_account_count: state.reference_accounts.len(),
        setup_owner_pubkey: setup_owner.as_ref().map(|owner| owner.to_hex()),
        setup_owner_npub: setup_owner
            .as_ref()
            .and_then(|owner| owner.to_bech32().ok()),
    })
}

async fn handle_setup(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<SetupRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if !admin_state.admin_pubkeys.read().is_empty() {
        return Err(error_response(
            StatusCode::CONFLICT,
            "Relay setup has already been completed",
        ));
    }

    let event = parse_signed_event(&req.signed_event)?;
    verify_auth_event(&event)?;
    consume_challenge(&admin_state, &event)?;

    if let Some(setup_owner) = load_setup_owner_pubkey(StdPath::new(&admin_state.config_dir)) {
        if event.pubkey != setup_owner {
            return Err(error_response(
                StatusCode::FORBIDDEN,
                "Setup is locked to the retained owner pubkey",
            ));
        }
    }

    let access_policy = req.access_policy.as_deref().unwrap_or("owner_only");
    if access_policy != "owner_only" && access_policy != "open" {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Unknown access policy",
        ));
    }

    {
        let mut admins = admin_state.admin_pubkeys.write();
        if !admins.is_empty() {
            return Err(error_response(
                StatusCode::CONFLICT,
                "Relay setup has already been completed",
            ));
        }
        admins.push(event.pubkey);
        if let Err(e) =
            persist_runtime_admin_pubkeys(admins.as_slice(), StdPath::new(&admin_state.config_dir))
        {
            admins.retain(|pk| pk != &event.pubkey);
            warn!("Failed to persist runtime admin pubkeys: {}", e);
            return Err(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to persist admin owner",
            ));
        }
    }

    let whitelisted_owner = access_policy == "owner_only";
    if whitelisted_owner {
        if state.whitelist.add(event.pubkey) {
            if let Err(e) = state.whitelist.persist(StdPath::new(&state.config_dir)) {
                warn!("Failed to persist owner whitelist entry: {}", e);
            }
        }
    }

    let reference_owner = req.add_owner_reference.unwrap_or(true);
    if reference_owner && state.reference_accounts.add(event.pubkey) {
        if let Err(e) = state
            .reference_accounts
            .persist(StdPath::new(&state.config_dir))
        {
            warn!("Failed to persist owner reference account: {}", e);
        }
    }

    let token = create_session(&admin_state, event.pubkey);
    if let Err(e) = clear_setup_owner_pubkey(StdPath::new(&admin_state.config_dir)) {
        warn!("Failed to clear setup owner pubkey: {}", e);
    }
    info!("Relay setup completed by {}", event.pubkey);

    Ok(Json(SetupResponse {
        token,
        admin_pubkey: event.pubkey.to_hex(),
        admin_npub: event.pubkey.to_bech32().unwrap_or_default(),
        whitelisted_owner,
        reference_owner,
    }))
}

async fn handle_config_reset(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(req): Json<ConfigResetRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    let owner = validate_session(&admin_state, &headers).ok_or_else(unauthorized)?;

    if req.confirm.trim() != "RESET" {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Type RESET to confirm",
        ));
    }

    let config_dir = StdPath::new(&state.config_dir);
    let backup_path = backup_config_files(config_dir).map_err(|e| {
        warn!("Failed to back up config before reset: {}", e);
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to back up current config",
        )
    })?;

    persist_setup_owner_pubkey(owner, config_dir).map_err(|e| {
        warn!("Failed to persist setup owner pubkey: {}", e);
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to preserve setup owner pubkey",
        )
    })?;

    write_setup_mode_settings(config_dir).map_err(|e| {
        warn!("Failed to write setup-mode settings: {}", e);
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to write relay settings",
        )
    })?;

    {
        let empty_admins: Vec<PublicKey> = Vec::new();
        persist_runtime_admin_pubkeys(empty_admins.as_slice(), config_dir).map_err(|e| {
            warn!("Failed to persist reset admin pubkeys: {}", e);
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to persist admin pubkeys",
            )
        })?;
        admin_state.admin_pubkeys.write().clear();
    }
    admin_state.sessions.write().clear();
    admin_state.challenges.write().clear();

    state.whitelist.replace_manual(Vec::new());
    state.whitelist.set_follow_derived(Vec::new());
    state.whitelist.persist(config_dir).map_err(|e| {
        warn!("Failed to persist reset whitelist: {}", e);
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to persist whitelist",
        )
    })?;
    crate::follow_sync::persist_follow_derived(&[], config_dir).map_err(|e| {
        warn!("Failed to clear follow-derived whitelist: {}", e);
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to clear follow-derived whitelist",
        )
    })?;

    state.reference_accounts.replace_all(Vec::new());
    state.reference_accounts.persist(config_dir).map_err(|e| {
        warn!("Failed to persist reset reference accounts: {}", e);
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to persist reference accounts",
        )
    })?;

    state.whitelist.blacklist().replace_all(Vec::new());
    state
        .whitelist
        .blacklist()
        .persist(config_dir)
        .map_err(|e| {
            warn!("Failed to clear blacklist: {}", e);
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to clear blacklist",
            )
        })?;

    info!(
        "Admin {} reset relay configuration without deleting event data",
        owner
    );

    Ok(Json(ConfigResetResponse {
        setup_owner_pubkey: owner.to_hex(),
        setup_owner_npub: owner.to_bech32().unwrap_or_default(),
        needs_setup: true,
        whitelisted_count: state.whitelist.len(),
        reference_account_count: state.reference_accounts.len(),
        backup_path,
        message: "Relay configuration reset. Reopen setup to choose access policy. Event data was not removed.".to_string(),
    }))
}

async fn handle_config_backups_list(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    Ok(Json(list_backup_entries(StdPath::new(&state.config_dir))))
}

async fn handle_config_backup_download(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let config_dir = StdPath::new(&state.config_dir);
    let backup_dir = backup_id_to_dir(config_dir, &id)?;
    let mut files = Vec::new();
    for file_name in BACKUP_FILE_NAMES {
        let path = backup_dir.join(file_name);
        if !path.exists() {
            continue;
        }
        let content = std::fs::read_to_string(&path).map_err(|e| {
            warn!("Failed to read backup file {}: {}", path.display(), e);
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to read backup")
        })?;
        files.push(BackupDownloadFile {
            name: file_name.to_string(),
            content,
        });
    }

    Ok(Json(BackupDownloadResponse { id, files }))
}

async fn handle_config_backup_restore(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(req): Json<RestoreBackupRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    if req.confirm.trim() != "RESTORE" {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Type RESTORE to confirm",
        ));
    }

    let config_dir = StdPath::new(&state.config_dir);
    let backup_dir = backup_id_to_dir(config_dir, &id)?;
    let backup_before_restore_path = backup_config_files(config_dir).map_err(|e| {
        warn!("Failed to back up config before restore: {}", e);
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to back up current config",
        )
    })?;

    for file_name in BACKUP_FILE_NAMES {
        let source = backup_dir.join(file_name);
        let destination = config_dir.join(file_name);
        if source.exists() {
            std::fs::copy(&source, &destination).map_err(|e| {
                warn!(
                    "Failed to restore {} from {}: {}",
                    destination.display(),
                    source.display(),
                    e
                );
                error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to restore backup",
                )
            })?;
        } else if destination.exists() {
            std::fs::remove_file(&destination).map_err(|e| {
                warn!(
                    "Failed to clear {} during restore: {}",
                    destination.display(),
                    e
                );
                error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Failed to restore backup",
                )
            })?;
        }
    }

    apply_runtime_config_from_files(&state, &admin_state, config_dir);
    info!("Restored relay config backup {}", id);

    let admin_count = admin_state.admin_pubkeys.read().len();

    Ok(Json(RestoreBackupResponse {
        id,
        backup_before_restore_path,
        admin_count,
        whitelisted_count: state.whitelist.len(),
        reference_account_count: state.reference_accounts.len(),
        restart_required: true,
        message: "Backup restored. Restart the relay to apply startup-only settings.".to_string(),
    }))
}

async fn handle_restart_relay(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(req): Json<RestartRelayRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    let admin_pubkey = validate_session(&admin_state, &headers).ok_or_else(unauthorized)?;

    if req.confirm.trim() != "RESTART" {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Type RESTART to confirm",
        ));
    }

    info!("Admin {} requested relay restart", admin_pubkey);
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(750)).await;
        std::process::exit(0);
    });

    Ok(Json(RestartRelayResponse {
        message: "Relay restart scheduled".to_string(),
        restart_in_ms: 750,
    }))
}

async fn handle_admin_pubkeys_list(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    let current_pubkey = validate_session(&admin_state, &headers).ok_or_else(unauthorized)?;

    let entries: Vec<AdminPubkeyEntry> = admin_state
        .admin_pubkeys
        .read()
        .iter()
        .map(|pk| AdminPubkeyEntry {
            hex: pk.to_hex(),
            npub: pk.to_bech32().unwrap_or_default(),
            current_session: *pk == current_pubkey,
        })
        .collect();

    Ok(Json(entries))
}

async fn handle_admin_pubkeys_add(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(req): Json<AddAdminPubkeyRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let pk = parse_pubkey_input(&req.pubkey)?;
    {
        let mut admins = admin_state.admin_pubkeys.write();
        if !admins.contains(&pk) {
            admins.push(pk);
        }
        persist_runtime_admin_pubkeys(admins.as_slice(), StdPath::new(&state.config_dir)).map_err(
            |e| {
                warn!("Failed to persist admin pubkeys: {}", e);
                error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to persist admin")
            },
        )?;
    }

    Ok(Json(AdminPubkeyEntry {
        hex: pk.to_hex(),
        npub: pk.to_bech32().unwrap_or_default(),
        current_session: false,
    }))
}

async fn handle_admin_pubkeys_remove(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(hex): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    let current_pubkey = validate_session(&admin_state, &headers).ok_or_else(unauthorized)?;
    let pk = PublicKey::from_hex(&hex)
        .map_err(|_| error_response(StatusCode::BAD_REQUEST, "Invalid hex pubkey"))?;

    if pk == current_pubkey {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "You cannot remove the admin pubkey for your current session",
        ));
    }

    {
        let mut admins = admin_state.admin_pubkeys.write();
        if admins.len() <= 1 {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "At least one admin pubkey is required",
            ));
        }
        admins.retain(|candidate| candidate != &pk);
        persist_runtime_admin_pubkeys(admins.as_slice(), StdPath::new(&state.config_dir)).map_err(
            |e| {
                warn!("Failed to persist admin pubkeys: {}", e);
                error_response(StatusCode::INTERNAL_SERVER_ERROR, "Failed to persist admin")
            },
        )?;
    }

    Ok(StatusCode::NO_CONTENT)
}

async fn handle_relay_identity(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    Ok(Json(relay_identity_response(&state, false)))
}

async fn handle_relay_identity_update(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(req): Json<RelayIdentityRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    if req.relay_name.trim().is_empty() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Relay name is required",
        ));
    }
    RelayUrl::parse(req.relay_url.trim()).map_err(|_| {
        error_response(
            StatusCode::BAD_REQUEST,
            "Relay URL must be a valid ws:// or wss:// URL",
        )
    })?;

    persist_relay_identity_settings(
        StdPath::new(&state.config_dir),
        req.relay_name.trim(),
        req.relay_description.trim(),
        req.relay_url.trim(),
    )
    .map_err(|e| {
        warn!("Failed to persist relay identity settings: {}", e);
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to persist relay identity",
        )
    })?;

    Ok(Json(RelayIdentityResponse {
        relay_name: req.relay_name.trim().to_string(),
        relay_description: req.relay_description.trim().to_string(),
        relay_url: req.relay_url.trim().to_string(),
        relay_pubkey: state.relay_pubkey.clone(),
        restart_required: true,
    }))
}

async fn handle_relay_key_rotate(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(req): Json<RotateRelayKeyRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    let admin_pubkey = validate_session(&admin_state, &headers).ok_or_else(unauthorized)?;

    if req.confirm.trim() != "ROTATE" {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Type ROTATE to confirm",
        ));
    }

    let (secret_key_hex, relay_pubkey) = generate_relay_secret_key();
    persist_relay_secret_key(StdPath::new(&state.config_dir), &secret_key_hex).map_err(|e| {
        warn!("Failed to persist rotated relay secret key: {}", e);
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to persist relay key",
        )
    })?;

    info!(
        "Admin {} rotated relay key; new relay pubkey {}",
        admin_pubkey, relay_pubkey
    );

    Ok(Json(RotateRelayKeyResponse {
        relay_pubkey: relay_pubkey.to_hex(),
        restart_required: true,
        message: "Relay key rotated. Restart the relay to activate the new key.".to_string(),
    }))
}

async fn handle_access_settings(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let config_dir = StdPath::new(&state.config_dir);
    Ok(Json(AccessSettingsResponse {
        access_policy: if state.whitelist.is_empty() {
            "open".to_string()
        } else {
            "owner_only".to_string()
        },
        pubkey_rate_limit_per_minute: read_yaml_u32(
            config_dir,
            "pubkey_rate_limit_per_minute",
            6000,
        ),
        connection_rate_limit_per_minute: read_yaml_u32(
            config_dir,
            "connection_rate_limit_per_minute",
            12000,
        ),
        global_rate_limit_per_minute: read_yaml_u32(
            config_dir,
            "global_rate_limit_per_minute",
            600000,
        ),
        whitelisted_count: state.whitelist.len(),
        restart_required: false,
    }))
}

async fn handle_access_settings_update(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(req): Json<AccessSettingsRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    let admin_pubkey = validate_session(&admin_state, &headers).ok_or_else(unauthorized)?;

    if req.access_policy != "owner_only" && req.access_policy != "open" {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Unknown access policy",
        ));
    }

    let pubkey_rate_limit = req.pubkey_rate_limit_per_minute.unwrap_or(6000);
    let connection_rate_limit = req.connection_rate_limit_per_minute.unwrap_or(12000);
    let global_rate_limit = req.global_rate_limit_per_minute.unwrap_or(600000);

    if req.access_policy == "open"
        && (pubkey_rate_limit == 0 || connection_rate_limit == 0 || global_rate_limit == 0)
    {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Open relay mode requires all rate limits to be greater than zero",
        ));
    }

    let config_dir = StdPath::new(&state.config_dir);
    persist_rate_limit_settings(
        config_dir,
        pubkey_rate_limit,
        connection_rate_limit,
        global_rate_limit,
    )
    .map_err(|e| {
        warn!("Failed to persist access settings: {}", e);
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to persist access settings",
        )
    })?;

    if req.access_policy == "open" {
        state.whitelist.replace_manual(Vec::new());
        state.whitelist.set_follow_derived(Vec::new());
        state.whitelist.persist(config_dir).map_err(|e| {
            warn!("Failed to persist open whitelist state: {}", e);
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to persist whitelist settings",
            )
        })?;
        crate::follow_sync::persist_follow_derived(&[], config_dir).map_err(|e| {
            warn!("Failed to clear follow-derived whitelist: {}", e);
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to persist whitelist settings",
            )
        })?;
    } else if state.whitelist.is_empty() {
        state.whitelist.add(admin_pubkey);
        state.whitelist.persist(config_dir).map_err(|e| {
            warn!("Failed to persist enforced whitelist state: {}", e);
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to persist whitelist settings",
            )
        })?;
    }

    Ok(Json(AccessSettingsResponse {
        access_policy: req.access_policy,
        pubkey_rate_limit_per_minute: pubkey_rate_limit,
        connection_rate_limit_per_minute: connection_rate_limit,
        global_rate_limit_per_minute: global_rate_limit,
        whitelisted_count: state.whitelist.len(),
        restart_required: true,
    }))
}

async fn handle_storage_settings(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    Ok(Json(storage_settings_response(&state, false)))
}

async fn handle_storage_settings_update(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(req): Json<StorageSettingsRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    if req.pruning_enabled {
        if req.retention_days == 0 {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "Retention must be at least 1 day",
            ));
        }
        if req.prune_interval_minutes == 0 {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "Prune interval must be at least 1 minute",
            ));
        }
        if req.prune_kinds.is_empty() {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "Choose at least one event kind to prune",
            ));
        }
    }

    let safe_kinds: Vec<u16> = req
        .prune_kinds
        .into_iter()
        .filter(|kind| !crate::pruner::NEVER_PRUNE_KINDS.contains(kind))
        .collect();

    if req.pruning_enabled && safe_kinds.is_empty() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Selected kinds are protected and cannot be pruned",
        ));
    }

    let config_dir = StdPath::new(&state.config_dir);
    std::fs::create_dir_all(config_dir).map_err(|e| {
        warn!("Failed to create config dir for storage settings: {}", e);
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to persist storage settings",
        )
    })?;
    let path = config_dir.join(SETTINGS_LOCAL_FILE);
    let contents = std::fs::read_to_string(&path).unwrap_or_else(|_| "relay:\n".to_string());
    let contents = upsert_relay_value(
        contents,
        "enable_event_pruner",
        if req.pruning_enabled { "true" } else { "false" },
    );
    let contents = upsert_relay_value(
        contents,
        "event_retention",
        &format!("\"{}d\"", req.retention_days),
    );
    let contents = upsert_relay_value(
        contents,
        "prune_interval",
        &format!("\"{}m\"", req.prune_interval_minutes),
    );
    let kind_list = safe_kinds
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let contents = upsert_relay_value(contents, "prune_kinds", &format!("[{kind_list}]"));
    std::fs::write(path, contents).map_err(|e| {
        warn!("Failed to persist storage settings: {}", e);
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to persist storage settings",
        )
    })?;

    Ok(Json(storage_settings_response(&state, true)))
}

async fn handle_obelisk_index_settings(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    Ok(Json(obelisk_index_settings_response(&state, false)))
}

async fn handle_obelisk_index_settings_update(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(req): Json<ObeliskIndexSettingsRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    if req.recent_per_group == 0
        || req.max_bootstrap_groups == 0
        || req.max_page_limit == 0
        || req.bootstrap_requests_per_minute == 0
        || req.message_requests_per_minute == 0
        || req.reconcile_interval_minutes == 0
    {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Obelisk index limits and intervals must be greater than zero",
        ));
    }

    persist_obelisk_index_settings(StdPath::new(&state.config_dir), &req).map_err(|e| {
        warn!("Failed to persist Obelisk index settings: {}", e);
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to persist Obelisk index settings",
        )
    })?;

    Ok(Json(ObeliskIndexSettingsResponse {
        enabled: req.enabled,
        active_enabled: state.obelisk_index.is_some(),
        recent_per_group: req.recent_per_group,
        max_bootstrap_groups: req.max_bootstrap_groups,
        max_page_limit: req.max_page_limit,
        bootstrap_requests_per_minute: req.bootstrap_requests_per_minute,
        message_requests_per_minute: req.message_requests_per_minute,
        reconcile_interval_minutes: req.reconcile_interval_minutes,
        restart_required: true,
    }))
}

async fn handle_auth(
    State(state): State<Arc<ServerState>>,
    Json(req): Json<AuthRequest>,
) -> impl IntoResponse {
    let admin_state = get_admin_state(&state);

    let event = parse_signed_event(&req.signed_event)?;
    verify_auth_event(&event)?;

    // Check pubkey is an admin
    if !is_admin(&admin_state, &event.pubkey) {
        return Err(error_response(StatusCode::FORBIDDEN, "Not an admin pubkey"));
    }

    consume_challenge(&admin_state, &event)?;
    let token = create_session(&admin_state, event.pubkey);

    debug!("Admin authenticated: {}", event.pubkey);
    Ok(Json(AuthResponse { token }))
}

async fn handle_session_check(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let admin_state = get_admin_state(&state);
    match validate_session(&admin_state, &headers) {
        Some(pk) => Json(SessionCheckResponse {
            valid: true,
            pubkey: Some(pk.to_hex()),
        }),
        None => Json(SessionCheckResponse {
            valid: false,
            pubkey: None,
        }),
    }
}

async fn handle_whitelist_list(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let entries: Vec<WhitelistEntry> = state
        .whitelist
        .list()
        .iter()
        .map(|pk| WhitelistEntry {
            hex: pk.to_hex(),
            npub: pk.to_bech32().unwrap_or_default(),
        })
        .collect();

    Ok(Json(entries))
}

async fn handle_whitelist_add(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(req): Json<AddWhitelistRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let pk = parse_pubkey_input(&req.pubkey)?;

    let added = state.whitelist.add(pk);
    if added {
        if let Err(e) = state
            .whitelist
            .persist(std::path::Path::new(&state.config_dir))
        {
            warn!("Failed to persist whitelist: {}", e);
        }
    }

    Ok((
        StatusCode::OK,
        Json(WhitelistEntry {
            hex: pk.to_hex(),
            npub: pk.to_bech32().unwrap_or_default(),
        }),
    ))
}

async fn handle_whitelist_remove(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(hex): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let pk = PublicKey::from_hex(&hex).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid hex pubkey".to_string(),
            }),
        )
    })?;

    let removed = state.whitelist.remove(&pk);
    if removed {
        if let Err(e) = state
            .whitelist
            .persist(std::path::Path::new(&state.config_dir))
        {
            warn!("Failed to persist whitelist: {}", e);
        }
    }

    Ok(StatusCode::NO_CONTENT)
}

async fn handle_groups(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let groups = &state.http_state.groups;
    let mut result = Vec::new();

    for entry in groups.iter() {
        let ((_, id), group) = (entry.key(), entry.value());
        let admin_count = group
            .members
            .values()
            .filter(|member| {
                member
                    .roles
                    .iter()
                    .any(|role| role.to_string().eq_ignore_ascii_case("admin"))
            })
            .count();
        let metadata_tags = group
            .metadata
            .unknown_tags
            .iter()
            .map(|tag| tag.as_slice().iter().map(|item| item.to_string()).collect())
            .collect();

        result.push(GroupInfo {
            id: id.clone(),
            name: group.metadata.name.clone(),
            about: group.metadata.about.clone(),
            picture: group.metadata.picture.clone(),
            banner: group.metadata.banner.clone(),
            parent: group.metadata.parent.clone(),
            channel_kind: group.metadata.channel_kind.clone(),
            member_count: group.members.len(),
            admin_count,
            private: group.metadata.private,
            closed: group.metadata.closed,
            broadcast: group.metadata.is_broadcast,
            metadata_tags,
        });
    }

    Ok(Json(result))
}

async fn handle_group_delete(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(group_id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    state
        .http_state
        .groups
        .admin_delete_group(&group_id)
        .await
        .map_err(|e| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;

    Ok(StatusCode::NO_CONTENT)
}

async fn handle_stats(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let groups = &state.http_state.groups;
    let mut total_members = 0usize;
    let mut total_groups = 0usize;

    for entry in groups.iter() {
        total_groups += 1;
        total_members += entry.value().members.len();
    }

    Ok(Json(StatsResponse {
        active_connections: state.connection_counter.load(Ordering::Relaxed),
        total_groups,
        total_members,
        whitelisted_count: state.whitelist.len(),
        uptime_seconds: state.start_time.elapsed().as_secs(),
    }))
}

async fn handle_relay_info(State(state): State<Arc<ServerState>>) -> impl IntoResponse {
    let groups = &state.http_state.groups;
    let mut group_count = 0usize;

    for _ in groups.iter() {
        group_count += 1;
    }

    Json(RelayInfoResponse {
        name: state.relay_name.clone(),
        description: state.relay_description.clone(),
        group_count,
        supported_nips: state.supported_nips.clone(),
    })
}

// --- Retention / pruner status ---

#[derive(Serialize)]
struct RetentionStatus {
    enabled: bool,
    retention_secs: Option<u64>,
    interval_secs: Option<u64>,
    prune_kinds: Option<Vec<u16>>,
    total_pruned: u64,
    runs: u64,
    last_run_unix: i64,
}

async fn handle_retention_status(State(state): State<Arc<ServerState>>) -> impl IntoResponse {
    let (enabled, retention_secs, interval_secs, prune_kinds) = match &state.pruner_config {
        Some(cfg) => (
            true,
            Some(cfg.retention.as_secs()),
            Some(cfg.interval.as_secs()),
            Some(cfg.kinds_as_u16()),
        ),
        None => (false, None, None, None),
    };

    let (total_pruned, runs, last_run_unix) = match &state.pruner_stats {
        Some(s) => (
            s.total_pruned.load(Ordering::Relaxed),
            s.runs.load(Ordering::Relaxed),
            s.last_run_unix.load(Ordering::Relaxed),
        ),
        None => (0, 0, 0),
    };

    Json(RetentionStatus {
        enabled,
        retention_secs,
        interval_secs,
        prune_kinds,
        total_pruned,
        runs,
        last_run_unix,
    })
}

// --- Reference accounts handlers ---

async fn handle_reference_accounts_list(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let entries: Vec<ReferenceAccountEntry> = state
        .reference_accounts
        .list()
        .iter()
        .map(|pk| ReferenceAccountEntry {
            hex: pk.to_hex(),
            npub: pk.to_bech32().unwrap_or_default(),
        })
        .collect();

    Ok(Json(entries))
}

async fn handle_reference_accounts_add(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(req): Json<AddReferenceAccountRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let pk = if req.pubkey.starts_with("npub") {
        PublicKey::from_bech32(&req.pubkey).map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "Invalid npub".to_string(),
                }),
            )
        })?
    } else {
        PublicKey::from_hex(&req.pubkey).map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "Invalid hex pubkey".to_string(),
                }),
            )
        })?
    };

    let added = state.reference_accounts.add(pk);
    if added {
        if let Err(e) = state
            .reference_accounts
            .persist(std::path::Path::new(&state.config_dir))
        {
            warn!("Failed to persist reference accounts: {}", e);
        }

        // Auto-sync follows in background
        let whitelist = state.whitelist.clone();
        let reference_accounts = state.reference_accounts.clone();
        let config_dir = state.config_dir.clone();
        tokio::spawn(async move {
            let ref_list = reference_accounts.list();
            if ref_list.is_empty() {
                return;
            }
            info!("Auto-syncing follows after adding reference account");
            match follow_sync::sync_follows(&ref_list).await {
                Ok(follows) => {
                    let count = follows.len();
                    whitelist.set_follow_derived(follows.clone());
                    if let Err(e) = follow_sync::persist_follow_derived(
                        &follows,
                        std::path::Path::new(&config_dir),
                    ) {
                        warn!("Failed to persist follow-derived whitelist: {}", e);
                    }
                    info!("Auto-sync complete: {} derived pubkeys", count);
                }
                Err(e) => {
                    warn!("Auto-sync failed: {}", e);
                }
            }
        });
    }

    Ok((
        StatusCode::OK,
        Json(ReferenceAccountEntry {
            hex: pk.to_hex(),
            npub: pk.to_bech32().unwrap_or_default(),
        }),
    ))
}

async fn handle_reference_accounts_remove(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(hex): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let pk = PublicKey::from_hex(&hex).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid hex pubkey".to_string(),
            }),
        )
    })?;

    let removed = state.reference_accounts.remove(&pk);
    if removed {
        if let Err(e) = state
            .reference_accounts
            .persist(std::path::Path::new(&state.config_dir))
        {
            warn!("Failed to persist reference accounts: {}", e);
        }
    }

    Ok(StatusCode::NO_CONTENT)
}

async fn handle_reference_accounts_sync(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let ref_accounts = state.reference_accounts.list();
    if ref_accounts.is_empty() {
        return Ok(Json(SyncFollowsResponse {
            derived_count: 0,
            message: "No reference accounts configured".to_string(),
        }));
    }

    info!(
        "Starting follow sync for {} reference accounts",
        ref_accounts.len()
    );

    let follows = follow_sync::sync_follows(&ref_accounts)
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ErrorResponse {
                    error: format!("Sync failed: {}", e),
                }),
            )
        })?;

    let count = follows.len();

    // Update whitelist follow-derived set
    state.whitelist.set_follow_derived(follows.clone());

    // Persist to disk
    if let Err(e) =
        follow_sync::persist_follow_derived(&follows, std::path::Path::new(&state.config_dir))
    {
        warn!("Failed to persist follow-derived whitelist: {}", e);
    }

    info!("Follow sync complete: {} derived pubkeys", count);

    Ok(Json(SyncFollowsResponse {
        derived_count: count,
        message: format!(
            "Synced {} follows from {} reference accounts",
            count,
            ref_accounts.len()
        ),
    }))
}

// --- Blacklist handlers ---

async fn handle_blacklist_list(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let entries: Vec<BlacklistEntry> = state
        .whitelist
        .blacklist()
        .list()
        .iter()
        .map(|pk| BlacklistEntry {
            hex: pk.to_hex(),
            npub: pk.to_bech32().unwrap_or_default(),
        })
        .collect();

    Ok(Json(entries))
}

async fn handle_blacklist_add(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(req): Json<AddBlacklistRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let pk = if req.pubkey.starts_with("npub") {
        PublicKey::from_bech32(&req.pubkey).map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "Invalid npub".to_string(),
                }),
            )
        })?
    } else {
        PublicKey::from_hex(&req.pubkey).map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: "Invalid hex pubkey".to_string(),
                }),
            )
        })?
    };

    let added = state.whitelist.blacklist().add(pk);
    if added {
        if let Err(e) = state
            .whitelist
            .blacklist()
            .persist(std::path::Path::new(&state.config_dir))
        {
            warn!("Failed to persist blacklist: {}", e);
        }
    }

    Ok((
        StatusCode::OK,
        Json(BlacklistEntry {
            hex: pk.to_hex(),
            npub: pk.to_bech32().unwrap_or_default(),
        }),
    ))
}

async fn handle_blacklist_remove(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(hex): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let pk = PublicKey::from_hex(&hex).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "Invalid hex pubkey".to_string(),
            }),
        )
    })?;

    let removed = state.whitelist.blacklist().remove(&pk);
    if removed {
        if let Err(e) = state
            .whitelist
            .blacklist()
            .persist(std::path::Path::new(&state.config_dir))
        {
            warn!("Failed to persist blacklist: {}", e);
        }
    }

    Ok(StatusCode::NO_CONTENT)
}

// --- Group event / member handlers ---

async fn handle_group_events(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(group_id): Path<String>,
    Query(params): Query<GroupEventsQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let limit = params.limit.unwrap_or(100).min(500);
    let author = params.author.as_deref();

    let raw_events = state
        .http_state
        .groups
        .admin_get_group_events(&group_id, limit, author)
        .await
        .map_err(|e| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;

    let events: Vec<EventInfo> = raw_events
        .into_iter()
        .filter_map(|v| {
            Some(EventInfo {
                id: v.get("id")?.as_str()?.to_string(),
                pubkey: v.get("pubkey")?.as_str()?.to_string(),
                kind: v.get("kind")?.as_u64()?,
                content: v.get("content")?.as_str()?.to_string(),
                created_at: v.get("created_at")?.as_u64()?,
            })
        })
        .collect();

    Ok(Json(events))
}

async fn handle_event_delete(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(event_id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    state
        .http_state
        .groups
        .admin_delete_event(&event_id)
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;

    Ok(StatusCode::NO_CONTENT)
}

async fn handle_group_member_remove(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path((group_id, pubkey)): Path<(String, String)>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    state
        .http_state
        .groups
        .admin_remove_group_member(&group_id, &pubkey)
        .await
        .map_err(|e| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;

    Ok(StatusCode::NO_CONTENT)
}

async fn handle_group_members(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(group_id): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let raw = state
        .http_state
        .groups
        .admin_get_group_members(&group_id)
        .map_err(|e| {
            (
                StatusCode::NOT_FOUND,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;

    let members: Vec<MemberInfo> = raw
        .into_iter()
        .filter_map(|v| {
            Some(MemberInfo {
                pubkey: v.get("pubkey")?.as_str()?.to_string(),
                roles: v
                    .get("roles")?
                    .as_array()?
                    .iter()
                    .filter_map(|r| r.as_str().map(String::from))
                    .collect(),
            })
        })
        .collect();

    Ok(Json(members))
}

async fn handle_user_events_delete(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(pubkey): Path<String>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    state
        .http_state
        .groups
        .admin_delete_user_events(&pubkey)
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;

    Ok(StatusCode::NO_CONTENT)
}

// --- State helpers ---

fn get_admin_state(_state: &ServerState) -> AdminState {
    // AdminState is derived from ServerState on the fly.
    // Sessions and challenges are stored in the ServerState via lazy init.
    // For simplicity we store them in a once_cell inside this module.
    ADMIN_SHARED.get_or_init(|| AdminState {
        admin_pubkeys: Arc::new(RwLock::new(Vec::new())), // will be overridden
        sessions: Arc::new(RwLock::new(HashMap::new())),
        challenges: Arc::new(RwLock::new(HashMap::new())),
        relay_url: String::new(),
        config_dir: "config".to_string(),
    });

    // We actually need per-server state. Use a global for now since there's one server.
    ADMIN_SHARED.get().cloned().unwrap()
}

use once_cell::sync::OnceCell;
static ADMIN_SHARED: OnceCell<AdminState> = OnceCell::new();

/// Initialize the admin state. Must be called once during server setup.
pub fn init_admin_state(admin_pubkeys: Vec<PublicKey>, relay_url: String, config_dir: String) {
    let _ = ADMIN_SHARED.set(AdminState {
        admin_pubkeys: Arc::new(RwLock::new(admin_pubkeys)),
        sessions: Arc::new(RwLock::new(HashMap::new())),
        challenges: Arc::new(RwLock::new(HashMap::new())),
        relay_url,
        config_dir,
    });
}
