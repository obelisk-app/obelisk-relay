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
use std::path::Path as StdPath;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tracing::{debug, info, warn};

// --- Types ---

const ADMIN_RUNTIME_FILE: &str = "admin_pubkeys_runtime.json";
const SETTINGS_LOCAL_FILE: &str = "settings.local.yml";
const SETTINGS_DEFAULT_FILE: &str = "settings.yml";
const WHITELIST_FOLLOWS_FILE: &str = "whitelist_follows.json";
const BLACKLIST_FILE: &str = "blacklist.json";
const CHALLENGE_TTL: std::time::Duration = std::time::Duration::from_secs(300);
const SESSION_TTL: std::time::Duration = std::time::Duration::from_secs(4 * 3600);

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
    access_policy: Option<String>,
    keep_owner_reference: Option<bool>,
}

#[derive(Serialize)]
struct ConfigResetResponse {
    admin_pubkey: String,
    admin_npub: String,
    access_policy: String,
    whitelisted_count: usize,
    reference_account_count: usize,
    backup_path: String,
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

fn write_owner_only_settings(
    config_dir: &StdPath,
    owner: PublicKey,
    owner_only: bool,
) -> Result<(), std::io::Error> {
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
    let owner_hex = owner.to_hex();

    let whitelist_block = if owner_only {
        format!("  whitelisted_pubkeys:\n    - \"{owner_hex}\"\n")
    } else {
        "  whitelisted_pubkeys: []\n".to_string()
    };

    let contents = format!(
        "relay:\n  relay_secret_key: \"{relay_secret_key}\"\n  relay_url: \"{relay_url}\"\n  db_path: \"{db_path}\"\n  local_addr: \"{local_addr}\"\n\n{whitelist_block}\n  admin_pubkeys:\n    - \"{owner_hex}\"\n\n  max_subscriptions: 50\n  max_limit: 500\n\n  pubkey_rate_limit_per_minute: 6000\n  connection_rate_limit_per_minute: 12000\n  global_rate_limit_per_minute: 600000\n\n  websocket:\n    max_connection_duration: \"24h\"\n    idle_timeout: \"30m\"\n    max_connections: 300\n"
    );

    std::fs::write(config_dir.join(SETTINGS_LOCAL_FILE), contents)
}

fn backup_config_files(config_dir: &StdPath) -> Result<String, std::io::Error> {
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let backup_dir = config_dir.join("backups").join(format!("config-reset-{unix}"));
    std::fs::create_dir_all(&backup_dir)?;

    for file_name in [
        SETTINGS_LOCAL_FILE,
        "reference_accounts.json",
        "whitelist_runtime.json",
        WHITELIST_FOLLOWS_FILE,
        ADMIN_RUNTIME_FILE,
        BLACKLIST_FILE,
    ] {
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

async fn handle_challenge(
    State(state): State<Arc<ServerState>>,
) -> impl IntoResponse {
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

    Json(SetupStatusResponse {
        needs_setup: admin_count == 0,
        admin_count,
        relay_url: admin_state.relay_url.clone(),
        whitelisted_count: state.whitelist.len(),
        reference_account_count: state.reference_accounts.len(),
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
            persist_runtime_admin_pubkeys(
                admins.as_slice(),
                StdPath::new(&admin_state.config_dir),
            )
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

    let access_policy = req.access_policy.as_deref().unwrap_or("owner_only");
    if access_policy != "owner_only" && access_policy != "open" {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Unknown access policy",
        ));
    }

    let owner_only = access_policy == "owner_only";
    let keep_owner_reference = req.keep_owner_reference.unwrap_or(true);
    let config_dir = StdPath::new(&state.config_dir);
    let backup_path = backup_config_files(config_dir).map_err(|e| {
        warn!("Failed to back up config before reset: {}", e);
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to back up current config",
        )
    })?;

    write_owner_only_settings(config_dir, owner, owner_only).map_err(|e| {
        warn!("Failed to write owner-only settings: {}", e);
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to write relay settings",
        )
    })?;

    {
        let mut admins = admin_state.admin_pubkeys.write();
        admins.clear();
        admins.push(owner);
        persist_runtime_admin_pubkeys(admins.as_slice(), config_dir).map_err(|e| {
            warn!("Failed to persist reset admin pubkeys: {}", e);
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to persist admin pubkeys",
            )
        })?;
    }

    let whitelist_entries = if owner_only { vec![owner] } else { Vec::new() };
    state.whitelist.replace_manual(whitelist_entries);
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

    let reference_entries = if keep_owner_reference { vec![owner] } else { Vec::new() };
    state.reference_accounts.replace_all(reference_entries);
    state.reference_accounts.persist(config_dir).map_err(|e| {
        warn!("Failed to persist reset reference accounts: {}", e);
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to persist reference accounts",
        )
    })?;

    state.whitelist.blacklist().replace_all(Vec::new());
    state.whitelist.blacklist().persist(config_dir).map_err(|e| {
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
        admin_pubkey: owner.to_hex(),
        admin_npub: owner.to_bech32().unwrap_or_default(),
        access_policy: access_policy.to_string(),
        whitelisted_count: state.whitelist.len(),
        reference_account_count: state.reference_accounts.len(),
        backup_path,
        message: "Relay configuration reset. Event data was not removed.".to_string(),
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

    // Try to parse as npub first, then hex
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

    let added = state.whitelist.add(pk);
    if added {
        if let Err(e) = state.whitelist.persist(std::path::Path::new(&state.config_dir)) {
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
        if let Err(e) = state.whitelist.persist(std::path::Path::new(&state.config_dir)) {
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

async fn handle_relay_info(
    State(state): State<Arc<ServerState>>,
) -> impl IntoResponse {
    let groups = &state.http_state.groups;
    let mut group_count = 0usize;

    for _ in groups.iter() {
        group_count += 1;
    }

    Json(RelayInfoResponse {
        name: state.relay_name.clone(),
        description: state.relay_description.clone(),
        group_count,
        supported_nips: vec![1, 9, 11, 29, 40, 42, 70],
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

async fn handle_retention_status(
    State(state): State<Arc<ServerState>>,
) -> impl IntoResponse {
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

    info!("Starting follow sync for {} reference accounts", ref_accounts.len());

    let follows = follow_sync::sync_follows(&ref_accounts).await.map_err(|e| {
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
    ADMIN_SHARED
        .get()
        .cloned()
        .unwrap()
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
