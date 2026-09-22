use crate::follow_sync;
use crate::server::ServerState;
use axum::{
    extract::{Path, Query, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use nostr_sdk::prelude::*;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path as StdPath, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
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

/// The relay's owner — the identity that ran setup.
///
/// Distinct from [`SETUP_OWNER_FILE`], which is a transient marker cleared the
/// moment setup completes so the wizard cannot be replayed. This one is
/// permanent: it records who the relay belongs to. Without it every admin looks
/// alike, and the person who actually hosts the box is indistinguishable from
/// someone they granted access to.
const RELAY_OWNER_FILE: &str = "relay_owner_pubkey.json";
const BACKUP_DIR: &str = "backups";
const BACKUP_PREFIX: &str = "config-reset-";
const CHALLENGE_TTL: std::time::Duration = std::time::Duration::from_secs(300);
const SESSION_TTL: std::time::Duration = std::time::Duration::from_secs(4 * 3600);
const BACKUP_FILE_NAMES: [&str; 8] = [
    SETTINGS_LOCAL_FILE,
    REFERENCE_ACCOUNTS_FILE,
    WHITELIST_RUNTIME_FILE,
    WHITELIST_FOLLOWS_FILE,
    ADMIN_RUNTIME_FILE,
    BLACKLIST_FILE,
    SETUP_OWNER_FILE,
    RELAY_OWNER_FILE,
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
    prune_interval_minutes: u32,
    /// Retention in days, per kind. The unit is days rather than a humantime
    /// string because the UI offers a number input, and a free-text duration
    /// is an easy way to typo a policy that deletes far more than intended.
    #[serde(default)]
    retention_days_by_kind: std::collections::BTreeMap<u16, u32>,
    /// Pre-per-kind fields, still accepted so an older client keeps working.
    #[serde(default)]
    retention_days: Option<u32>,
    #[serde(default)]
    prune_kinds: Option<Vec<u16>>,
}

impl StorageSettingsRequest {
    /// Per-kind policies, folding the legacy single-window fields in when the
    /// caller did not send a map.
    fn policies(&self) -> std::collections::BTreeMap<u16, u32> {
        if !self.retention_days_by_kind.is_empty() {
            return self.retention_days_by_kind.clone();
        }
        match (self.retention_days, &self.prune_kinds) {
            (Some(days), Some(kinds)) => kinds.iter().map(|k| (*k, days)).collect(),
            _ => Default::default(),
        }
    }
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
    /// Events an operator deleted by hand since process start. Distinct from
    /// `total_pruned`, which is the automatic sweep only.
    admin_deleted_total: u64,
    runs: u64,
    last_run_unix: i64,
    /// Per-kind retention currently in force, seconds. None when pruning is off.
    policies_secs: Option<std::collections::BTreeMap<u16, u64>>,
    /// Events deleted per kind since process start, so the policy table can
    /// attribute deletions rather than showing one aggregate.
    deleted_by_kind: Option<std::collections::BTreeMap<u16, u64>>,
    restart_required: bool,
}

#[derive(Deserialize)]
struct StorageStatsQuery {
    /// Force a background recount even if the cached snapshot is still fresh.
    refresh: Option<bool>,
}

/// Envelope so the UI can distinguish "no snapshot yet, one is being built"
/// from "here are the numbers". Counting a multi-GB database takes longer than
/// an HTTP request may live, so the client polls instead of waiting.
#[derive(Serialize, Clone)]
struct StorageStatsEnvelope {
    /// A scan is running right now; poll again shortly.
    computing: bool,
    /// Last completed snapshot, if there has ever been one. May be stale while
    /// `computing` is true — `stats.computed_at` says how stale.
    stats: Option<StorageStatsResponse>,
}

#[derive(Serialize, Clone)]
struct StorageKindStat {
    kind: u16,
    count: usize,
    /// Bytes the sampled events of this kind occupy.
    sampled_bytes: u64,
    /// Mean bytes per event, divided here so the client never divides by zero.
    avg_bytes: u64,
}

#[derive(Serialize, Clone)]
struct RecipientStat {
    pubkey: String,
    count: usize,
}

/// One pubkey's share of the sample.
#[derive(Serialize, Clone)]
struct StorageAuthorStat {
    pubkey: String,
    npub: String,
    count: usize,
    sampled_bytes: u64,
    /// `"author"` or `"recipient"`. Gift wraps are signed by throwaway keys, so
    /// they are charged to the `p` tag instead; the UI must say which, or the
    /// recipient of a flood reads as its sender.
    attributed_by: &'static str,
    /// Whether this pubkey is currently blocked, so the row can offer the
    /// right action without a second request.
    blacklisted: bool,
}

/// The pubkeys behind a single kind.
#[derive(Serialize, Clone)]
struct StorageKindAuthorsResponse {
    kind: u16,
    attributed_by: &'static str,
    /// Events of this kind in the sample, so a row's share is computable.
    kind_count: usize,
    kind_sampled_bytes: u64,
    authors: Vec<StorageAuthorStat>,
    /// Unix seconds the underlying sample was taken.
    computed_at: i64,
}

#[derive(Deserialize)]
struct ExactCountQuery {
    kind: u16,
    /// When set, also report how many of those are older than this many days —
    /// the blast radius of a policy at that window.
    older_than_days: Option<u32>,
}

#[derive(Serialize, Clone)]
struct ExactCountResponse {
    kind: u16,
    total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    older_than: Option<u64>,
    older_than_days: Option<u32>,
    computed_at: i64,
}

/// Envelope mirroring the storage-stats one: counting a kind on a multi-GB
/// database outlives an HTTP request, so the client polls rather than waits.
#[derive(Serialize)]
struct ExactCountEnvelope {
    computing: bool,
    result: Option<ExactCountResponse>,
}

/// Completed exact counts, keyed by kind.
static EXACT_COUNT_CACHE: OnceCell<RwLock<HashMap<u16, ExactCountResponse>>> = OnceCell::new();
/// Kind currently being counted, if any. One at a time — these are the most
/// expensive reads the admin API can issue.
static EXACT_COUNT_RUNNING: OnceCell<RwLock<Option<u16>>> = OnceCell::new();

/// Exact count for one kind, on request.
///
/// Deliberately one kind at a time: a full sweep of every kind was measured at
/// over ten minutes on the production database, so the overview samples and
/// this exists for the moment an operator needs a real number before setting a
/// deletion policy.
async fn handle_exact_kind_count(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Query(params): Query<ExactCountQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let cache = EXACT_COUNT_CACHE.get_or_init(|| RwLock::new(HashMap::new()));
    let running = EXACT_COUNT_RUNNING.get_or_init(|| RwLock::new(None));

    // A cached result for this kind and window is returned as-is; the operator
    // asks for an exact count precisely when they are about to act on it, and
    // recomputing on every poll would keep the database busy for minutes.
    if let Some(hit) = cache.read().get(&params.kind) {
        if hit.older_than_days == params.older_than_days {
            return Ok(Json(ExactCountEnvelope {
                computing: false,
                result: Some(hit.clone()),
            }));
        }
    }

    {
        let mut slot = running.write();
        if slot.is_none() {
            *slot = Some(params.kind);
            let state_for_scan = Arc::clone(&state);
            let kind = params.kind;
            let days = params.older_than_days;
            tokio::spawn(async move {
                match state_for_scan
                    .http_state
                    .groups
                    .admin_exact_kind_count(kind, days)
                    .await
                {
                    Ok((total, older_than)) => {
                        EXACT_COUNT_CACHE
                            .get_or_init(|| RwLock::new(HashMap::new()))
                            .write()
                            .insert(
                                kind,
                                ExactCountResponse {
                                    kind,
                                    total,
                                    older_than,
                                    older_than_days: days,
                                    computed_at: Timestamp::now().as_secs() as i64,
                                },
                            );
                    }
                    Err(e) => warn!("Exact count failed for kind {}: {}", kind, e),
                }
                *EXACT_COUNT_RUNNING
                    .get_or_init(|| RwLock::new(None))
                    .write() = None;
            });
        }
    }

    Ok(Json(ExactCountEnvelope {
        computing: true,
        result: None,
    }))
}

#[derive(Serialize, Clone)]
struct StorageStatsResponse {
    /// Events examined for the kind breakdown below. This is a newest-first
    /// SAMPLE, not a total — exact per-kind counts cost >12s each on the
    /// production database, which is not affordable as a UI refresh.
    sampled_events: usize,
    /// The sample cap. If `sampled_events < sample_size`, the whole database
    /// was examined and the breakdown is exhaustive after all.
    sample_size: usize,
    /// True when the sample covered every stored event.
    sample_is_complete: bool,
    kinds: Vec<StorageKindStat>,
    /// Busiest gift-wrap recipients in the sample. Gift wraps have no usable
    /// author, so this is the only per-user view of that traffic.
    top_recipients: Vec<RecipientStat>,
    /// Heaviest pubkeys across all kinds — who the relay is storing data for.
    /// The kinds table says what is filling the disk; this says whose it is.
    top_authors: Vec<StorageAuthorStat>,
    /// Per-kind pubkey breakdown, kept in the cache but served by
    /// `/storage/kinds/{kind}/authors` rather than inlined: it is up to 25 rows
    /// per kind across dozens of kinds, which would dwarf a payload the Storage
    /// screen polls on a timer.
    #[serde(skip)]
    kind_authors: Vec<crate::groups::StorageKindAuthors>,
    newest_event_unix: u64,
    /// Oldest timestamp reached by the sample.
    oldest_sampled_unix: u64,
    scope_count: usize,
    db_size_bytes: u64,
    db_file_count: u64,
    /// How many events a prune run would delete right now under the currently
    /// configured retention window and kinds — whether or not pruning is armed.
    prune_preview: usize,
    prune_preview_retention_days: u32,
    prune_preview_kinds: Vec<u16>,
    /// Unix seconds these figures were computed; they are served from a short
    /// cache because each refresh walks one index range per kind.
    computed_at: i64,
    cached: bool,
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

/// The relay's capacity and abuse limits, as a single editable group.
///
/// These all lived in the config file only, with no console path, which stopped
/// being acceptable when `websocket.max_connections` went from dead config to an
/// enforced ceiling: a limit you cannot see or adjust is one you discover by
/// outage. Every field here needs a restart to take effect -- they are read once
/// at startup into `RelayConfig`, the middleware stack and the connection
/// limiter -- so the response says so plainly rather than implying a live change.
#[derive(Serialize)]
struct ConnectionSettingsResponse {
    max_connections: u32,
    max_connections_per_ip: u32,
    max_connection_duration_minutes: u32,
    idle_timeout_minutes: u32,
    max_subscriptions: u32,
    max_limit: u32,
    /// Coerce every group public at startup. Irreversible in practice: the
    /// startup sweep clears `private`/`hidden` on all stored groups, and nothing
    /// puts them back.
    force_public_groups: bool,
    running_force_public_groups: bool,
    /// What the running process is actually enforcing, which is not the saved
    /// value until a restart. Shown next to the field so the difference is
    /// visible rather than implied by a badge.
    running_max_connections: u32,
    running_max_connections_per_ip: u32,
    active_connections: u32,
    restart_required: bool,
}

#[derive(Deserialize)]
struct ConnectionSettingsRequest {
    max_connections: u32,
    max_connections_per_ip: u32,
    max_connection_duration_minutes: u32,
    idle_timeout_minutes: u32,
    max_subscriptions: u32,
    max_limit: u32,
    #[serde(default)]
    force_public_groups: bool,
    /// Must be the literal `FORCE PUBLIC` to turn the flag on. Ignored when
    /// turning it off, which is the safe direction.
    #[serde(default)]
    force_public_confirm: String,
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
    /// The identity that ran setup — the person who hosts this relay. Shown
    /// separately from the admins they granted access to.
    owner: bool,
}

#[derive(Deserialize)]
struct AddAdminPubkeyRequest {
    pubkey: String,
}

#[derive(Serialize)]
struct RelayIdentityResponse {
    relay_name: String,
    /// NIP-11 icon: an https:// URL or a small data: URI. Empty when unset.
    relay_icon: String,
    relay_description: String,
    relay_url: String,
    relay_pubkey: String,
    restart_required: bool,
}

#[derive(Deserialize)]
struct RelayIdentityRequest {
    #[serde(default)]
    relay_icon: String,
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
    /// Relay icon (URL or data URI); empty when unset.
    pub icon: String,
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

pub(crate) fn random_hex(bytes: usize) -> String {
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

/// Read one child of a nested `relay:` block, e.g. `websocket.idle_timeout`.
///
/// Generalized from the `obelisk_index` reader. The flat `read_yaml_scalar` is
/// indentation-blind, so it would happily return `wot.enabled` when asked for
/// `obelisk_index.enabled`; scoping the scan to the block is what keeps leaf
/// names from colliding across blocks. A block ends at the first non-empty,
/// non-comment line indented no further than the block header.
fn read_block_scalar(config_dir: &StdPath, block: &str, key: &str, default_value: &str) -> String {
    let header = format!("{block}:");
    for file_name in [SETTINGS_LOCAL_FILE, SETTINGS_DEFAULT_FILE] {
        let path = config_dir.join(file_name);
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let lines: Vec<&str> = contents.lines().collect();
        let Some(start) = lines.iter().position(|line| line.trim_start() == header) else {
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

fn read_obelisk_index_scalar(config_dir: &StdPath, key: &str, default_value: &str) -> String {
    read_block_scalar(config_dir, "obelisk_index", key, default_value)
}

fn read_block_u32(config_dir: &StdPath, block: &str, key: &str, default_value: u32) -> u32 {
    read_block_scalar(config_dir, block, key, "")
        .parse::<u32>()
        .unwrap_or(default_value)
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

/// Replace `key` under `relay:` with a single-line value, inserting it if absent.
///
/// The key's whole block is consumed, not just its first line. A setting written
/// in block style owns the indented lines beneath it:
///
/// ```yaml
/// prune_retention_by_kind:
///   1059: "30d"
/// ```
///
/// Overwriting only the `key:` line leaves those children stranded under a flow
/// mapping — `prune_retention_by_kind: {1059: "7d"}` followed by an orphaned
/// `  1059: "30d"` — which is not parseable YAML, so the relay refuses to start
/// on the next restart. That is reachable from the admin console today: the
/// block form is what `docs/retention.md` documents, so an operator who follows
/// the docs and then saves from the Storage screen bricks their own config.
fn upsert_relay_value(contents: String, key: &str, value: &str) -> String {
    let replacement = format!("  {key}: {value}");
    let mut found = false;
    let mut lines = Vec::new();

    let mut rest = contents.lines().peekable();
    while let Some(line) = rest.next() {
        let trimmed = line.trim_start();
        if trimmed.starts_with(&format!("{key}:")) {
            lines.push(replacement.clone());
            found = true;

            // Drop the nested block this key used to own, if it had one. Blank
            // lines and anything indented no further than the key belong to the
            // parent mapping and are left alone.
            let indent = line.len() - trimmed.len();
            while let Some(next) = rest.peek() {
                let next_trimmed = next.trim_start();
                if next_trimmed.is_empty() || next.len() - next_trimmed.len() <= indent {
                    break;
                }
                rest.next();
            }
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

/// Write one child of a nested `relay:` block, creating the block if needed.
///
/// Replaces a single child line in place, so sibling children, their comments
/// and the block's own comments all survive. This is the difference between
/// this and `upsert_relay_value`, which owns a key's whole indented subtree and
/// would delete it -- which is why `wot` had to be written as a one-line flow
/// map and cannot be read back out of the file.
fn upsert_block_value(contents: String, block: &str, key: &str, value: &str) -> String {
    let header = format!("{block}:");
    let mut lines: Vec<String> = contents.lines().map(str::to_string).collect();
    let block_start = lines
        .iter()
        .position(|line| line.trim_start() == header)
        .unwrap_or_else(|| {
            // `websocket:` is where `upsert_relay_value` inserts new flat keys,
            // so put a new block before it to stay out of that path. If there is
            // no websocket block, append.
            let insert_at = lines
                .iter()
                .position(|line| line.trim_start().starts_with("websocket:"))
                .unwrap_or(lines.len());
            lines.insert(insert_at, format!("  {header}"));
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

fn upsert_obelisk_index_value(contents: String, key: &str, value: &str) -> String {
    upsert_block_value(contents, "obelisk_index", key, value)
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
    refresh_wot_roots(state);
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
        relay_icon: read_yaml_scalar(
            config_dir,
            "relay_icon",
            state.relay_icon.as_deref().unwrap_or(""),
        ),
        relay_url: read_yaml_scalar(config_dir, "relay_url", &state.relay_url),
        relay_pubkey: state.relay_pubkey.clone(),
        restart_required,
    }
}

/// Largest accepted `data:` icon, before base64 expansion. The value is
/// inlined into settings.local.yml and echoed in every NIP-11 response, so it
/// has to stay small — this is a favicon, not an image host.
const MAX_ICON_DATA_URI_BYTES: usize = 256 * 1024;

/// Validate a relay icon before it is persisted and advertised.
///
/// Accepts an empty string (unset), an `https://`/`http://` URL, or a
/// `data:image/...;base64,` URI. Anything else is rejected rather than stored:
/// this value is served to every client that reads the NIP-11 document and is
/// injected into the admin page's `<link rel="icon">`, so `javascript:` and
/// other schemes must never reach it.
fn validate_relay_icon(icon: &str) -> Result<String, (StatusCode, Json<ErrorResponse>)> {
    if icon.is_empty() {
        return Ok(String::new());
    }

    if let Some(rest) = icon.strip_prefix("data:") {
        if !rest.starts_with("image/") {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "Icon data URI must be an image",
            ));
        }
        if !rest.contains(";base64,") {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "Icon data URI must be base64 encoded",
            ));
        }
        if icon.len() > MAX_ICON_DATA_URI_BYTES {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "Icon is too large; use an image under 256 KB or link to a URL",
            ));
        }
        return Ok(icon.to_string());
    }

    if icon.starts_with("https://") || icon.starts_with("http://") {
        // Reject embedded quotes/newlines: this is written into YAML and into
        // an HTML attribute.
        if icon.contains(['"', '\'', '\n', '\r', '<', '>']) {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "Icon URL contains invalid characters",
            ));
        }
        return Ok(icon.to_string());
    }

    Err(error_response(
        StatusCode::BAD_REQUEST,
        "Icon must be an https:// URL or a data:image/... URI",
    ))
}

fn persist_relay_identity_settings(
    config_dir: &StdPath,
    relay_name: &str,
    relay_description: &str,
    relay_url: &str,
    relay_icon: &str,
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
    let contents = upsert_relay_value(contents, "relay_icon", &yaml_quote(relay_icon));
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
    let (total_pruned, runs, last_run_unix, deleted_by_kind) = match &state.pruner_stats {
        Some(s) => (
            s.total_pruned.load(Ordering::Relaxed),
            s.runs.load(Ordering::Relaxed),
            s.last_run_unix.load(Ordering::Relaxed),
            Some(s.per_kind_snapshot()),
        ),
        None => (0, 0, 0, None),
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
        policies_secs: state.pruner_config.as_ref().map(|c| c.policies_as_secs()),
        deleted_by_kind,
        total_pruned,
        admin_deleted_total: ADMIN_DELETED_TOTAL.load(Ordering::Relaxed),
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

/// Record the relay owner. Written once at setup and never cleared by the
/// config reset, which keeps ownership across a reconfigure.
fn persist_relay_owner(config_dir: &StdPath, owner: PublicKey) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(config_dir)?;
    let json =
        serde_json::to_string_pretty(&owner.to_hex()).map_err(|e| std::io::Error::other(e))?;
    std::fs::write(config_dir.join(RELAY_OWNER_FILE), json)
}

pub fn load_relay_owner(config_dir: &StdPath) -> Option<PublicKey> {
    let contents = std::fs::read_to_string(config_dir.join(RELAY_OWNER_FILE)).ok()?;
    let hex: String = serde_json::from_str(&contents).ok()?;
    PublicKey::from_hex(&hex).ok()
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

// --- Connection / capacity limits ---

/// Defaults mirror `config.rs`; kept here so a missing key reads back as the
/// value the relay would actually use rather than as zero.
const DEFAULT_MAX_CONNECTIONS: u32 = 1000;
const DEFAULT_MAX_CONNECTIONS_PER_IP: u32 = 32;
const DEFAULT_MAX_CONNECTION_DURATION_MINUTES: u32 = 10;
const DEFAULT_IDLE_TIMEOUT_MINUTES: u32 = 10;
const DEFAULT_MAX_SUBSCRIPTIONS: u32 = 50;
const DEFAULT_MAX_LIMIT: u32 = 500;

/// Upper bounds on what the form may set.
///
/// Not arbitrary: these are the values past which a setting stops being a limit.
/// A relay configured for a million connections has no connection limit, and
/// saying so at save time is kinder than discovering it when the box runs out of
/// file descriptors.
const MAX_ALLOWED_CONNECTIONS: u32 = 100_000;
const MAX_ALLOWED_SUBSCRIPTIONS: u32 = 10_000;
const MAX_ALLOWED_LIMIT: u32 = 10_000;
const MAX_ALLOWED_TIMEOUT_MINUTES: u32 = 60 * 24 * 7;

fn connection_settings_response(
    state: &Arc<ServerState>,
    restart_required: bool,
) -> ConnectionSettingsResponse {
    let dir = StdPath::new(&state.config_dir);
    ConnectionSettingsResponse {
        max_connections: read_block_u32(
            dir,
            "websocket",
            "max_connections",
            DEFAULT_MAX_CONNECTIONS,
        ),
        max_connections_per_ip: read_block_u32(
            dir,
            "websocket",
            "max_connections_per_ip",
            DEFAULT_MAX_CONNECTIONS_PER_IP,
        ),
        max_connection_duration_minutes: parse_duration_minutes(
            &read_block_scalar(dir, "websocket", "max_connection_duration", ""),
            DEFAULT_MAX_CONNECTION_DURATION_MINUTES,
        ),
        idle_timeout_minutes: parse_duration_minutes(
            &read_block_scalar(dir, "websocket", "idle_timeout", ""),
            DEFAULT_IDLE_TIMEOUT_MINUTES,
        ),
        max_subscriptions: read_yaml_u32(dir, "max_subscriptions", DEFAULT_MAX_SUBSCRIPTIONS),
        max_limit: read_yaml_u32(dir, "max_limit", DEFAULT_MAX_LIMIT),
        force_public_groups: config_bool(dir, "force_public_groups", false),
        running_force_public_groups: state.http_state.groups.force_public_groups,
        running_max_connections: state.connection_limiter.configured_max_total().unwrap_or(0)
            as u32,
        running_max_connections_per_ip: state.connection_limiter.configured_max_per_ip() as u32,
        active_connections: state.connection_limiter.active() as u32,
        restart_required,
    }
}

async fn handle_connection_settings(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    Ok(Json(connection_settings_response(&state, false)))
}

async fn handle_connection_settings_update(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(req): Json<ConnectionSettingsRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    // Zero is the dangerous input throughout: a zero connection cap locks
    // everyone out including the operator, and a zero timeout disconnects every
    // client immediately. Neither is recoverable from this console afterwards.
    let checks: [(&str, u32, u32); 6] = [
        (
            "Max connections",
            req.max_connections,
            MAX_ALLOWED_CONNECTIONS,
        ),
        (
            "Max connections per IP",
            req.max_connections_per_ip,
            MAX_ALLOWED_CONNECTIONS,
        ),
        (
            "Max connection duration",
            req.max_connection_duration_minutes,
            MAX_ALLOWED_TIMEOUT_MINUTES,
        ),
        (
            "Idle timeout",
            req.idle_timeout_minutes,
            MAX_ALLOWED_TIMEOUT_MINUTES,
        ),
        (
            "Max subscriptions",
            req.max_subscriptions,
            MAX_ALLOWED_SUBSCRIPTIONS,
        ),
        ("Max limit", req.max_limit, MAX_ALLOWED_LIMIT),
    ];
    for (label, value, ceiling) in checks {
        if value == 0 {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                &format!("{label} must be at least 1"),
            ));
        }
        if value > ceiling {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                &format!("{label} must be at most {ceiling}"),
            ));
        }
    }

    if req.max_connections_per_ip > req.max_connections {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Per-IP limit cannot exceed the total connection limit",
        ));
    }

    // Turning this on is a one-way door: on the next start every stored group
    // has `private` and `hidden` cleared, and nothing restores them. A checkbox
    // is not enough friction for that, so require the phrase -- but only in the
    // direction that destroys information.
    let currently_on = config_bool(
        StdPath::new(&state.config_dir),
        "force_public_groups",
        false,
    );
    if req.force_public_groups
        && !currently_on
        && req.force_public_confirm.trim().to_uppercase() != "FORCE PUBLIC"
    {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Type FORCE PUBLIC to confirm making every existing group public",
        ));
    }

    persist_connection_settings(StdPath::new(&state.config_dir), &req).map_err(|e| {
        warn!("Failed to persist connection settings: {}", e);
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to persist connection settings",
        )
    })?;

    Ok(Json(connection_settings_response(&state, true)))
}

fn persist_connection_settings(
    config_dir: &StdPath,
    req: &ConnectionSettingsRequest,
) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(config_dir)?;
    let path = config_dir.join(SETTINGS_LOCAL_FILE);
    let contents = std::fs::read_to_string(&path).unwrap_or_else(|_| "relay:\n".to_string());

    // The websocket trio are children of a nested block, so they go through the
    // block-aware writer -- `upsert_relay_value` would swallow the block and the
    // comments in it. The other two are flat `relay:` scalars.
    let contents = upsert_block_value(
        contents,
        "websocket",
        "max_connections",
        &req.max_connections.to_string(),
    );
    let contents = upsert_block_value(
        contents,
        "websocket",
        "max_connections_per_ip",
        &req.max_connections_per_ip.to_string(),
    );
    let contents = upsert_block_value(
        contents,
        "websocket",
        "max_connection_duration",
        &yaml_quote(&format!("{}m", req.max_connection_duration_minutes)),
    );
    let contents = upsert_block_value(
        contents,
        "websocket",
        "idle_timeout",
        &yaml_quote(&format!("{}m", req.idle_timeout_minutes)),
    );
    let contents = upsert_relay_scalar(contents, "max_subscriptions", req.max_subscriptions);
    let contents = upsert_relay_scalar(contents, "max_limit", req.max_limit);
    let contents = upsert_relay_value(
        contents,
        "force_public_groups",
        if req.force_public_groups {
            "true"
        } else {
            "false"
        },
    );

    std::fs::write(path, contents)
}

// --- Moderation reports (NIP-56) ---

#[derive(Deserialize)]
struct ReportsQuery {
    /// "open" (default), "resolved", or "all".
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Deserialize)]
struct ResolveReportRequest {
    /// The case key from the listing, e.g. "e:<id>" or "p:<hex>".
    key: String,
    /// dismiss | delete_event | remove_from_group | blacklist
    action: String,
    #[serde(default)]
    note: String,
    /// Required for remove_from_group; ignored otherwise.
    #[serde(default)]
    group_id: Option<String>,
}

async fn handle_reports_list(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Query(params): Query<ReportsQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let limit = params.limit.unwrap_or(500).min(2000);
    let cases = state
        .http_state
        .groups
        .admin_get_reports(&state.reports, &state.report_evidence, limit)
        .await
        .map_err(|e| error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;

    let status = params.status.as_deref().unwrap_or("open");
    let filtered: Vec<_> = match status {
        "all" => cases,
        "resolved" => cases
            .into_iter()
            .filter(|c| c.resolution.is_some())
            .collect(),
        // Default to the work queue: what still needs a decision.
        _ => cases
            .into_iter()
            .filter(|c| c.resolution.is_none())
            .collect(),
    };

    Ok(Json(serde_json::json!({
        "cases": filtered,
        "resolved_total": state.reports.len(),
    })))
}

async fn handle_report_resolve(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(req): Json<ResolveReportRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    let admin_pubkey = validate_session(&admin_state, &headers).ok_or_else(unauthorized)?;

    let action = crate::reports::ResolutionAction::parse(&req.action)
        .map_err(|e| error_response(StatusCode::BAD_REQUEST, &e.to_string()))?;

    // Reconstruct the target from the key the listing handed out, so the client
    // cannot invent a shape the server never offered.
    let target = match req.key.split_once(':') {
        Some(("e", id)) => crate::reports::ReportTarget::Event { id: id.to_string() },
        Some(("p", hex)) => crate::reports::ReportTarget::Pubkey {
            hex: hex.to_string(),
        },
        _ => {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "Malformed report key",
            ))
        }
    };

    // Carry out the action before recording it. If the action fails, the case
    // stays open -- a queue that says "blacklisted" about someone who is not
    // blacklisted is worse than one that still has work in it.
    apply_report_action(&state, &action, &target, req.group_id.as_deref()).await?;

    state.reports.resolve(
        &target,
        crate::reports::Resolution {
            action: action.clone(),
            resolved_by: admin_pubkey.to_hex(),
            resolved_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            note: req.note.clone(),
        },
    );

    if let Err(e) = state.reports.persist(StdPath::new(&state.config_dir)) {
        // The decision is applied and held in memory; failing to write it means
        // a restart would requeue it. Worth a loud log, not a failed request.
        warn!("Failed to persist report resolution: {}", e);
    }

    info!(
        "Admin {} resolved report {} as {:?}",
        admin_pubkey, req.key, action
    );

    Ok(Json(serde_json::json!({ "resolved": true })))
}

/// Carry out what the admin decided, reusing the existing moderation paths.
async fn apply_report_action(
    state: &Arc<ServerState>,
    action: &crate::reports::ResolutionAction,
    target: &crate::reports::ReportTarget,
    group_id: Option<&str>,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    use crate::reports::{ReportTarget, ResolutionAction};

    match (action, target) {
        // A false positive: record the judgement, touch nothing.
        (ResolutionAction::Dismissed, _) => Ok(()),

        (ResolutionAction::DeletedEvent, ReportTarget::Event { id }) => state
            .http_state
            .groups
            .admin_delete_event(id)
            .await
            .map_err(|e| error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string())),
        (ResolutionAction::DeletedEvent, ReportTarget::Pubkey { .. }) => Err(error_response(
            StatusCode::BAD_REQUEST,
            "This report names a person, not an event; there is nothing to delete",
        )),

        (ResolutionAction::RemovedFromGroup, _) => {
            let group_id = group_id.ok_or_else(|| {
                error_response(
                    StatusCode::BAD_REQUEST,
                    "Removing from a group needs to know which group",
                )
            })?;
            let hex = report_subject_pubkey(state, target).await?;
            state
                .http_state
                .groups
                .admin_remove_group_member(group_id, &hex)
                .await
                .map_err(|e| error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))
        }

        (ResolutionAction::Blacklisted, _) => {
            let hex = report_subject_pubkey(state, target).await?;
            let pubkey = PublicKey::from_hex(&hex)
                .map_err(|_| error_response(StatusCode::BAD_REQUEST, "Invalid pubkey"))?;

            // The relay's own key must never be bannable from the relay.
            if pubkey == state.relay_public_key {
                return Err(error_response(
                    StatusCode::BAD_REQUEST,
                    "Refusing to blacklist the relay's own key",
                ));
            }
            if state.admin_pubkeys.contains(&pubkey) {
                return Err(error_response(
                    StatusCode::BAD_REQUEST,
                    "Refusing to blacklist a relay admin; remove them as admin first",
                ));
            }

            state.whitelist.blacklist().add(pubkey);
            state
                .whitelist
                .blacklist()
                .persist(StdPath::new(&state.config_dir))
                .map_err(|e| error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
            Ok(())
        }
    }
}

/// The person a case is about: the pubkey directly, or the author of the
/// reported event.
async fn report_subject_pubkey(
    state: &Arc<ServerState>,
    target: &crate::reports::ReportTarget,
) -> Result<String, (StatusCode, Json<ErrorResponse>)> {
    match target {
        crate::reports::ReportTarget::Pubkey { hex } => Ok(hex.clone()),
        crate::reports::ReportTarget::Event { id } => {
            let event_id = EventId::from_hex(id)
                .map_err(|_| error_response(StatusCode::BAD_REQUEST, "Invalid event id"))?;
            state
                .http_state
                .groups
                .admin_find_event_author(&event_id)
                .await
                .map_err(|e| error_response(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?
                // Deleted since, but the relay saw it when it was reported --
                // that snapshot names the real author, so the case is still
                // actionable. Deleting the evidence must not be a way out.
                .or_else(|| state.report_evidence.get(id).map(|e| e.author))
                .ok_or_else(|| {
                    error_response(
                        StatusCode::NOT_FOUND,
                        "The reported event is no longer stored, so its author cannot be determined",
                    )
                })
        }
    }
}

// --- Access tiers ---

/// One account in a tier listing.
#[derive(Serialize)]
struct TierEntry {
    hex: String,
    npub: String,
    /// Hops from a reference account. `None` for Tier 1, which is not a distance.
    hops: Option<u8>,
    /// Why this account is in this tier: `manual`, `follow_sync`, or
    /// `web_of_trust`. Tier 1 is two different things and an operator removing
    /// an entry needs to know which -- a hand-added key is removed by hand, a
    /// follow-derived one comes back on the next sync.
    source: &'static str,
}

#[derive(Serialize)]
struct TierPageResponse {
    tier: u8,
    /// Total in this tier after any search filter, not the page length.
    total: usize,
    /// The graph ran out of fetch budget, so this tier is a floor rather than a
    /// count. The UI must say "at least N".
    truncated: bool,
    /// Deepest hop the graph can answer for with confidence.
    complete_to_hop: u8,
    entries: Vec<TierEntry>,
}

#[derive(Deserialize)]
struct TierQuery {
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
    /// Substring match on hex or npub. Name search stays in the browser, which
    /// is where the profile cache lives.
    #[serde(default)]
    q: Option<String>,
}

/// One page of the accounts in an access tier.
///
/// Tier 1 is hand-added plus follow-derived -- the two backend tiers whose
/// budget is identical and which an operator thinks of as "people I vouch for".
/// Tier 2 and 3 are web-of-trust hop counts, served from the distance map the
/// graph retains at rebuild; see `wot_graph::FollowGraph::admitted`. Nothing
/// here re-runs the breadth-first search.
async fn handle_access_tier(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(tier): Path<u8>,
    Query(params): Query<TierQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let limit = params
        .limit
        .unwrap_or(100)
        .min(crate::wot_graph::ADMITTED_PAGE_MAX);
    let offset = params.offset.unwrap_or(0);
    let needle = params
        .q
        .as_deref()
        .map(|q| q.trim().to_ascii_lowercase())
        .filter(|q| !q.is_empty());

    let blacklist = state.whitelist.blacklist();

    if tier == 1 {
        // Tier 1 is everyone the relay trusts at full budget: hand-added, plus
        // the reference accounts themselves, plus anyone they follow.
        //
        // The graph hops 0 and 1 belong here and not in a tier of their own.
        // `budget_percent` already says so -- `Manual`, `FollowSync` and
        // `WebOfTrust(0..=1)` all sit at 100% with a comment that follow sync
        // *is* the one-hop set. Leaving them out made the tiers fail to
        // partition the admitted set: on a relay whose roots are pinned
        // directly rather than via reference accounts, follow sync never runs,
        // so the root and everyone it follows appeared in no tier at all.
        let mut rows: Vec<(PublicKey, &'static str)> = state
            .whitelist
            .list_manual()
            .into_iter()
            .map(|pk| (pk, "manual"))
            .collect();
        for pk in state.whitelist.list_follow_derived() {
            if !rows.iter().any(|(existing, _)| *existing == pk) {
                rows.push((pk, "follow_sync"));
            }
        }
        if let Some(graph) = state.whitelist.wot().and_then(|o| o.graph().cloned()) {
            for hops in [0u8, 1u8] {
                // Whole-tier pages: this is bounded by the root set and its
                // follows, not by the six-figure outer hops.
                let (_, page) =
                    graph.admitted_page(hops, 0, crate::wot_graph::ADMITTED_PAGE_MAX, None);
                for (pk, h) in page {
                    debug_assert_eq!(crate::whitelist::AccessTier::tier_for_hops(h), 1);
                    if !rows.iter().any(|(existing, _)| *existing == pk) {
                        rows.push((
                            pk,
                            if h == 0 {
                                "reference"
                            } else {
                                "follows_reference"
                            },
                        ));
                    }
                }
            }
        }

        // The blacklist overrides every tier, so a blocked key is not "in"
        // Tier 1 however it got there.
        rows.retain(|(pk, _)| !blacklist.contains(pk));

        if let Some(n) = &needle {
            rows.retain(|(pk, _)| {
                pk.to_hex().contains(n)
                    || pk
                        .to_bech32()
                        .map(|npub: String| npub.to_ascii_lowercase().contains(n))
                        .unwrap_or(false)
            });
        }

        let total = rows.len();
        let entries = rows
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|(pk, source)| TierEntry {
                hex: pk.to_hex(),
                npub: pk.to_bech32().unwrap_or_default(),
                hops: None,
                source,
            })
            .collect();

        return Ok(Json(TierPageResponse {
            tier: 1,
            total,
            truncated: false,
            complete_to_hop: u8::MAX,
            entries,
        }));
    }

    // Tier 2 and up are hop counts in the follow graph.
    let Some(oracle) = state.whitelist.wot() else {
        return Ok(Json(TierPageResponse {
            tier,
            total: 0,
            truncated: false,
            complete_to_hop: 0,
            entries: Vec::new(),
        }));
    };
    let Some(graph) = oracle.graph() else {
        // Oracle mode: distances come from a remote service and there is no
        // local set to enumerate. The tier is testable, not listable.
        return Ok(Json(TierPageResponse {
            tier,
            total: 0,
            truncated: true,
            complete_to_hop: 0,
            entries: Vec::new(),
        }));
    };

    let coverage = graph.coverage();
    let (total, page) = graph.admitted_page(tier, offset, limit, needle.as_deref());

    let entries = page
        .into_iter()
        .filter(|(pk, _)| !blacklist.contains(pk))
        .map(|(pk, hops)| TierEntry {
            hex: pk.to_hex(),
            npub: pk.to_bech32().unwrap_or_default(),
            hops: Some(hops),
            source: "web_of_trust",
        })
        .collect();

    Ok(Json(TierPageResponse {
        tier,
        // Only the outermost hop is affected by the fetch budget running out;
        // an inner hop is complete regardless.
        truncated: coverage.truncated && tier >= coverage.complete_to_hop,
        complete_to_hop: coverage.complete_to_hop,
        total,
        entries,
    }))
}

/// Tier sizes in one call, for the Access screen header and the Overview.
///
/// Exists so the console does not have to fetch four pages just to render four
/// counts, and so every surface showing "who can connect" is reading the same
/// numbers from the same place.
async fn handle_access_tier_summary(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let blacklist = state.whitelist.blacklist();
    let manual = state.whitelist.list_manual().len();
    let follow_derived = state
        .whitelist
        .list_follow_derived()
        .into_iter()
        .filter(|pk| !blacklist.contains(pk))
        .count();

    #[allow(clippy::type_complexity)]
    let (wot_total, per_hop, truncated, complete_to_hop, max_hops) = match state.whitelist.wot() {
        Some(oracle) => match oracle.graph() {
            Some(graph) => {
                let (total, per_hop) = graph.admitted_totals();
                let c = graph.coverage();
                (
                    total,
                    per_hop,
                    c.truncated,
                    c.complete_to_hop,
                    oracle.max_hops(),
                )
            }
            None => (0, Vec::new(), true, 0, oracle.max_hops()),
        },
        None => (0, Vec::new(), false, 0, 0),
    };

    // Hops 0 and 1 count toward Tier 1, matching `handle_access_tier`. Counted
    // from the graph rather than added blindly, because a key can be both
    // hand-added and one hop away and must not be counted twice.
    let near_graph = per_hop
        .iter()
        .filter(|(h, _)| *h <= 1)
        .map(|(_, n)| *n)
        .sum::<usize>();

    Ok(Json(serde_json::json!({
        "tier1": {
            "manual": manual,
            "follow_sync": follow_derived,
            "near_graph": near_graph,
            // Not a sum: the same key can appear in more than one source, so
            // the authoritative figure is what the tier page reports.
            "total": std::cmp::max(manual + follow_derived, near_graph),
        },
        "wot": {
            "enabled": state.whitelist.wot().is_some(),
            "total": wot_total,
            "per_hop": per_hop.iter().map(|(h, n)| serde_json::json!({ "hops": h, "count": n })).collect::<Vec<_>>(),
            "max_hops": max_hops,
            "truncated": truncated,
            "complete_to_hop": complete_to_hop,
        },
        "blocked": blacklist.list().len(),
        // Whether admission is enforced at all. An empty Tier 1 with no WoT
        // means everyone is in, which is a different screen entirely.
        "open_relay": state.whitelist.is_empty(),
    })))
}

// --- Routes ---

/// Reject anything without a valid admin session, before the handler runs.
///
/// Every protected handler still performs its own `validate_session` check, and
/// those are deliberately left in place -- this is a second, structural gate, not
/// a replacement. The per-handler checks are ~45 hand-written copies, correct
/// today, but correctness maintained by memory is a defect waiting for the next
/// route to be added. With this layer, forgetting one is no longer exploitable.
///
/// It also fixes an ordering bug: Axum runs the `Json` extractor before the
/// handler body, so an unauthenticated request with a malformed body used to get
/// `422 Unprocessable Entity` instead of `401`. A layer runs before extraction,
/// so the status is now right.
async fn require_admin_session(
    State(state): State<Arc<ServerState>>,
    req: Request,
    next: Next,
) -> Response {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, req.headers()).is_none() {
        return unauthorized().into_response();
    }
    next.run(req).await
}

pub fn admin_routes(state: Arc<ServerState>) -> Router<Arc<ServerState>> {
    // Unauthenticated by design: these are how a client *obtains* a session, and
    // `/setup` is guarded instead by `admin_pubkeys` being empty.
    let public = Router::new()
        .route("/setup/status", get(handle_setup_status))
        .route("/setup", post(handle_setup))
        .route("/challenge", get(handle_challenge))
        .route("/auth", post(handle_auth));

    let protected = Router::new()
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
            "/connection-settings",
            get(handle_connection_settings).post(handle_connection_settings_update),
        )
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
        .route("/session", get(handle_session_check))
        .route("/whitelist", get(handle_whitelist_list))
        .route("/whitelist/sources", get(handle_access_sources))
        .route("/whitelist/check", get(handle_access_check))
        .route("/access/tiers", get(handle_access_tier_summary))
        .route("/access/tier/{tier}", get(handle_access_tier))
        .route("/whitelist", post(handle_whitelist_add))
        .route("/whitelist/{hex}", delete(handle_whitelist_remove))
        .route("/retention", get(handle_retention_status))
        .route("/groups", get(handle_groups))
        .route("/groups/{id}", delete(handle_group_delete))
        .route("/stats", get(handle_stats))
        .route("/storage/stats", get(handle_storage_stats))
        .route("/storage/exact-count", get(handle_exact_kind_count))
        .route("/storage/history", get(handle_storage_history))
        .route(
            "/storage/kinds/{kind}/authors",
            get(handle_storage_kind_authors),
        )
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
        .route(
            "/wot",
            get(handle_wot_status).post(handle_wot_settings_update),
        )
        .route("/storage/prune", post(handle_storage_prune))
        .route("/storage/compaction", get(handle_compaction_status))
        .route("/storage/compact", post(handle_compact_database))
        .route("/version", get(handle_version))
        .route(
            "/update",
            get(handle_update_status).post(handle_update_request),
        )
        .route("/groups/{id}/events", get(handle_group_events))
        .route("/events/{event_id}", delete(handle_event_delete))
        .route("/events/delete", post(handle_events_bulk_delete))
        .route(
            "/events/delete-by-recipient",
            post(handle_events_delete_by_recipient),
        )
        .route(
            "/users/delete-events",
            post(handle_users_events_bulk_delete),
        )
        .route(
            "/groups/{id}/members/{pubkey}",
            delete(handle_group_member_remove),
        )
        .route("/groups/{id}/members", get(handle_group_members))
        .route("/reports", get(handle_reports_list))
        .route("/reports/resolve", post(handle_report_resolve))
        .route("/users/{pubkey}/events", delete(handle_user_events_delete))
        // route_layer, not layer: it runs only for routes this router matched, so
        // an unknown /api/admin path still 404s rather than reporting 401.
        .route_layer(middleware::from_fn_with_state(state, require_admin_session));

    public.merge(protected)
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

        refresh_wot_roots(&state);

        // Sync the owner's follows immediately rather than waiting for someone
        // to find the button. Making them a reference account only declares
        // where trust starts; until the sync runs, nothing is derived from it
        // and a freshly set-up relay admits nobody but the owner -- which reads
        // as the relay being broken on the very first visit.
        let whitelist = state.whitelist.clone();
        let reference_accounts = state.reference_accounts.clone();
        let config_dir = state.config_dir.clone();
        tokio::spawn(async move {
            let roots = reference_accounts.list();
            if roots.is_empty() {
                return;
            }
            info!("Setup complete: syncing the owner's follows");
            match follow_sync::sync_follows(&roots).await {
                Ok(follows) => {
                    let count = follows.len();
                    whitelist.set_follow_derived(follows.clone());
                    if let Err(e) =
                        follow_sync::persist_follow_derived(&follows, StdPath::new(&config_dir))
                    {
                        warn!("Failed to persist follow-derived whitelist: {}", e);
                    }
                    info!("Setup follow sync complete: {count} pubkeys whitelisted");
                }
                Err(e) => warn!("Setup follow sync failed: {}", e),
            }
        });
    }

    // Ownership is recorded before the transient setup marker is cleared, so
    // the two cannot both be lost.
    if let Err(e) = persist_relay_owner(StdPath::new(&state.config_dir), event.pubkey) {
        warn!("Failed to persist relay owner: {}", e);
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
    refresh_wot_roots(&state);
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

    let config_dir = StdPath::new(&state.config_dir);
    let mut owner = load_relay_owner(config_dir);

    // Relays set up before ownership was recorded have no owner file. With
    // exactly one admin there is no ambiguity about who that is, so adopt them
    // and write it down. With several, leave it unset rather than guess.
    if owner.is_none() {
        let admins = admin_state.admin_pubkeys.read().clone();
        if let [only] = admins.as_slice() {
            if persist_relay_owner(config_dir, *only).is_ok() {
                owner = Some(*only);
            }
        }
    }

    let mut entries: Vec<AdminPubkeyEntry> = admin_state
        .admin_pubkeys
        .read()
        .iter()
        .map(|pk| AdminPubkeyEntry {
            hex: pk.to_hex(),
            npub: pk.to_bech32().unwrap_or_default(),
            current_session: *pk == current_pubkey,
            owner: owner == Some(*pk),
        })
        .collect();

    // Owner first; the rest keep their existing order.
    entries.sort_by_key(|e| !e.owner);

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
        // A newly granted admin is never the owner; ownership is set at setup.
        owner: false,
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

    let relay_icon = validate_relay_icon(req.relay_icon.trim())?;

    persist_relay_identity_settings(
        StdPath::new(&state.config_dir),
        req.relay_name.trim(),
        req.relay_description.trim(),
        req.relay_url.trim(),
        &relay_icon,
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
        relay_icon,
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

/// Disk usage over time, so growth is visible rather than inferred from a
/// single current number. See `crate::storage_history`.
async fn handle_storage_history(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let mut history = crate::storage_history::load(&state.config_dir);

    // Always append the live figure so the graph ends at "now" rather than at
    // the last tick, which can be up to a sampling interval stale.
    let now_bytes = crate::storage_history::measure_db_bytes(&state.db_path);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    if now_bytes > 0 {
        history.samples.push(crate::storage_history::StorageSample {
            connections: None,
            at: now,
            db_bytes: now_bytes,
        });
    }

    Ok(Json(history))
}

/// What a compaction would reclaim, and what earlier ones did.
///
/// The measurement runs in a child process. It has to: heed refuses a second
/// open of a path already open in this process, and LMDB forbids it outright, so
/// the relay cannot measure the database it is currently serving from. See
/// `src/bin/lmdb_stat.rs`.
#[derive(Serialize, Clone)]
struct CompactionStatusResponse {
    /// Size of `data.mdb` on disk.
    db_file_bytes: u64,
    /// Bytes in pages actually in use, or `None` if the measurement failed.
    live_bytes: Option<u64>,
    /// Free-list slack — what a compaction would hand back to the filesystem.
    reclaimable_bytes: Option<u64>,
    /// Free space on the filesystem holding the database.
    free_disk_bytes: Option<u64>,
    /// Free space a compaction needs before it will start.
    required_free_bytes: u64,
    /// Whether a compaction would be allowed to run right now.
    can_compact: bool,
    /// Why not, when `can_compact` is false.
    blocked_reason: Option<String>,
    /// Whether a compaction is already staged for the next start.
    pending: bool,
    measured_at: i64,
    /// Why the measurement is missing, if it is.
    measure_error: Option<String>,
    /// Most recent runs, newest last.
    history: Vec<crate::compaction::CompactionEntry>,
}

/// Measuring reads the whole free list; on a multi-GB database that is seconds,
/// not milliseconds, and the Storage screen polls.
const COMPACTION_MEASURE_TTL_SECS: i64 = 60;

static COMPACTION_MEASUREMENT: OnceCell<RwLock<Option<CompactionMeasurement>>> = OnceCell::new();

#[derive(Clone)]
struct CompactionMeasurement {
    measurement: Option<crate::compaction::Measurement>,
    free_disk_bytes: Option<u64>,
    error: Option<String>,
    measured_at: i64,
}

/// Where `lmdb_stat` lives. It sits beside the relay binary in the image
/// (`/app/lmdb_stat`) and in `target/<profile>/` during development, so resolve
/// it relative to the running executable and fall back to `PATH`.
fn lmdb_stat_path() -> std::path::PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("lmdb_stat")))
        .filter(|path| path.exists())
        .unwrap_or_else(|| std::path::PathBuf::from("lmdb_stat"))
}

#[derive(Deserialize)]
struct LmdbStatOutput {
    file_bytes: u64,
    live_bytes: Option<u64>,
    reclaimable_bytes: Option<u64>,
    measured_at: i64,
    free_disk_bytes: Option<u64>,
}

async fn measure_database(db_path: &str) -> CompactionMeasurement {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let failed = |error: String| CompactionMeasurement {
        measurement: None,
        // Free space is still worth reporting when the database could not be
        // read — it is half of why a compaction gets refused.
        free_disk_bytes: crate::compaction::free_disk_bytes(db_path),
        error: Some(error),
        measured_at: now,
    };

    let run = tokio::process::Command::new(lmdb_stat_path())
        .arg("--db")
        .arg(db_path)
        .output();

    // A free-list walk on a large database is seconds; anything beyond this is
    // a wedged child, and the admin request must not hang on it.
    let output = match tokio::time::timeout(std::time::Duration::from_secs(60), run).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return failed(format!("could not run lmdb_stat: {e}")),
        Err(_) => return failed("measuring the database timed out".to_string()),
    };

    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return failed(if detail.is_empty() {
            "lmdb_stat failed".to_string()
        } else {
            detail
        });
    }

    match serde_json::from_slice::<LmdbStatOutput>(&output.stdout) {
        Ok(parsed) => CompactionMeasurement {
            measurement: Some(crate::compaction::Measurement {
                file_bytes: parsed.file_bytes,
                live_bytes: parsed.live_bytes,
                reclaimable_bytes: parsed.reclaimable_bytes,
                measured_at: parsed.measured_at,
            }),
            free_disk_bytes: parsed.free_disk_bytes,
            error: None,
            measured_at: now,
        },
        Err(e) => failed(format!("could not read lmdb_stat output: {e}")),
    }
}

/// Measure, reusing a recent result unless `refresh` asks for a fresh one.
async fn cached_measurement(db_path: &str, refresh: bool) -> CompactionMeasurement {
    let cell = COMPACTION_MEASUREMENT.get_or_init(|| RwLock::new(None));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    if !refresh {
        if let Some(cached) = cell.read().as_ref() {
            if now - cached.measured_at < COMPACTION_MEASURE_TTL_SECS {
                return cached.clone();
            }
        }
    }

    let fresh = measure_database(db_path).await;
    *cell.write() = Some(fresh.clone());
    fresh
}

/// Turn a measurement into the answer to "may I compact now, and why not".
fn compaction_status(
    state: &ServerState,
    measured: CompactionMeasurement,
) -> CompactionStatusResponse {
    let pending = crate::compaction::request_pending(&state.config_dir);

    let (db_file_bytes, live_bytes, reclaimable_bytes, required_free_bytes) =
        match measured.measurement.as_ref() {
            Some(m) => (
                m.file_bytes,
                m.live_bytes,
                m.reclaimable_bytes,
                crate::compaction::required_free_bytes(m),
            ),
            None => (
                crate::storage_history::measure_db_bytes(&state.db_path),
                None,
                None,
                0,
            ),
        };

    // Ordered so the operator sees the most actionable reason first.
    let blocked_reason = if pending {
        Some("A compaction is already staged for the next restart.".to_string())
    } else if let Some(error) = measured.error.clone() {
        Some(format!("The database could not be measured: {error}"))
    } else if measured
        .free_disk_bytes
        .is_some_and(|free| free < required_free_bytes)
    {
        Some(format!(
            "Compaction needs {} free to copy the live data, but only {} is available.",
            human_bytes(required_free_bytes),
            human_bytes(measured.free_disk_bytes.unwrap_or(0))
        ))
    } else if reclaimable_bytes == Some(0) {
        Some("There is no free-list slack to reclaim.".to_string())
    } else {
        None
    };

    CompactionStatusResponse {
        db_file_bytes,
        live_bytes,
        reclaimable_bytes,
        free_disk_bytes: measured.free_disk_bytes,
        required_free_bytes,
        can_compact: blocked_reason.is_none(),
        blocked_reason,
        pending,
        measured_at: measured.measured_at,
        measure_error: measured.error,
        history: crate::compaction::load_log(&state.config_dir).entries,
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[derive(Deserialize)]
struct CompactionStatusQuery {
    /// Skip the cached measurement and walk the free list again.
    refresh: Option<bool>,
}

async fn handle_compaction_status(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Query(params): Query<CompactionStatusQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let measured = cached_measurement(&state.db_path, params.refresh.unwrap_or(false)).await;
    Ok(Json(compaction_status(&state, measured)))
}

#[derive(Deserialize)]
struct CompactRequest {
    confirm: String,
}

#[derive(Serialize)]
struct CompactResponse {
    message: String,
    restart_in_ms: u64,
    /// What the measurement said we should get back, so the UI can show a
    /// target while the relay is down.
    expected_reclaim_bytes: Option<u64>,
}

/// Stage a compaction and restart into it.
///
/// The relay cannot compact the database it is serving from: the copy is taken
/// from a snapshot, so every write landing afterwards would be lost when the
/// copy replaced the original. So this writes a request file and exits, exactly
/// like `handle_restart_relay`; `compaction::run_pending` does the work at the
/// next start, before the database is opened.
async fn handle_compact_database(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(req): Json<CompactRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    let admin_pubkey = validate_session(&admin_state, &headers).ok_or_else(unauthorized)?;

    // Case-insensitive, matching `confirmMatches` in the console. The friction
    // worth having is typing a specific word, not typing it in a specific case;
    // rejecting `compact` would read as a broken button.
    if !req.confirm.trim().eq_ignore_ascii_case("COMPACT") {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Type COMPACT to confirm",
        ));
    }

    // Measure fresh rather than trusting a cached figure: this is the check that
    // stands between the operator and a restart into a failed compaction.
    let measured = cached_measurement(&state.db_path, true).await;
    let status = compaction_status(&state, measured);
    if !status.can_compact {
        return Err(error_response(
            StatusCode::CONFLICT,
            &status
                .blocked_reason
                .unwrap_or_else(|| "Compaction is not possible right now".to_string()),
        ));
    }

    crate::compaction::stage_request(&state.config_dir, &admin_pubkey.to_hex()).map_err(|e| {
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            &format!("Could not stage the compaction: {e}"),
        )
    })?;

    info!(
        "Admin {} requested a database compaction ({} reclaimable)",
        admin_pubkey,
        human_bytes(status.reclaimable_bytes.unwrap_or(0))
    );

    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(750)).await;
        std::process::exit(0);
    });

    Ok(Json(CompactResponse {
        message: "Compaction staged; the relay is restarting to run it".to_string(),
        restart_in_ms: 750,
        expected_reclaim_bytes: status.reclaimable_bytes,
    }))
}

/// What this build is. Cheap, and polled through a restart by the update card,
/// so it does nothing but read three constants and an environment variable.
async fn handle_version(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    Ok(Json(crate::version::current()))
}

#[derive(Serialize)]
struct UpdateStatusResponse {
    running: crate::version::VersionInfo,
    /// The repository updates come from. Fixed — a request names a tag, never a
    /// registry.
    image_repository: &'static str,
    /// Published tags, newest first.
    available_tags: Vec<String>,
    /// Why the tag list is empty or stale. "Registry unreachable" and "nothing
    /// published" must not look the same.
    tags_error: Option<String>,
    tags_fetched_at: i64,
    /// Whether the tag this relay is running is one the registry publishes.
    ///
    /// False means a locally built image, which is the state both relays on this
    /// host were left in. It matters because the console compares the running
    /// tag against the newest published one: without this the comparison reads
    /// "different, therefore out of date" and offers an update that is really a
    /// downgrade to something older than what is installed.
    running_is_published: bool,
    /// Whether the host agent has checked in recently enough to act on a
    /// request. Without it, a request would sit unread and look like a success.
    agent_live: bool,
    agent_last_seen: Option<i64>,
    /// A request already written and not yet consumed.
    pending: Option<crate::update::UpdateRequest>,
    last_result: Option<crate::update::UpdateResult>,
    /// Whether an update can be requested right now.
    can_update: bool,
    blocked_reason: Option<String>,
}

#[derive(Deserialize)]
struct UpdateStatusQuery {
    /// Re-ask the registry instead of using the cached tag list.
    refresh: Option<bool>,
}

fn update_blocked_reason(
    agent_live: bool,
    pending: &Option<crate::update::UpdateRequest>,
    tags: &crate::update::PublishedTags,
) -> Option<String> {
    if let Some(request) = pending {
        return Some(format!(
            "An update to {} is already queued and has not been picked up yet.",
            request.requested_tag
        ));
    }
    if !agent_live {
        // No file path here. The console runs in a browser, and the relay serves
        // nothing but frontend/dist, so naming a repo file told an operator to
        // go and find something they had no way to open. The command is given in
        // full, and the console renders DOC_UPDATING_URL beside it as a link.
        return Some(
            "No update agent is running on the host, so an update cannot be carried out. \
             Run scripts/install-update-agent.sh on the host to install one."
                .to_string(),
        );
    }
    if tags.tags.is_empty() {
        return Some(match &tags.error {
            Some(error) => format!("The list of published versions could not be fetched: {error}"),
            None => "No versions are published for this relay's image.".to_string(),
        });
    }
    None
}

async fn handle_update_status(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Query(params): Query<UpdateStatusQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let tags = crate::update::published_tags(params.refresh.unwrap_or(false)).await;
    let agent = crate::update::agent_heartbeat(&state.config_dir);
    let agent_live = crate::update::agent_is_live(&state.config_dir);
    let pending = crate::update::pending_request(&state.config_dir);
    let blocked_reason = update_blocked_reason(agent_live, &pending, &tags);

    let running = crate::version::current();
    let running_is_published = running
        .image_tag
        .as_ref()
        .is_some_and(|tag| tags.tags.iter().any(|published| published == tag));

    Ok(Json(UpdateStatusResponse {
        running,
        image_repository: crate::update::IMAGE_REPOSITORY,
        available_tags: tags.tags,
        tags_error: tags.error,
        tags_fetched_at: tags.fetched_at,
        running_is_published,
        agent_live,
        agent_last_seen: agent.map(|beat| beat.at),
        pending,
        last_result: crate::update::last_result(&state.config_dir),
        can_update: blocked_reason.is_none(),
        blocked_reason,
    }))
}

#[derive(Deserialize)]
struct UpdateRelayRequest {
    tag: String,
    confirm: String,
}

#[derive(Serialize)]
struct UpdateRelayResponse {
    message: String,
    requested_tag: String,
    /// Identifies this request in the result the agent writes back, so the
    /// console can tell its own update from a previous one.
    nonce: String,
}

/// Ask the host agent to move this relay to another published image.
///
/// This does not restart anything. It writes a request; the agent pulls the
/// image, re-points compose and recreates the container, and rolls back if the
/// new container does not come up healthy. The relay is not in a position to do
/// any of that — see `src/update.rs` for why it does not get a Docker socket.
async fn handle_update_request(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(req): Json<UpdateRelayRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    let admin_pubkey = validate_session(&admin_state, &headers).ok_or_else(unauthorized)?;

    if !req.confirm.trim().eq_ignore_ascii_case("UPDATE") {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Type UPDATE to confirm",
        ));
    }

    let tag = req.tag.trim().to_string();
    if !crate::update::tag_is_well_formed(&tag) {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "That is not a usable image tag",
        ));
    }

    // Membership in the published list, not a pattern match: the point is that
    // the operator cannot send the relay to an image that does not exist and
    // watch it fail to come back.
    let tags = crate::update::published_tags(false).await;
    if !tags.tags.iter().any(|published| published == &tag) {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            &format!(
                "{tag} is not published for {}",
                crate::update::IMAGE_REPOSITORY
            ),
        ));
    }

    let pending = crate::update::pending_request(&state.config_dir);
    let agent_live = crate::update::agent_is_live(&state.config_dir);
    if let Some(reason) = update_blocked_reason(agent_live, &pending, &tags) {
        return Err(error_response(StatusCode::CONFLICT, &reason));
    }

    let nonce = crate::update::stage_request(&state.config_dir, &tag, &admin_pubkey.to_hex())
        .map_err(|e| {
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("Could not queue the update: {e}"),
            )
        })?;

    info!("Admin {} requested an update to {}", admin_pubkey, tag);

    Ok(Json(UpdateRelayResponse {
        message: format!("Update to {tag} queued"),
        requested_tag: tag,
        nonce,
    }))
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

    // Protected kinds are dropped before anything is written, so a hand-crafted
    // request cannot persist a policy the pruner would then have to refuse.
    let policies: std::collections::BTreeMap<u16, u32> = req
        .policies()
        .into_iter()
        .filter(|(kind, _)| !crate::pruner::NEVER_PRUNE_KINDS.contains(kind))
        .collect();

    if req.pruning_enabled {
        if req.prune_interval_minutes == 0 {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "Prune interval must be at least 1 minute",
            ));
        }
        if policies.is_empty() {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "Choose at least one prunable event kind",
            ));
        }
        if policies.values().any(|days| *days == 0) {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "Every retention window must be at least 1 day",
            ));
        }
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
        "prune_interval",
        &format!("\"{}m\"", req.prune_interval_minutes),
    );
    // Written as a YAML flow map on one line so the existing single-line
    // upsert_relay_value can own the key, the same as every other setting.
    let policy_map = policies
        .iter()
        .map(|(kind, days)| format!("{kind}: \"{days}d\""))
        .collect::<Vec<_>>()
        .join(", ");
    let contents = upsert_relay_value(
        contents,
        "prune_retention_by_kind",
        &format!("{{{policy_map}}}"),
    );
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

/// Storage statistics: what is stored, by kind, and what pruning would delete.
///
/// `?refresh=1` bypasses the cache. Without it a snapshot up to
/// `STORAGE_STATS_TTL_SECS` old is served, flagged with `cached` and
/// `computed_at` so the UI can show its age rather than implying live data.
async fn handle_storage_stats(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Query(params): Query<StorageStatsQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let cache = STORAGE_STATS_CACHE.get_or_init(|| RwLock::new(None));
    let now = Timestamp::now().as_secs() as i64;
    let snapshot = cache.read().clone();

    let fresh = snapshot
        .as_ref()
        .is_some_and(|s| now - s.computed_at < STORAGE_STATS_TTL_SECS);

    if fresh && !params.refresh.unwrap_or(false) {
        return Ok(Json(StorageStatsEnvelope {
            computing: STORAGE_STATS_COMPUTING.load(Ordering::Relaxed),
            stats: snapshot.map(|s| with_live_blacklist(s, &state)),
        }));
    }

    // Stale or forced: kick off a scan and answer immediately with whatever we
    // already have. compare_exchange keeps concurrent callers to one scan.
    if STORAGE_STATS_COMPUTING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        let state_for_scan = Arc::clone(&state);
        tokio::spawn(async move {
            if let Err(e) = refresh_storage_stats(&state_for_scan).await {
                warn!("Storage stats refresh failed: {}", e);
            }
            STORAGE_STATS_COMPUTING.store(false, Ordering::Release);
        });
    }

    Ok(Json(StorageStatsEnvelope {
        computing: true,
        stats: snapshot.map(|s| with_live_blacklist(s, &state)),
    }))
}

/// Present one attribution row for the UI: npub for display, and whether the
/// key is blocked so the row can show the right action.
///
/// The pubkey can be unparseable — for gift wraps it comes from a `p` tag,
/// which is attacker-controlled. Such a row still counts toward storage, so it
/// is shown with an empty npub rather than dropped.
fn author_stat(a: &crate::groups::StorageAuthorCount, state: &ServerState) -> StorageAuthorStat {
    let parsed = PublicKey::from_hex(&a.pubkey).ok();
    StorageAuthorStat {
        pubkey: a.pubkey.clone(),
        npub: parsed
            .and_then(|pk| pk.to_bech32().ok())
            .unwrap_or_default(),
        count: a.count,
        sampled_bytes: a.sampled_bytes,
        attributed_by: a.attributed_by.as_str(),
        blacklisted: parsed.is_some_and(|pk| state.whitelist.blacklist().contains(&pk)),
    }
}

/// Re-check the blacklist flags on a cached snapshot.
///
/// The sample is cached for minutes, but blocking someone from one of these
/// tables must change that row immediately — otherwise the button you just
/// pressed appears to have done nothing until the next scan.
fn with_live_blacklist(
    mut response: StorageStatsResponse,
    state: &ServerState,
) -> StorageStatsResponse {
    for row in &mut response.top_authors {
        row.blacklisted = PublicKey::from_hex(&row.pubkey)
            .is_ok_and(|pk| state.whitelist.blacklist().contains(&pk));
    }
    response.cached = true;
    response
}

/// The pubkeys behind one kind, from the cached sample.
///
/// Reads the snapshot only — never triggers a scan. A drilldown is a click on a
/// row of a table that is already on screen, so it must be instant and must
/// agree with the numbers next to it.
async fn handle_storage_kind_authors(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(kind): Path<u16>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let snapshot = STORAGE_STATS_CACHE
        .get_or_init(|| RwLock::new(None))
        .read()
        .clone();

    let Some(snapshot) = snapshot else {
        return Err(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "No storage sample yet — refresh the storage stats first",
        ));
    };

    let entry = snapshot.kind_authors.iter().find(|k| k.kind == kind);
    let kind_stat = snapshot.kinds.iter().find(|k| k.kind == kind);

    // A kind absent from the sample is not an error: it simply has no events in
    // the window that was examined. Say that with an empty list.
    let (attributed_by, authors) = match entry {
        Some(e) => (
            e.attributed_by.as_str(),
            e.authors.iter().map(|a| author_stat(a, &state)).collect(),
        ),
        None => (
            crate::groups::Attribution::for_kind(kind).as_str(),
            Vec::new(),
        ),
    };

    Ok(Json(StorageKindAuthorsResponse {
        kind,
        attributed_by,
        kind_count: kind_stat.map(|k| k.count).unwrap_or(0),
        kind_sampled_bytes: kind_stat.map(|k| k.sampled_bytes).unwrap_or(0),
        authors,
        computed_at: snapshot.computed_at,
    }))
}

async fn refresh_storage_stats(state: &Arc<ServerState>) -> Result<(), String> {
    let config_dir = StdPath::new(&state.config_dir);
    // Preview against what is configured on disk, not against what is running:
    // an operator reviewing a not-yet-armed setting needs to see its blast
    // radius before enabling it.
    let retention_days =
        parse_duration_days(&read_yaml_scalar(config_dir, "event_retention", "0d"), 0);
    let prune_kinds = read_prune_kinds(config_dir);

    let stats = state
        .http_state
        .groups
        .admin_storage_stats(STORAGE_SAMPLE_SIZE, &prune_kinds, retention_days)
        .await
        .map_err(|e| e.to_string())?;

    let (db_size_bytes, db_file_count) = directory_stats(StdPath::new(&state.db_path));

    let response = StorageStatsResponse {
        sampled_events: stats.sampled_events,
        sample_size: stats.sample_size,
        // Exact: set when a scope's page came back full. The old check
        // compared sampled_events against sample_size, which breaks once there
        // is more than one scope, because `limit` applies per scope.
        sample_is_complete: !stats.sample_truncated,
        kinds: stats
            .kinds
            .iter()
            .map(|k| StorageKindStat {
                kind: k.kind,
                count: k.count,
                sampled_bytes: k.sampled_bytes,
                avg_bytes: if k.count > 0 {
                    k.sampled_bytes / k.count as u64
                } else {
                    0
                },
            })
            .collect(),
        top_recipients: stats
            .top_recipients
            .iter()
            .map(|r| RecipientStat {
                pubkey: r.pubkey.clone(),
                count: r.count,
            })
            .collect(),
        top_authors: stats
            .top_authors
            .iter()
            .map(|a| author_stat(a, state))
            .collect(),
        kind_authors: stats.kind_authors.clone(),
        newest_event_unix: stats.newest_event_unix,
        oldest_sampled_unix: stats.oldest_sampled_unix,
        scope_count: stats.scope_count,
        db_size_bytes,
        db_file_count,
        prune_preview: stats.prune_preview,
        prune_preview_retention_days: retention_days,
        prune_preview_kinds: prune_kinds,
        computed_at: Timestamp::now().as_secs() as i64,
        cached: false,
    };

    *STORAGE_STATS_CACHE
        .get_or_init(|| RwLock::new(None))
        .write() = Some(response);
    Ok(())
}

#[derive(Serialize)]
struct UserEventsDeleteResponse {
    deleted: u64,
}

/// Cap on pubkeys per bulk moderation request, matching the batch event route.
/// Each pubkey costs a count + delete per scope, so an unbounded list could hold
/// the database for minutes.
const MAX_BULK_PUBKEYS: usize = 200;

#[derive(Deserialize)]
struct BulkUserEventsRequest {
    pubkeys: Vec<String>,
    /// Restrict to these kinds. Omit for "everything they authored, minus
    /// protected kinds".
    #[serde(default)]
    kinds: Option<Vec<u16>>,
}

#[derive(Deserialize)]
struct BulkRecipientRequest {
    pubkeys: Vec<String>,
    /// Kinds addressed to these pubkeys. Defaults to gift wraps, the case this
    /// route exists for.
    #[serde(default)]
    kinds: Option<Vec<u16>>,
}

#[derive(Serialize)]
struct BulkUserResult {
    pubkey: String,
    deleted: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct BulkUserResponse {
    /// Total events removed across every listed pubkey.
    deleted: u64,
    failed: usize,
    results: Vec<BulkUserResult>,
}

fn validate_bulk_pubkeys(pubkeys: &[String]) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    if pubkeys.is_empty() {
        return Err(error_response(StatusCode::BAD_REQUEST, "No users selected"));
    }
    if pubkeys.len() > MAX_BULK_PUBKEYS {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Too many users in one request; select up to 200 at a time",
        ));
    }
    Ok(())
}

/// Delete events authored by each listed pubkey.
///
/// Per-pubkey outcomes rather than one pass/fail: a partial failure across a
/// 50-user sweep is otherwise indistinguishable from total success.
async fn handle_users_events_bulk_delete(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(req): Json<BulkUserEventsRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }
    validate_bulk_pubkeys(&req.pubkeys)?;

    let kinds = req.kinds.as_deref();
    let mut results = Vec::with_capacity(req.pubkeys.len());
    let mut deleted = 0u64;
    let mut failed = 0usize;

    for pubkey in &req.pubkeys {
        match state
            .http_state
            .groups
            .admin_delete_user_events(pubkey, kinds)
            .await
        {
            Ok(n) => {
                deleted = deleted.saturating_add(n);
                results.push(BulkUserResult {
                    pubkey: pubkey.clone(),
                    deleted: n,
                    error: None,
                });
            }
            Err(e) => {
                failed += 1;
                results.push(BulkUserResult {
                    pubkey: pubkey.clone(),
                    deleted: 0,
                    error: Some(e.to_string()),
                });
            }
        }
    }

    info!(
        "Admin bulk user wipe: {} events across {} users, {} failed",
        deleted,
        req.pubkeys.len(),
        failed
    );

    Ok(Json(BulkUserResponse {
        deleted,
        failed,
        results,
    }))
}

/// Delete events addressed to each listed pubkey via the `p` tag.
///
/// The only route that can act on gift wraps per user — their authors are
/// one-time keys, so author-based deletion cannot reach them.
async fn handle_events_delete_by_recipient(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(req): Json<BulkRecipientRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }
    validate_bulk_pubkeys(&req.pubkeys)?;

    // Defaulting to gift wraps keeps an omitted `kinds` from meaning "everything
    // that mentions this person", which would sweep up group membership events.
    let kinds = req.kinds.unwrap_or_else(|| vec![1059]);
    if kinds.is_empty() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Choose at least one event kind",
        ));
    }

    let mut results = Vec::with_capacity(req.pubkeys.len());
    let mut deleted = 0u64;
    let mut failed = 0usize;

    for pubkey in &req.pubkeys {
        match state
            .http_state
            .groups
            .admin_delete_events_by_recipient(pubkey, &kinds)
            .await
        {
            Ok(n) => {
                deleted = deleted.saturating_add(n);
                results.push(BulkUserResult {
                    pubkey: pubkey.clone(),
                    deleted: n,
                    error: None,
                });
            }
            Err(e) => {
                failed += 1;
                results.push(BulkUserResult {
                    pubkey: pubkey.clone(),
                    deleted: 0,
                    error: Some(e.to_string()),
                });
            }
        }
    }

    info!(
        "Admin deleted {} events addressed to {} recipients (kinds {:?}), {} failed",
        deleted,
        req.pubkeys.len(),
        kinds,
        failed
    );

    Ok(Json(BulkUserResponse {
        deleted,
        failed,
        results,
    }))
}

#[derive(Deserialize)]
struct BulkDeleteRequest {
    event_ids: Vec<String>,
}

#[derive(Serialize)]
struct BulkDeleteResult {
    id: String,
    deleted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct BulkDeleteResponse {
    deleted: usize,
    failed: usize,
    results: Vec<BulkDeleteResult>,
}

/// Delete a batch of events. Reports per-id outcomes rather than a single
/// pass/fail so the UI can say exactly what did and did not go.
async fn handle_events_bulk_delete(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(req): Json<BulkDeleteRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    if req.event_ids.is_empty() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "No events selected",
        ));
    }
    // Bound the batch so one request cannot pin the database for minutes.
    if req.event_ids.len() > 500 {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Too many events in one request; delete up to 500 at a time",
        ));
    }

    let outcomes = state
        .http_state
        .groups
        .admin_delete_events(&req.event_ids)
        .await;

    let results: Vec<BulkDeleteResult> = outcomes
        .into_iter()
        .map(|(id, error)| BulkDeleteResult {
            id,
            deleted: error.is_none(),
            error,
        })
        .collect();

    let deleted = results.iter().filter(|r| r.deleted).count();
    let failed = results.len() - deleted;
    info!("Admin bulk delete: {} deleted, {} failed", deleted, failed);

    Ok(Json(BulkDeleteResponse {
        deleted,
        failed,
        results,
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
        icon: state.relay_icon.clone().unwrap_or_default(),
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
    /// Retention window per kind, in seconds. Replaces the single
    /// `retention_secs` for relays using per-kind policies; `retention_secs`
    /// stays populated with the shortest window so existing readers of this
    /// endpoint keep working.
    policies_secs: Option<std::collections::BTreeMap<u16, u64>>,
    /// Events deleted per kind since process start.
    deleted_by_kind: Option<std::collections::BTreeMap<u16, u64>>,
    total_pruned: u64,
    /// Events an operator deleted by hand since process start. Distinct from
    /// `total_pruned`, which is the automatic sweep only.
    admin_deleted_total: u64,
    runs: u64,
    last_run_unix: i64,
}

async fn handle_retention_status(State(state): State<Arc<ServerState>>) -> impl IntoResponse {
    let (enabled, retention_secs, interval_secs, prune_kinds, policies_secs) =
        match &state.pruner_config {
            Some(cfg) => {
                let policies = cfg.policies_as_secs();
                // Shortest window: the most aggressive policy is the honest
                // single-number summary for a caller that cannot read the map.
                let shortest = policies.values().copied().min();
                (
                    true,
                    shortest,
                    Some(cfg.interval.as_secs()),
                    Some(cfg.kinds_as_u16()),
                    Some(policies),
                )
            }
            None => (false, None, None, None, None),
        };

    let (total_pruned, runs, last_run_unix, deleted_by_kind) = match &state.pruner_stats {
        Some(s) => (
            s.total_pruned.load(Ordering::Relaxed),
            s.runs.load(Ordering::Relaxed),
            s.last_run_unix.load(Ordering::Relaxed),
            Some(s.per_kind_snapshot()),
        ),
        None => (0, 0, 0, None),
    };

    Json(RetentionStatus {
        enabled,
        retention_secs,
        policies_secs,
        deleted_by_kind,
        interval_secs,
        prune_kinds,
        total_pruned,
        admin_deleted_total: ADMIN_DELETED_TOTAL.load(Ordering::Relaxed),
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

        refresh_wot_roots(&state);

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

        // Dropping a root must revoke the admissions it granted, not leave them
        // cached for the rest of the TTL.
        refresh_wot_roots(&state);
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

/// A targeted prune: one pubkey, specific kinds, an optional date window.
///
/// Blocking a pubkey stops it connecting but reclaims no disk. This is the
/// other half — removing what is already stored, scoped narrowly enough that
/// an operator can aim it at a spam flood without touching anything else.
#[derive(Deserialize)]
struct PruneTarget {
    pubkey: String,
    /// `"author"` or `"recipient"`. Must match how the row was attributed:
    /// gift wraps can only be matched by their `p` tag. Per target, because a
    /// multi-select can mix gift-wrap recipients with ordinary authors and
    /// applying one rule to both would delete the wrong people's data.
    attributed_by: String,
}

#[derive(Deserialize)]
struct PruneRequest {
    /// One or more pubkeys to clear in a single action.
    targets: Vec<PruneTarget>,
    /// Required. Protected NIP-29 kinds are dropped server-side regardless.
    kinds: Vec<u16>,
    /// Unix seconds, inclusive bounds. Omit for open-ended.
    #[serde(default)]
    since: Option<u64>,
    #[serde(default)]
    until: Option<u64>,
    /// When true, count only. The UI previews before asking to confirm.
    #[serde(default)]
    dry_run: bool,
    /// Typed confirmation, required for the destructive path.
    #[serde(default)]
    confirm: String,
}

#[derive(Serialize)]
struct PruneResponse {
    /// Events matched across every target. Equals `deleted` unless this was a
    /// dry run.
    matched: u64,
    deleted: u64,
    dry_run: bool,
    /// Per-pubkey breakdown, so a bulk delete reports what it actually did
    /// rather than one opaque total.
    per_target: Vec<PruneTargetResult>,
}

#[derive(Serialize)]
struct PruneTargetResult {
    pubkey: String,
    matched: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

async fn handle_storage_prune(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(req): Json<PruneRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    let Some(owner) = validate_session(&admin_state, &headers) else {
        return Err(unauthorized());
    };

    if req.targets.is_empty() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Select at least one pubkey",
        ));
    }
    if req.targets.len() > MAX_BULK_PUBKEYS {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Too many pubkeys in one request; select up to 200 at a time",
        ));
    }

    if req.kinds.is_empty() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Select at least one kind to delete",
        ));
    }
    if let (Some(since), Some(until)) = (req.since, req.until) {
        if since > until {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "The start of the window must come before its end",
            ));
        }
    }
    // The preview is free; only the irreversible path needs the phrase.
    if !req.dry_run && req.confirm.trim() != "DELETE" {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "Type DELETE to confirm",
        ));
    }

    // Per-target outcomes rather than one pass/fail: a partial failure across a
    // 50-pubkey sweep is otherwise indistinguishable from total success.
    let mut per_target = Vec::with_capacity(req.targets.len());
    let mut matched = 0u64;

    for target in &req.targets {
        let by = match target.attributed_by.as_str() {
            "author" => crate::groups::Attribution::Author,
            "recipient" => crate::groups::Attribution::Recipient,
            _ => {
                return Err(error_response(
                    StatusCode::BAD_REQUEST,
                    "attributed_by must be author or recipient",
                ))
            }
        };

        match state
            .http_state
            .groups
            .admin_prune_events(
                &target.pubkey,
                by,
                &req.kinds,
                req.since,
                req.until,
                req.dry_run,
            )
            .await
        {
            Ok(n) => {
                matched = matched.saturating_add(n);
                per_target.push(PruneTargetResult {
                    pubkey: target.pubkey.clone(),
                    matched: n,
                    error: None,
                });
            }
            Err(e) => per_target.push(PruneTargetResult {
                pubkey: target.pubkey.clone(),
                matched: 0,
                error: Some(e.to_string()),
            }),
        }
    }

    if !req.dry_run {
        ADMIN_DELETED_TOTAL.fetch_add(matched, Ordering::Relaxed);
        info!(
            "Admin {} pruned {} events across {} pubkey(s)",
            owner,
            matched,
            req.targets.len()
        );
        // The cached sample now overstates what is on disk. Flag the rescan as
        // in progress so the client's poll keeps going until it lands --
        // without this the next poll reports `computing: false`, the UI stops
        // polling, and the deleted rows sit there until a manual refresh.
        if STORAGE_STATS_COMPUTING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let state_for_scan = Arc::clone(&state);
            tokio::spawn(async move {
                if let Err(e) = refresh_storage_stats(&state_for_scan).await {
                    warn!("Storage stats refresh after prune failed: {}", e);
                }
                STORAGE_STATS_COMPUTING.store(false, Ordering::Release);
            });
        }
    }

    Ok(Json(PruneResponse {
        matched,
        deleted: if req.dry_run { 0 } else { matched },
        dry_run: req.dry_run,
        per_target,
    }))
}

/// Where access actually comes from, counted per tier.
///
/// The whitelist screen lists manual and follow-derived entries, because those
/// are the only ones that exist as a list. Web-of-Trust admission is computed
/// per pubkey against a graph, so it never appeared in that count — making a
/// relay admitting 140,000 accounts report "233 whitelisted" and look as though
/// the tier was doing nothing.
#[derive(Serialize)]
struct AccessSourcesResponse {
    /// Pubkeys added by hand.
    manual: usize,
    /// Pulled from the reference accounts' contact lists by follow sync.
    follow_derived: usize,
    /// Blocked outright; overrides every tier above.
    blacklisted: usize,
    wot_enabled: bool,
    /// Accounts the follow graph admits. Zero until it has been built.
    wot_admitted: usize,
    wot_max_hops: u8,
    /// True when no tier restricts anything, i.e. an open relay.
    open_relay: bool,
    /// The publishing ladder: what each tier may send per minute, derived from
    /// the configured per-pubkey budget. Sent so the console can state the rule
    /// rather than leave an operator to infer it.
    budget_ladder: Vec<BudgetRung>,
}

#[derive(Serialize)]
struct BudgetRung {
    tier: &'static str,
    label: String,
    percent: u32,
    events_per_minute: u32,
}

async fn handle_access_sources(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let wot = state.whitelist.wot();
    let wot_admitted = wot
        .as_ref()
        .and_then(|o| o.graph())
        .map(|g| g.coverage().reachable_total)
        .unwrap_or(0);

    let base = read_yaml_u32(
        StdPath::new(&state.config_dir),
        "pubkey_rate_limit_per_minute",
        6000,
    );
    // One rung per distinct budget. Follow sync and one hop are the same
    // population, so listing both produced two identical rows.
    let budget_ladder = [
        crate::whitelist::AccessTier::Manual,
        crate::whitelist::AccessTier::FollowSync,
        crate::whitelist::AccessTier::WebOfTrust(2),
        crate::whitelist::AccessTier::WebOfTrust(3),
        crate::whitelist::AccessTier::Open,
    ]
    .into_iter()
    .map(|tier| BudgetRung {
        tier: tier.as_budget_key(),
        label: tier.label(),
        percent: tier.budget_percent(),
        events_per_minute: (base * tier.budget_percent() / 100).max(1),
    })
    .collect();

    Ok(Json(AccessSourcesResponse {
        budget_ladder,
        manual: state.whitelist.list_manual().len(),
        follow_derived: state.whitelist.list_follow_derived().len(),
        blacklisted: state.whitelist.blacklist().len(),
        wot_enabled: wot.is_some(),
        wot_admitted,
        wot_max_hops: wot.as_ref().map(|o| o.max_hops()).unwrap_or(0),
        open_relay: state.whitelist.is_empty(),
    }))
}

#[derive(Deserialize)]
struct AccessCheckQuery {
    pubkey: String,
}

/// The admission decision for one pubkey, and which tier produced it.
///
/// Exists because "is the Web of Trust actually being used?" is otherwise
/// unanswerable from the console: WoT admission is computed per pubkey and
/// never appears in any list, so a relay admitting a hundred thousand accounts
/// looks identical to one admitting none. This runs the real tier ladder --
/// the same `Whitelist` the hot path uses -- rather than re-deriving it.
#[derive(Serialize)]
struct AccessCheckResponse {
    hex: String,
    npub: String,
    admitted: bool,
    /// `blacklist`, `manual`, `follow_sync`, `web_of_trust`, `open_relay`, or
    /// `none`.
    tier: &'static str,
    /// Hops from the nearest root, when the graph could place them.
    hops: Option<u8>,
    explanation: String,
}

async fn handle_access_check(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Query(params): Query<AccessCheckQuery>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let input = params.pubkey.trim();
    // The two decoders return different error types, so map each to () before
    // joining them.
    let pk = if input.starts_with("npub") {
        PublicKey::from_bech32(input).map_err(|_| ())
    } else {
        PublicKey::from_hex(input).map_err(|_| ())
    }
    .map_err(|_| error_response(StatusCode::BAD_REQUEST, "Not a valid npub or hex pubkey"))?;

    let whitelist = &state.whitelist;
    let wot = whitelist.wot();

    // Ask the graph directly rather than the resolution cache: the cache is
    // only filled when someone connects, and the question here is "would this
    // key be admitted", not "has it been".
    let hops = wot
        .as_ref()
        .and_then(|o| o.graph().map(|g| (o, g)))
        .and_then(|(o, g)| g.distance(&o.roots(), &pk, o.max_hops()));

    let (tier, admitted, explanation) = if whitelist.blacklist().contains(&pk) {
        (
            "blacklist",
            false,
            "Blocked. This overrides every other tier.".to_string(),
        )
    } else if whitelist.list_manual().contains(&pk) {
        (
            "manual",
            true,
            "Added by hand to the allowlist.".to_string(),
        )
    } else if whitelist.list_follow_derived().contains(&pk) {
        (
            "follow_sync",
            true,
            "Followed by a reference account, via follow sync.".to_string(),
        )
    } else if let Some(hops) = hops {
        (
            "web_of_trust",
            true,
            format!("{hops} hop(s) from a reference account in the follow graph."),
        )
    } else if whitelist.is_empty() {
        (
            "open_relay",
            true,
            "No tier restricts anything, so everyone is admitted.".to_string(),
        )
    } else {
        let detail = match wot.as_ref().and_then(|o| o.graph()) {
            Some(g) if !g.is_built() => " The follow graph is still being built.",
            Some(g) if g.coverage().truncated => {
                " The graph is incomplete at the outermost hop, so this may be                  a path that was never fetched rather than one that does not exist."
            }
            _ => "",
        };
        (
            "none",
            false,
            format!("On no list, and not reachable in the follow graph.{detail}"),
        )
    };

    Ok(Json(AccessCheckResponse {
        hex: pk.to_hex(),
        npub: pk.to_bech32().unwrap_or_default(),
        admitted,
        tier,
        hops,
        explanation,
    }))
}

/// serde default for booleans that should stay on unless explicitly disabled.
fn default_true() -> bool {
    true
}

/// Editable Web-of-Trust settings.
#[derive(Deserialize)]
struct WotSettingsRequest {
    enabled: bool,
    /// Compute from this relay's own follow graph rather than an oracle.
    #[serde(default = "default_true")]
    local: bool,
    oracle_url: String,
    #[serde(default)]
    fallback_oracle_url: String,
    max_hops: u8,
    /// Hex pubkeys. Empty means "track the reference accounts".
    #[serde(default)]
    roots: Vec<String>,
}

/// Write `relay.wot` to `settings.local.yml` as a one-line flow mapping.
///
/// Flow style, like `prune_retention_by_kind`, because `upsert_relay_value`
/// replaces a key's entire block: a multi-line `wot:` written here and then
/// re-saved would leave its children orphaned under a flow mapping, which is
/// unparseable YAML and would stop the relay starting.
fn persist_wot_settings(
    config_dir: &StdPath,
    req: &WotSettingsRequest,
) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(config_dir)?;
    let path = config_dir.join(SETTINGS_LOCAL_FILE);
    let contents = std::fs::read_to_string(&path).unwrap_or_else(|_| "relay:\n".to_string());

    let roots = req
        .roots
        .iter()
        .map(|r| format!("\"{r}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let value = format!(
        "{{enabled: {}, local: {}, oracle_url: \"{}\", fallback_oracle_url: \"{}\", max_hops: {}, roots: [{}]}}",
        req.enabled,
        req.local,
        req.oracle_url.replace('"', ""),
        req.fallback_oracle_url.replace('"', ""),
        req.max_hops,
        roots,
    );

    let contents = upsert_relay_value(contents, "wot", &value);
    std::fs::write(path, contents)
}

async fn handle_wot_settings_update(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(req): Json<WotSettingsRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    // The oracle rejects anything outside 1..=5, so saving a value it will
    // refuse would produce a tier that silently admits nobody.
    if !(1..=5).contains(&req.max_hops) {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "max_hops must be between 1 and 5",
        ));
    }
    if req.enabled && !req.local && req.oracle_url.trim().is_empty() {
        return Err(error_response(
            StatusCode::BAD_REQUEST,
            "An oracle URL is required to enable Web-of-Trust admission",
        ));
    }
    for root in &req.roots {
        if PublicKey::from_hex(root).is_err() {
            return Err(error_response(
                StatusCode::BAD_REQUEST,
                "Roots must be 64-character hex pubkeys",
            ));
        }
    }

    persist_wot_settings(StdPath::new(&state.config_dir), &req).map_err(|e| {
        warn!("Failed to persist WoT settings: {}", e);
        error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to persist WoT settings",
        )
    })?;

    *state.wot_configured.write() = crate::server::WotConfigured {
        enabled: req.enabled,
        local: req.local,
        oracle_url: req.oracle_url.clone(),
        fallback_oracle_url: req.fallback_oracle_url.clone(),
        max_hops: req.max_hops,
        roots: req.roots.clone(),
    };

    info!(
        "Admin updated WoT settings: enabled={}, max_hops={}, {} root(s)",
        req.enabled,
        req.max_hops,
        req.roots.len()
    );

    // Deliberately not applied live. The tier is constructed at startup — it
    // owns an HTTP client, a verdict cache and the whitelist wiring — so
    // pretending a toggle took effect here would be a lie. The response says a
    // restart is required and the UI repeats it.
    Ok(Json(serde_json::json!({
        "saved": true,
        "restart_required": true,
    })))
}

/// Re-point the WoT graph at the current reference accounts.
///
/// Only when `relay.wot.roots` was left empty — an operator who pinned roots
/// explicitly does not expect editing reference accounts to move them. Clears
/// the verdict cache as a side effect, since every cached hop count was
/// measured against the previous roots.
fn refresh_wot_roots(state: &ServerState) {
    if !state.wot_roots_follow_reference_accounts {
        return;
    }
    let Some(oracle) = state.whitelist.wot() else {
        return;
    };
    let roots = state.reference_accounts.list();
    info!(
        "Reference accounts changed: re-rooting the WoT graph on {} account(s)",
        roots.len()
    );
    oracle.set_roots(roots);
}

/// A pubkey admitted by the Web-of-Trust tier, with how far it sits from the
/// nearest root.
#[derive(Serialize)]
struct WotAdmittedEntry {
    hex: String,
    npub: String,
    hops: u8,
}

/// Whether WoT admission is on, whether it is actually working, and who it has
/// let in.
///
/// `enabled` alone is not enough for an operator: an enabled tier with a dead
/// oracle or no roots admits nobody, and looks identical to a quiet relay.
#[derive(Serialize)]
struct WotStatusResponse {
    enabled: bool,
    /// Plain-language verdict: "admitting", "oracle unreachable", "no roots",
    /// or "disabled".
    status: String,
    oracle_url: String,
    /// Result of a live `/health` probe. None when the tier is off.
    oracle_reachable: Option<bool>,
    oracle_error: Option<String>,
    /// True once repeated failures have tripped the breaker. Always false in
    /// local mode: there is nothing remote to be degraded.
    degraded: bool,
    /// Computing from this relay's own follow graph rather than an oracle.
    local: bool,
    /// Accounts whose follow list the local graph knows, and total follow edges.
    graph_accounts: usize,
    graph_edges: usize,
    /// Deepest hop the graph can answer for with confidence. Below `max_hops`
    /// when the contact-list budget ran out, which makes refusals past this
    /// depth "never fetched" rather than "not connected".
    graph_complete_to_hop: u8,
    graph_truncated: bool,
    /// How many accounts the graph would admit. This is the number that answers
    /// "is the tier working"; `admitted` only counts keys that have connected.
    would_admit_total: usize,
    max_hops: u8,
    root_count: usize,
    /// Keys resolved and admitted so far. Not the full set the graph would
    /// admit — only those that have connected.
    admitted: Vec<WotAdmittedEntry>,
    /// What is on disk, which after a save is ahead of what is running.
    configured: crate::server::WotConfigured,
    /// True when the saved config differs from the running tier, so the UI can
    /// say a restart is pending rather than showing the change as applied.
    restart_required: bool,
}

async fn handle_wot_status(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorResponse>)> {
    let admin_state = get_admin_state(&state);
    if validate_session(&admin_state, &headers).is_none() {
        return Err(unauthorized());
    }

    let configured = state.wot_configured.read().clone();

    let Some(oracle) = state.whitelist.wot() else {
        return Ok(Json(WotStatusResponse {
            enabled: false,
            // Saved-on but not running means the relay has not been restarted
            // since. Saying "disabled" there would be wrong.
            status: if configured.enabled {
                "saved — restart to apply".to_string()
            } else {
                "disabled".to_string()
            },
            oracle_url: configured.oracle_url.clone(),
            oracle_reachable: None,
            oracle_error: None,
            degraded: false,
            local: configured.local,
            graph_accounts: 0,
            graph_edges: 0,
            graph_complete_to_hop: 0,
            graph_truncated: false,
            would_admit_total: 0,
            max_hops: configured.max_hops,
            root_count: 0,
            restart_required: configured.enabled,
            configured,
            admitted: Vec::new(),
        }));
    };

    let health = oracle.health_check().await;
    let reachable = health.is_ok();
    let root_count = oracle.roots().len();
    let local = oracle.is_local();
    let (graph_accounts, graph_edges) = oracle.graph().map(|g| g.size()).unwrap_or((0, 0));
    let graph_coverage = oracle.graph().map(|g| g.coverage()).unwrap_or_default();

    // Report the first thing that would stop admission, in the order an
    // operator can act on it. "No roots" comes first locally because without
    // them the graph is not merely unbuilt, it is meaningless.
    let status = if root_count == 0 {
        "no roots — add reference accounts"
    } else if local && !reachable {
        "building the follow graph…"
    } else if !local && !reachable {
        "oracle unreachable"
    } else if oracle.is_degraded() {
        "degraded"
    } else {
        "admitting"
    };

    // In local mode show who the graph *admits*, nearest first -- not merely
    // who has happened to connect. The latter is near-empty on a quiet relay
    // and reads as a broken tier.
    let blacklist = state.whitelist.blacklist();
    let admitted: Vec<WotAdmittedEntry> = match oracle.graph() {
        // A bounded, nearest-first window across every hop -- purely the
        // "who is in" preview for this card. The tier screens page the full
        // set through /access/tier/{n} instead.
        Some(graph) => graph
            .admitted_preview(crate::wot_graph::ADMITTED_PAGE_MAX)
            .into_iter()
            .filter(|(pk, _)| !blacklist.contains(pk))
            .map(|(pk, hops)| WotAdmittedEntry {
                hex: pk.to_hex(),
                npub: pk.to_bech32().unwrap_or_default(),
                hops,
            })
            .collect(),
        None => state
            .whitelist
            .list_wot_admitted()
            .into_iter()
            .map(|(pk, hops)| WotAdmittedEntry {
                hex: pk.to_hex(),
                npub: pk.to_bech32().unwrap_or_default(),
                hops,
            })
            .collect(),
    };

    // Compare what is running against what is saved, field by field, so the
    // banner appears only when a restart would actually change something.
    let restart_required = !configured.enabled
        || configured.max_hops != oracle.max_hops()
        || configured.oracle_url.trim_end_matches('/') != oracle.oracle_url();

    Ok(Json(WotStatusResponse {
        enabled: true,
        status: status.to_string(),
        oracle_url: oracle.oracle_url().to_string(),
        oracle_reachable: Some(reachable),
        oracle_error: health.err(),
        degraded: oracle.is_degraded(),
        local,
        graph_accounts,
        graph_edges,
        graph_complete_to_hop: graph_coverage.complete_to_hop,
        graph_truncated: graph_coverage.truncated,
        would_admit_total: graph_coverage.reachable_total,
        max_hops: oracle.max_hops(),
        root_count,
        admitted,
        configured,
        restart_required,
    }))
}

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

    let deleted = state
        .http_state
        .groups
        .admin_delete_user_events(&pubkey, None)
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                Json(ErrorResponse {
                    error: e.to_string(),
                }),
            )
        })?;

    // Report the real figure rather than a bare 204: the caller cannot otherwise
    // tell a wipe of 10,000 events from one that matched nothing.
    Ok(Json(UserEventsDeleteResponse { deleted }))
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

/// Cached storage statistics. Recomputing walks one index range per probed
/// kind across every scope; measured against the 4.2 GB production database
/// that takes longer than the server's 30s request timeout, so it is never
/// computed inline — the handler serves the cache and refreshes behind it.
static STORAGE_STATS_CACHE: OnceCell<RwLock<Option<StorageStatsResponse>>> = OnceCell::new();

/// Guards against piling up concurrent scans when several admins (or a polling
/// UI) ask for a refresh at once.
static STORAGE_STATS_COMPUTING: AtomicBool = AtomicBool::new(false);

/// Events deleted by an operator since this process started.
///
/// Separate from the pruner's `total_pruned`, which only counts what the
/// automatic retention sweep removed. The Storage screen showed that figure
/// under a bare "Events deleted" heading, so an operator deleting by hand
/// watched a counter that could not move and reasonably concluded nothing was
/// being deleted.
static ADMIN_DELETED_TOTAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How long a computed storage snapshot stays fresh before a background
/// refresh is triggered on the next request.
const STORAGE_STATS_TTL_SECS: i64 = 300;

/// How many newest events to examine for the kind breakdown. Large enough to
/// be representative of what is currently filling the relay, small enough that
/// the read stays bounded on a multi-GB database.
const STORAGE_SAMPLE_SIZE: usize = 20_000;

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

#[cfg(test)]
mod settings_yaml_tests {
    use super::{upsert_block_value, upsert_relay_scalar, upsert_relay_value};
    use crate::config::Config;
    use std::time::Duration;

    /// Write `local` as settings.local.yml next to a minimal settings.yml and
    /// load it exactly as the relay does at boot. Returns Err if the file the
    /// admin API produced is not something the relay can start from.
    fn boots_with(local: &str, tag: &str) -> Result<crate::config::RelaySettings, String> {
        let dir =
            std::env::temp_dir().join(format!("obelisk-upsert-{}-{}", std::process::id(), tag));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("settings.yml"),
            "relay:\n  relay_secret_key: \"\"\n  local_addr: \"127.0.0.1:1\"\n  relay_url: \"ws://127.0.0.1:1\"\n  db_path: \"db\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("settings.local.yml"), local).unwrap();

        Config::new(&dir)
            .map_err(|e| e.to_string())?
            .get_settings()
            .map_err(|e| e.to_string())
    }

    /// force_public_groups is a one-way door -- the startup sweep clears
    /// `private` and `hidden` on every stored group -- so prove the written
    /// config round-trips to the value the relay will actually act on.
    #[test]
    fn force_public_groups_round_trips_through_the_config() {
        let local = concat!(
            "relay:\n",
            "  relay_secret_key: \"\"\n",
            "  local_addr: \"127.0.0.1:1\"\n",
            "  relay_url: \"ws://127.0.0.1:1\"\n",
            "  db_path: \"db\"\n",
            "  force_public_groups: false\n",
        );

        let on = upsert_relay_value(local.to_string(), "force_public_groups", "true");
        assert!(
            boots_with(&on, "fpg-on")
                .expect("config must parse")
                .force_public_groups
        );

        let off = upsert_relay_value(on, "force_public_groups", "false");
        assert!(
            !boots_with(&off, "fpg-off")
                .expect("config must parse")
                .force_public_groups,
            "turning it back off must be expressible, even though it cannot undo the sweep"
        );
    }

    /// The websocket block is the one place where two writers meet: it is the
    /// insert anchor `upsert_relay_value` uses for new flat keys, and now also a
    /// block the console rewrites child-by-child. So prove the result still
    /// parses, and that the comments and untouched siblings survive.
    #[test]
    fn saving_connection_settings_keeps_the_websocket_block_loadable() {
        let local = concat!(
            "relay:\n",
            "  relay_secret_key: \"\"\n",
            "  local_addr: \"127.0.0.1:1\"\n",
            "  relay_url: \"ws://127.0.0.1:1\"\n",
            "  db_path: \"db\"\n",
            "  max_subscriptions: 50\n",
            "  websocket:\n",
            "    # Raised so an idle reader keeps its connection for a session.\n",
            "    max_connection_duration: \"6h\"\n",
            "    idle_timeout: \"2h\"\n",
            "    max_connections: 500\n",
        );

        let written = upsert_block_value(local.to_string(), "websocket", "idle_timeout", "\"45m\"");
        let written = upsert_block_value(written, "websocket", "max_connections", "250");
        let written = upsert_block_value(written, "websocket", "max_connections_per_ip", "16");
        let written = upsert_relay_scalar(written, "max_subscriptions", 64);

        assert!(
            written.contains("# Raised so an idle reader keeps its connection"),
            "the block's comments must survive a save:\n{written}"
        );
        assert!(
            written.contains("max_connection_duration: \"6h\""),
            "an untouched sibling must survive a save:\n{written}"
        );

        let settings = boots_with(&written, "ws-block").expect("written config must parse");
        assert_eq!(
            settings.websocket.idle_timeout(),
            Some(Duration::from_secs(45 * 60))
        );
        assert_eq!(settings.websocket.max_connections(), Some(250));
        assert_eq!(settings.websocket.max_connections_per_ip(), Some(16));
        assert_eq!(
            settings.websocket.max_connection_duration(),
            Some(Duration::from_secs(6 * 3600))
        );
        assert_eq!(settings.max_subscriptions, 64);
    }

    /// A relay with no `websocket:` block at all -- the console must create one
    /// rather than write orphaned children at the wrong depth.
    #[test]
    fn connection_settings_can_create_a_missing_websocket_block() {
        let local = concat!(
            "relay:\n",
            "  relay_secret_key: \"\"\n",
            "  local_addr: \"127.0.0.1:1\"\n",
            "  relay_url: \"ws://127.0.0.1:1\"\n",
            "  db_path: \"db\"\n",
        );

        let written = upsert_block_value(local.to_string(), "websocket", "max_connections", "300");
        let settings = boots_with(&written, "ws-create").expect("written config must parse");
        assert_eq!(settings.websocket.max_connections(), Some(300));
    }

    /// The exact corruption seen in production on 2026-09-15.
    ///
    /// settings.local.yml held the block form that docs/retention.md documents.
    /// Saving from the Storage screen rewrote the key as a flow map and left the
    /// indented child stranded, producing YAML the relay could not parse — it
    /// would have failed to start on the next restart.
    #[test]
    fn replacing_a_block_mapping_does_not_orphan_its_children() {
        let before = "relay:\n  \
                      force_public_groups: false\n  \
                      prune_retention_by_kind:\n    \
                      1059: \"30d\"     # gift wraps\n  \
                      max_limit: 500\n";

        let after = upsert_relay_value(
            before.to_string(),
            "prune_retention_by_kind",
            "{1059: \"7d\"}",
        );

        assert!(
            !after.contains("\"30d\""),
            "the old block must be gone, got:\n{after}"
        );
        assert!(after.contains("  max_limit: 500"), "siblings survive");

        let settings = boots_with(&after, "block").expect("relay must still start");
        let policies = settings.prune_retention_by_kind.expect("policy parses");
        assert_eq!(policies.len(), 1);
        assert_eq!(
            policies.get(&1059).map(std::time::Duration::as_secs),
            Some(7 * 86_400),
            "the new window wins, not the orphaned old one"
        );
    }

    #[test]
    fn replacing_a_single_line_value_leaves_neighbours_untouched() {
        let before = "relay:\n  relay_name: \"Old\"\n  max_limit: 500\n";
        let after = upsert_relay_value(before.to_string(), "relay_name", "\"New\"");
        assert!(!after.contains("Old"));
        let settings = boots_with(&after, "single").expect("relay must still start");
        assert_eq!(settings.relay_name.as_deref(), Some("New"));
        assert_eq!(settings.max_limit, 500);
    }

    #[test]
    fn a_sibling_block_further_down_is_not_swallowed() {
        // websocket: is a sibling of the replaced key, not a child of it.
        let before = "relay:\n  \
                      prune_interval: \"1h\"\n  \
                      websocket:\n    \
                      idle_timeout: \"2h\"\n";
        let after = upsert_relay_value(before.to_string(), "prune_interval", "\"360m\"");
        assert!(after.contains("  websocket:"), "got:\n{after}");
        assert!(after.contains("    idle_timeout: \"2h\""), "got:\n{after}");

        let settings = boots_with(&after, "sibling").expect("relay must still start");
        assert_eq!(settings.prune_interval.map(|d| d.as_secs()), Some(360 * 60));
    }

    #[test]
    fn an_absent_key_is_inserted() {
        let before = "relay:\n  relay_name: \"R\"\n  websocket:\n    idle_timeout: \"2h\"\n";
        let after = upsert_relay_value(before.to_string(), "enable_event_pruner", "true");
        let settings = boots_with(&after, "insert").expect("relay must still start");
        assert!(settings.enable_event_pruner);
    }
}
