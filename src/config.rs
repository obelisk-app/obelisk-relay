use anyhow::Result;
use config::{Config as ConfigTree, ConfigError, Environment, File};
use nostr_sdk::prelude::*;
use serde::Deserialize;
use std::path::Path;
use std::time::Duration;
use tracing::info;

const ENVIRONMENT_PREFIX: &str = "NIP29";
const CONFIG_SEPARATOR: &str = "__";

/// Deserializes `{kind: "30d"}` into `{u16: Duration}`.
///
/// `humantime_serde` handles a bare duration but not one nested as a map value,
/// and YAML map keys arrive as strings, so both sides are converted here.
mod humantime_kind_map {
    use serde::{Deserialize, Deserializer};
    use std::collections::BTreeMap;
    use std::time::Duration;

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<BTreeMap<u16, Duration>>, D::Error>
    where
        D: Deserializer<'de>,
    {
        // humantime_serde::Serde<Duration> parses "30d" without pulling in
        // `humantime` as a direct dependency.
        let raw = Option::<BTreeMap<String, humantime_serde::Serde<Duration>>>::deserialize(
            deserializer,
        )?;
        let Some(raw) = raw else { return Ok(None) };

        let mut out = BTreeMap::new();
        for (kind, window) in raw {
            let kind: u16 = kind
                .parse()
                .map_err(|_| serde::de::Error::custom(format!("invalid event kind: {kind}")))?;
            out.insert(kind, window.into_inner());
        }
        Ok(Some(out))
    }
}

/// The relay identity that used to ship in `config/settings.yml`.
///
/// It was committed to a public repository, so its private half is known to
/// anyone. Any relay still running on it can be impersonated and can have its
/// NIP-29 group state (kinds 39000-39003) forged. It is recognised here so a
/// deployment that inherited it is migrated to a unique key on next start
/// rather than silently continuing to use a compromised identity.
pub const COMPROMISED_DEFAULT_SECRET_KEY: &str =
    "6b911fd37cdf5c81d4c0adb1ab7fa822ed253ab0ad9aa18d77257c88b29b718e";

#[derive(Debug, Deserialize)]
pub struct RelaySettings {
    /// Relay identity. Empty on a fresh deployment — `ensure_relay_identity`
    /// generates and persists a unique key before the relay starts.
    #[serde(default)]
    pub relay_secret_key: String,
    pub local_addr: String,
    pub relay_url: String,
    pub db_path: String,
    #[serde(default)]
    pub websocket: WebSocketSettings,
    #[serde(default = "default_max_limit")]
    pub max_limit: usize,
    #[serde(default = "default_max_subscriptions")]
    pub max_subscriptions: usize,
    #[serde(default)]
    pub whitelisted_pubkeys: Vec<String>,
    #[serde(default)]
    pub admin_pubkeys: Vec<String>,
    /// Optional advertised relay name (NIP-11 / admin info). Defaults baked in if None.
    #[serde(default)]
    pub relay_name: Option<String>,
    /// Optional advertised relay description (NIP-11 / admin info).
    #[serde(default)]
    pub relay_description: Option<String>,
    /// Optional relay icon, advertised as NIP-11 `icon` and used as the
    /// browser favicon so multi-instance operators can tell deployments apart.
    /// Either an `https://` URL or a small `data:image/...;base64,` URI.
    #[serde(default)]
    pub relay_icon: Option<String>,
    /// If set, events of `prune_kinds` older than this duration can be deleted by
    /// a background task, but only when `enable_event_pruner` is explicitly true.
    /// Retention config alone is kept as inert policy metadata to prevent accidental
    /// data loss from stale settings files.
    #[serde(default, with = "humantime_serde")]
    pub event_retention: Option<Duration>,
    /// Explicit destructive switch for retention pruning. Defaults false.
    #[serde(default)]
    pub enable_event_pruner: bool,
    /// How often the pruner runs. Defaults to retention/48 (clamped 60s..6h) if not set.
    #[serde(default, with = "humantime_serde")]
    pub prune_interval: Option<Duration>,
    /// Kinds eligible for time-based pruning. Defaults to NIP-29 chat-style kinds [9, 11, 12].
    /// Superseded by `prune_retention_by_kind`; kept so existing configs keep working.
    #[serde(default)]
    pub prune_kinds: Option<Vec<u16>>,
    /// Per-kind retention, e.g. `{1059: "30d", 2390: "7d"}`.
    ///
    /// A relay holding both ephemeral game moves and long-lived conversation cannot
    /// express both with one window. When set, this replaces the
    /// `event_retention` + `prune_kinds` pair entirely; when absent, that pair is
    /// migrated into this shape at startup so nothing changes for existing configs.
    #[serde(default, with = "humantime_kind_map")]
    pub prune_retention_by_kind: Option<std::collections::BTreeMap<u16, Duration>>,
    /// Per-pubkey events/minute. None disables the per-pubkey limiter.
    #[serde(default)]
    pub pubkey_rate_limit_per_minute: Option<u32>,
    /// Per-connection events/minute. None disables the per-connection limiter.
    #[serde(default)]
    pub connection_rate_limit_per_minute: Option<u32>,
    /// Global events/minute across the whole relay. None disables the global limiter.
    #[serde(default)]
    pub global_rate_limit_per_minute: Option<u32>,
    /// When `true`, the relay forces every group to be public regardless
    /// of the `["private"]` / `["public"]` tags in incoming kind 9007 /
    /// 9002 events. Intended for the public Obelisk relay where private
    /// groups would create read-access scoping the public relay isn't
    /// designed to enforce. Defaults to `false` so the whitelisted relay
    /// keeps full NIP-29 semantics.
    #[serde(default)]
    pub force_public_groups: bool,
    /// NIP-50 search filters. When false, incoming REQ/COUNT filters that
    /// include `search` are rejected before they reach storage.
    #[serde(default = "default_enable_indexed_search")]
    pub enable_indexed_search: bool,
    /// Optional advertisement override. Defaults to `enable_indexed_search`.
    /// A disabled search capability is never advertised.
    #[serde(default)]
    pub advertise_indexed_search: Option<bool>,
    /// Obelisk-specific optimized HTTP bootstrap index. Additive to normal
    /// Nostr WebSocket behavior.
    #[serde(default)]
    pub obelisk_index: ObeliskIndexSettings,
    /// Web-of-Trust admission. Disabled by default, so an existing deployment
    /// keeps exactly the access rules it has today.
    #[serde(default)]
    pub wot: WotSettings,
}

/// Admission by follow-graph distance, answered by a `nostr-wot-oracle`.
///
/// When enabled this becomes a fourth whitelist tier — see
/// [`crate::whitelist::Whitelist::contains`]. It can only widen access: the
/// blacklist still overrides it, and manual entries are still consulted first.
#[derive(Debug, Deserialize, Clone)]
pub struct WotSettings {
    #[serde(default)]
    pub enabled: bool,
    /// Build the follow graph on this relay instead of querying an oracle.
    ///
    /// Default. The oracle is a second service to run and keep alive, and when
    /// it is down the tier admits nobody; a locally built graph has neither
    /// problem. Set false to use `oracle_url` instead.
    #[serde(default = "default_wot_local")]
    pub local: bool,
    /// Oracle tried first when `local` is false. Assumes the compose sidecar.
    #[serde(default = "default_wot_oracle_url")]
    pub oracle_url: String,
    /// A second oracle to try when `oracle_url` errors. Empty (the default)
    /// disables it.
    ///
    /// Deliberately not defaulted to the public host the JS SDK names: that
    /// host serves a web page and 404s the API, which would refuse every key.
    /// Point this at a second oracle you control if you want redundancy.
    #[serde(default = "default_wot_fallback_oracle_url")]
    pub fallback_oracle_url: String,
    /// Keys further than this from every root are refused. 2 is the useful
    /// setting: 1 is "only people a root follows" (which follow sync already
    /// does), and beyond 3 is noise.
    #[serde(default = "default_wot_max_hops")]
    pub max_hops: u8,
    /// Graph roots as hex pubkeys. Empty means "use the reference accounts",
    /// which are already the accounts whose follows this relay trusts.
    #[serde(default)]
    pub roots: Vec<String>,
    #[serde(with = "humantime_serde", default = "default_wot_timeout")]
    pub timeout: Duration,
    /// How long an admission is trusted.
    #[serde(with = "humantime_serde", default = "default_wot_cache_ttl")]
    pub cache_ttl: Duration,
    /// How long a refusal is trusted. Short so a newly-followed key gets in
    /// without waiting out the positive TTL.
    #[serde(with = "humantime_serde", default = "default_wot_negative_cache_ttl")]
    pub negative_cache_ttl: Duration,
}

impl Default for WotSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            local: default_wot_local(),
            oracle_url: default_wot_oracle_url(),
            fallback_oracle_url: default_wot_fallback_oracle_url(),
            max_hops: default_wot_max_hops(),
            roots: Vec::new(),
            timeout: default_wot_timeout(),
            cache_ttl: default_wot_cache_ttl(),
            negative_cache_ttl: default_wot_negative_cache_ttl(),
        }
    }
}

impl WotSettings {
    /// Parse the configured roots. Invalid hex is dropped with a warning rather
    /// than failing startup — a typo in one root should not take the relay down.
    pub fn parsed_roots(&self) -> Vec<PublicKey> {
        self.roots
            .iter()
            .filter_map(|hex| match PublicKey::from_hex(hex) {
                Ok(pk) => Some(pk),
                Err(e) => {
                    tracing::warn!("Ignoring invalid WoT root {hex}: {e}");
                    None
                }
            })
            .collect()
    }
}

#[derive(Debug, Deserialize, Clone, Default)]
pub struct WebSocketSettings {
    #[serde(with = "humantime_serde", default = "default_max_connection_duration")]
    pub max_connection_duration: Option<Duration>,
    #[serde(with = "humantime_serde", default = "default_idle_timeout")]
    pub idle_timeout: Option<Duration>,
    #[serde(default = "default_max_connections")]
    pub max_connections: Option<usize>,
    /// Ceiling on concurrent connections from a single client address.
    ///
    /// `max_connections` alone is a self-service outage: one host can take every
    /// slot and lock everybody else out. Nothing else in the stack is per-IP --
    /// the rate limits are per-connection and per-pubkey, and both only begin to
    /// apply after an AUTH an attacker never has to complete.
    #[serde(default = "default_max_connections_per_ip")]
    pub max_connections_per_ip: Option<usize>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ObeliskIndexSettings {
    #[serde(default = "default_obelisk_index_enabled")]
    pub enabled: bool,
    #[serde(default = "default_obelisk_recent_per_group")]
    pub recent_per_group: usize,
    #[serde(default = "default_obelisk_max_bootstrap_groups")]
    pub max_bootstrap_groups: usize,
    #[serde(default = "default_obelisk_max_page_limit")]
    pub max_page_limit: usize,
    #[serde(default = "default_obelisk_bootstrap_requests_per_minute")]
    pub bootstrap_requests_per_minute: u32,
    #[serde(default = "default_obelisk_message_requests_per_minute")]
    pub message_requests_per_minute: u32,
    #[serde(
        with = "humantime_serde",
        default = "default_obelisk_reconcile_interval"
    )]
    pub reconcile_interval: Duration,
}

impl Default for ObeliskIndexSettings {
    fn default() -> Self {
        Self {
            enabled: default_obelisk_index_enabled(),
            recent_per_group: default_obelisk_recent_per_group(),
            max_bootstrap_groups: default_obelisk_max_bootstrap_groups(),
            max_page_limit: default_obelisk_max_page_limit(),
            bootstrap_requests_per_minute: default_obelisk_bootstrap_requests_per_minute(),
            message_requests_per_minute: default_obelisk_message_requests_per_minute(),
            reconcile_interval: default_obelisk_reconcile_interval(),
        }
    }
}

fn default_max_connection_duration() -> Option<Duration> {
    Some(Duration::from_secs(10 * 60)) // 10 minutes default
}

fn default_idle_timeout() -> Option<Duration> {
    Some(Duration::from_secs(10 * 60)) // 10 minutes default, same as max_connection_duration
}

/// Well above honest use -- a shared NAT, a household, or one person with
/// several tabs and a phone are all normal -- so this only bites on a flood.
fn default_max_connections_per_ip() -> Option<usize> {
    Some(32)
}

fn default_max_connections() -> Option<usize> {
    Some(1000) // Default max connections
}

fn default_max_limit() -> usize {
    500 // Default/maximum limit for queries
}

fn default_max_subscriptions() -> usize {
    50 // Default max subscriptions per connection
}

fn default_obelisk_index_enabled() -> bool {
    true
}

fn default_wot_local() -> bool {
    true
}

fn default_wot_oracle_url() -> String {
    "http://wot-oracle:8080".to_string()
}

fn default_wot_fallback_oracle_url() -> String {
    String::new()
}

fn default_wot_max_hops() -> u8 {
    2
}

fn default_wot_timeout() -> Duration {
    Duration::from_secs(5)
}

fn default_wot_cache_ttl() -> Duration {
    Duration::from_secs(6 * 60 * 60)
}

fn default_wot_negative_cache_ttl() -> Duration {
    Duration::from_secs(10 * 60)
}

fn default_enable_indexed_search() -> bool {
    true
}

fn default_obelisk_recent_per_group() -> usize {
    50
}

fn default_obelisk_max_bootstrap_groups() -> usize {
    500
}

fn default_obelisk_max_page_limit() -> usize {
    100
}

fn default_obelisk_bootstrap_requests_per_minute() -> u32 {
    30
}

fn default_obelisk_message_requests_per_minute() -> u32 {
    120
}

fn default_obelisk_reconcile_interval() -> Duration {
    Duration::from_secs(5 * 60)
}

/// Make sure the relay has a unique identity of its own, generating one on
/// first start.
///
/// A fresh deployment should be "start the container, open the UI" — but that
/// only works if the relay can mint its own key. Without this, an operator who
/// never set `relay_secret_key` silently ran on the committed default
/// ([`COMPROMISED_DEFAULT_SECRET_KEY`]), whose private half is public.
///
/// Returns the hex secret key to use. Writes to `settings.local.yml` when a new
/// key is minted so the identity survives a restart — a relay whose pubkey
/// changed on every boot would invalidate its own group state each time.
pub fn ensure_relay_identity(config_dir: &Path, current: &str) -> Result<String, anyhow::Error> {
    let trimmed = current.trim();

    let reason = if trimmed.is_empty() {
        "no relay_secret_key configured"
    } else if trimmed.eq_ignore_ascii_case(COMPROMISED_DEFAULT_SECRET_KEY) {
        "relay_secret_key is the publicly-known default from config/settings.yml"
    } else {
        return Ok(trimmed.to_string());
    };

    let keys = Keys::generate();
    let secret_hex = keys.secret_key().to_secret_hex();

    let path = config_dir.join("settings.local.yml");
    let existing = std::fs::read_to_string(&path).unwrap_or_else(|_| "relay:\n".to_string());
    let updated = upsert_relay_secret_key(&existing, &secret_hex);
    std::fs::write(&path, updated)?;

    info!(
        "Generated a new relay identity ({}): pubkey {}. Persisted to {}.",
        reason,
        keys.public_key().to_hex(),
        path.display()
    );

    Ok(secret_hex)
}

/// Insert or replace `relay_secret_key` under the top-level `relay:` key,
/// preserving everything else in the file.
fn upsert_relay_secret_key(contents: &str, secret_hex: &str) -> String {
    let line = format!("  relay_secret_key: \"{secret_hex}\"");
    let mut out: Vec<String> = Vec::new();
    let mut replaced = false;

    for raw in contents.lines() {
        if !replaced && raw.trim_start().starts_with("relay_secret_key:") {
            out.push(line.clone());
            replaced = true;
        } else {
            out.push(raw.to_string());
        }
    }

    if !replaced {
        if !out.iter().any(|l| l.trim_start().starts_with("relay:")) {
            out.insert(0, "relay:".to_string());
        }
        let insert_at = out
            .iter()
            .position(|l| l.trim_start().starts_with("relay:"))
            .map_or(out.len(), |i| i + 1);
        out.insert(insert_at, line);
    }

    let mut joined = out.join("\n");
    if !joined.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

#[cfg(test)]
mod identity_tests {
    use super::*;

    #[test]
    fn replaces_an_existing_key_in_place() {
        let out = upsert_relay_secret_key(
            "relay:\n  relay_secret_key: \"old\"\n  relay_url: \"ws://x\"\n",
            "new",
        );
        assert!(out.contains("relay_secret_key: \"new\""));
        assert!(!out.contains("\"old\""));
        // Everything else survives.
        assert!(out.contains("relay_url: \"ws://x\""));
    }

    #[test]
    fn inserts_under_existing_relay_key() {
        let out = upsert_relay_secret_key("relay:\n  relay_url: \"ws://x\"\n", "k");
        assert!(out.starts_with("relay:\n  relay_secret_key: \"k\""));
        assert!(out.contains("relay_url: \"ws://x\""));
    }

    #[test]
    fn creates_the_relay_key_when_absent() {
        let out = upsert_relay_secret_key("", "k");
        assert!(out.contains("relay:"));
        assert!(out.contains("relay_secret_key: \"k\""));
    }

    #[test]
    fn keeps_an_operator_supplied_key() {
        let dir = std::env::temp_dir();
        let key = "a".repeat(64);
        assert_eq!(ensure_relay_identity(&dir, &key).unwrap(), key);
    }

    #[test]
    fn regenerates_the_publicly_known_default() {
        let dir = std::env::temp_dir().join("obelisk-identity-test");
        std::fs::create_dir_all(&dir).unwrap();
        let out = ensure_relay_identity(&dir, COMPROMISED_DEFAULT_SECRET_KEY).unwrap();
        assert_ne!(out, COMPROMISED_DEFAULT_SECRET_KEY);
        assert_eq!(out.len(), 64);
        std::fs::remove_dir_all(&dir).ok();
    }
}

impl RelaySettings {
    pub fn relay_keys(&self) -> Result<Keys, anyhow::Error> {
        let secret_key = SecretKey::from_hex(&self.relay_secret_key)?;
        Ok(Keys::new(secret_key))
    }

    pub fn relay_url(&self) -> Result<RelayUrl, anyhow::Error> {
        Ok(RelayUrl::parse(&self.relay_url)?)
    }
}

impl WebSocketSettings {
    pub fn max_connection_duration(&self) -> Option<Duration> {
        self.max_connection_duration
            .or_else(default_max_connection_duration)
    }

    pub fn idle_timeout(&self) -> Option<Duration> {
        self.idle_timeout.or_else(default_idle_timeout)
    }

    pub fn max_connections(&self) -> Option<usize> {
        self.max_connections.or_else(default_max_connections)
    }

    pub fn max_connections_per_ip(&self) -> Option<usize> {
        self.max_connections_per_ip
            .or_else(default_max_connections_per_ip)
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    config: ConfigTree,
}

impl Config {
    pub fn new<P: AsRef<Path>>(config_dir: P) -> Result<Self, ConfigError> {
        let environment =
            std::env::var(format!("{ENVIRONMENT_PREFIX}{CONFIG_SEPARATOR}ENVIRONMENT"))
                .unwrap_or_else(|_| "development".into());

        let config_dir = config_dir.as_ref();
        let default_config = config_dir.join("settings.yml");
        let env_config = config_dir.join(format!("settings.{environment}.yml"));
        let local_config = config_dir.join("settings.local.yml");

        let config = ConfigTree::builder()
            .add_source(File::from(default_config))
            .add_source(File::from(env_config).required(false))
            .add_source(File::from(local_config).required(false))
            .add_source(
                Environment::with_prefix(ENVIRONMENT_PREFIX)
                    .separator(CONFIG_SEPARATOR)
                    .try_parsing(true),
            )
            .build()?;

        Ok(Config { config })
    }

    pub fn get_settings(&self) -> Result<RelaySettings, ConfigError> {
        let settings: RelaySettings = self.config.get("relay")?;
        // Only log non-sensitive WebSocket settings
        info!(
            "WebSocket settings: max_connections={:?}, max_connection_duration={:?}, idle_timeout={:?}",
            settings.websocket.max_connections,
            settings.websocket.max_connection_duration,
            settings.websocket.idle_timeout,
        );
        Ok(settings)
    }
}

pub struct Settings {
    pub relay_url: String,
    pub local_addr: String,
    pub admin_keys: Vec<String>,
    pub whitelisted_pubkeys: Vec<String>,
    pub websocket: WebSocketSettings,
    pub db_path: String,
    pub max_limit: usize,
    pub max_subscriptions: usize,
    pub relay_name: Option<String>,
    pub relay_description: Option<String>,
    pub relay_icon: Option<String>,
    pub event_retention: Option<Duration>,
    pub enable_event_pruner: bool,
    pub prune_interval: Option<Duration>,
    pub prune_kinds: Option<Vec<u16>>,
    pub prune_retention_by_kind: Option<std::collections::BTreeMap<u16, Duration>>,
    pub pubkey_rate_limit_per_minute: Option<u32>,
    pub connection_rate_limit_per_minute: Option<u32>,
    pub global_rate_limit_per_minute: Option<u32>,
    pub force_public_groups: bool,
    pub enable_indexed_search: bool,
    pub advertise_indexed_search: Option<bool>,
    pub obelisk_index: ObeliskIndexSettings,
    pub wot: WotSettings,
}

impl Settings {
    pub fn should_advertise_indexed_search(&self) -> bool {
        self.enable_indexed_search && self.advertise_indexed_search.unwrap_or(true)
    }
}

pub use nostr_sdk::Keys;

#[cfg(test)]
mod policy_config_tests {
    use super::*;

    /// The admin API writes `prune_retention_by_kind` as a single-line YAML flow
    /// map. If that does not parse back, a saved policy silently does nothing --
    /// the pruner would read no policies and quietly delete nothing (or, worse,
    /// fall through to the legacy single window).
    #[test]
    fn flow_map_written_by_the_admin_api_round_trips() {
        let dir = std::env::temp_dir().join(format!("obelisk-policy-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(
            dir.join("settings.yml"),
            "relay:\n  relay_secret_key: \"\"\n  local_addr: \"127.0.0.1:1\"\n  relay_url: \"ws://127.0.0.1:1\"\n  db_path: \"db\"\n",
        )
        .unwrap();
        // Exactly the shape upsert_relay_value writes.
        std::fs::write(
            dir.join("settings.local.yml"),
            "relay:\n  prune_retention_by_kind: {1059: \"30d\", 2390: \"7d\"}\n",
        )
        .unwrap();

        let settings = Config::new(&dir).unwrap().get_settings().unwrap();
        let policies = settings
            .prune_retention_by_kind
            .expect("flow map parses into a policy map");

        assert_eq!(policies.len(), 2);
        assert_eq!(
            policies.get(&1059).map(Duration::as_secs),
            Some(30 * 86_400),
            "gift wrap window"
        );
        assert_eq!(
            policies.get(&2390).map(Duration::as_secs),
            Some(7 * 86_400),
            "game event window"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
