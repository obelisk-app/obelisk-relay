use crate::{
    admin, app_state::HttpServerState, blacklist::Blacklist, config, follow_sync, groups::Groups,
    groups_event_processor::GroupsRelayProcessor, handler, metrics,
    metrics_handler::PrometheusSubscriptionMetricsHandler,
    pruner::{self, PrunerConfig, PrunerStats},
    reference_accounts::ReferenceAccounts,
    sampled_metrics_handler::SampledMetricsHandler, whitelist::Whitelist, RelayDatabase,
};
use anyhow::Result;
use axum::{
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use governor::Quota;
use relay_builder::{handle_upgrade, HandlerFactory, WebSocketUpgrade};
use nostr_sdk::prelude::PublicKey;
use relay_builder::{
    middlewares::RateLimitMiddleware, CryptoHelper, Nip40ExpirationMiddleware, Nip70Middleware,
    RelayBuilder, RelayConfig, RelayInfo, WebSocketConfig,
};
use std::num::NonZeroU32;
use std::net::SocketAddr;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::Duration;
use tokio::time;
use tokio_util::sync::CancellationToken;
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;
use tower_http::timeout::TimeoutLayer;
use tracing::info;

const NOSTR_JSON_CONTENT_TYPE: &str = "application/nostr+json";
pub const SUPPORTED_NIPS: [u16; 7] = [1, 9, 11, 29, 40, 42, 70];

pub struct ServerState {
    pub http_state: Arc<HttpServerState>,
    pub cancellation_token: CancellationToken,
    pub metrics_handle: metrics::PrometheusHandle,
    pub connection_counter: Arc<AtomicUsize>,
    pub relay_url: String,
    pub relay_pubkey: String,
    pub whitelist: Whitelist,
    pub reference_accounts: ReferenceAccounts,
    pub start_time: std::time::Instant,
    pub config_dir: String,
    pub db_path: String,
    /// Pruner stats; None when pruning is disabled.
    pub pruner_stats: Option<Arc<PrunerStats>>,
    /// Active pruner config; mirrors `pruner_stats` presence.
    pub pruner_config: Option<PrunerConfig>,
    pub relay_name: String,
    pub relay_description: String,
}

fn accepts_nostr_json(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::ACCEPT)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| {
            value.split(',').any(|part| {
                part.split(';')
                    .next()
                    .map(str::trim)
                    .is_some_and(|media_type| {
                        media_type.eq_ignore_ascii_case(NOSTR_JSON_CONTENT_TYPE)
                    })
            })
        })
}

fn relay_info_response(relay_info: &RelayInfo) -> Response {
    match serde_json::to_string(relay_info) {
        Ok(body) => ([(header::CONTENT_TYPE, NOSTR_JSON_CONTENT_TYPE)], body).into_response(),
        Err(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to serialize relay information: {err}"),
        )
            .into_response(),
    }
}

pub async fn run_server(
    settings: config::Settings,
    relay_keys: config::Keys,
    database: Arc<RelayDatabase>,
    groups: Arc<Groups>,
) -> Result<()> {
    // Setup metrics
    let metrics_handle = metrics::setup_metrics()?;
    let http_state = Arc::new(HttpServerState::new(groups.clone()));

    info!(
        "Listening for websocket connections at: {}",
        settings.local_addr
    );
    info!("Frontend URL: {}", settings.local_addr);
    info!("Relay URL: {}", settings.relay_url);
    info!(
        "Auth requests must match: {} (with matching subdomain if present)",
        settings.relay_url
    );

    // Build the relay configuration
    let websocket_config = WebSocketConfig {
        max_connections: settings.websocket.max_connections(),
        max_connection_duration: settings
            .websocket
            .max_connection_duration()
            .map(|d| d.as_secs()),
        idle_timeout: settings.websocket.idle_timeout().map(|d| d.as_secs()),
    };

    let _crypto_helper = CryptoHelper::new(Arc::new(relay_keys.clone()));
    // Keep a handle to the database for the background pruner before moving it into RelayConfig.
    let database_for_pruner = Arc::clone(&database);
    let mut relay_config =
        RelayConfig::new(settings.relay_url.clone(), database, relay_keys.clone())
            .with_subdomains_from_url(&settings.relay_url)
            .with_websocket_config(websocket_config)
            .with_subscription_limits(settings.max_subscriptions, settings.max_limit)
            .with_diagnostics();

    // Enable NIP-42 authentication
    relay_config.enable_auth = true;

    // Parse whitelisted pubkeys and create shared whitelist
    let initial_whitelist: Vec<PublicKey> = settings
        .whitelisted_pubkeys
        .iter()
        .filter_map(|hex| PublicKey::from_hex(hex).ok())
        .collect();
    let config_dir = std::path::Path::new("config");
    let blacklist = Blacklist::new(Some(config_dir));
    if blacklist.len() > 0 {
        info!("Blacklist loaded: {} pubkeys blocked", blacklist.len());
    }
    let whitelist = Whitelist::new(initial_whitelist, Some(config_dir), blacklist);
    if !whitelist.is_empty() {
        info!("Whitelist enabled: {} pubkeys allowed", whitelist.len());
    }

    // Load reference accounts
    let reference_accounts = ReferenceAccounts::new(Some(config_dir));
    if reference_accounts.len() > 0 {
        info!(
            "Reference accounts loaded: {} accounts",
            reference_accounts.len()
        );
    }

    // Load follow-derived whitelist
    let follow_derived = follow_sync::load_follow_derived(config_dir);
    if !follow_derived.is_empty() {
        info!(
            "Follow-derived whitelist loaded: {} pubkeys",
            follow_derived.len()
        );
        whitelist.set_follow_derived(follow_derived);
    }

    // Parse admin pubkeys
    let mut admin_pubkeys: Vec<PublicKey> = settings
        .admin_keys
        .iter()
        .filter_map(|hex| PublicKey::from_hex(hex).ok())
        .collect();
    for pk in admin::load_runtime_admin_pubkeys(config_dir) {
        if !admin_pubkeys.contains(&pk) {
            admin_pubkeys.push(pk);
        }
    }
    if !admin_pubkeys.is_empty() {
        info!("Admin panel enabled: {} admin pubkeys", admin_pubkeys.len());
    }
    admin::init_admin_state(
        admin_pubkeys.clone(),
        settings.relay_url.clone(),
        "config".to_string(),
    );

    let mut groups_processor =
        GroupsRelayProcessor::with_admin_pubkeys(
            groups.clone(),
            relay_keys.public_key,
            admin_pubkeys.clone(),
            whitelist.clone(),
        );
    if let Some(per_minute) = settings.pubkey_rate_limit_per_minute {
        if per_minute > 0 {
            info!("Per-pubkey rate limit enabled: {} events/minute", per_minute);
            groups_processor = groups_processor.with_pubkey_rate_limit(per_minute);
        }
    }

    // Create cancellation token and connection counter
    let cancellation_token = CancellationToken::new();
    let connection_counter = Arc::new(AtomicUsize::new(0));

    // Background event retention is destructive. It is disabled unless both
    // event_retention is configured and enable_event_pruner is explicitly true.
    // This keeps stale/example retention settings from silently deleting relay data.
    let (pruner_stats, pruner_config_opt) = if settings.enable_event_pruner {
        if let Some(retention) = settings.event_retention {
            if retention.as_secs() > 0 {
                let cfg = PrunerConfig::from_settings(
                    retention,
                    settings.prune_interval,
                    settings.prune_kinds.clone(),
                );
                let stats = Arc::new(PrunerStats::default());
                pruner::spawn(
                    database_for_pruner.clone(),
                    cfg.clone(),
                    stats.clone(),
                    cancellation_token.clone(),
                );
                (Some(stats), Some(cfg))
            } else {
                tracing::info!("Event pruner disabled: event_retention is zero");
                (None, None)
            }
        } else {
            tracing::warn!(
                "enable_event_pruner=true but event_retention is unset; pruner disabled"
            );
            (None, None)
        }
    } else {
        if settings.event_retention.is_some() {
            tracing::warn!(
                "Event retention is configured but enable_event_pruner=false; automatic deletion is disabled"
            );
        }
        (None, None)
    };

    // Define relay information (advertised name/description configurable per instance).
    let relay_name = settings
        .relay_name
        .clone()
        .unwrap_or_else(|| "Obelisk Groups Relay".to_string());
    let relay_description = settings.relay_description.clone().unwrap_or_else(|| {
        if whitelist.is_empty() {
            "NIP-29 groups relay for Obelisk. Public access.".to_string()
        } else {
            "NIP-29 groups relay for Obelisk. Auth-required, whitelisted access.".to_string()
        }
    });
    let _relay_info = RelayInfo {
        name: relay_name.clone(),
        description: relay_description.clone(),
        pubkey: relay_keys.public_key.to_string(),
        contact: "npub1m9vsm9d8sy0pevcjhenwm4ny6l37dm2hsg4dnusna43ql3n5305qy4zlg4".to_string(),
        supported_nips: SUPPORTED_NIPS.to_vec(),
        software: "groups_relay".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        icon: None,
    };

    // Build per-connection + global event rate limits. We always install the
    // middleware (with effectively-unlimited defaults) so the static middleware
    // chain type is fixed regardless of config. Configured values bring the cap
    // down to the desired protection level.
    fn quota_per_minute(n: u32, fallback: u32) -> Quota {
        let n = if n == 0 { fallback } else { n };
        let nz = NonZeroU32::new(n).unwrap_or_else(|| NonZeroU32::new(fallback).unwrap());
        Quota::per_minute(nz)
    }
    let per_conn_quota = quota_per_minute(
        settings.connection_rate_limit_per_minute.unwrap_or(0),
        // default: 600/min (~10/s) — generous; tighten via settings.local.yml.
        600,
    );
    let global_quota = quota_per_minute(
        settings.global_rate_limit_per_minute.unwrap_or(0),
        // default: 60_000/min — effectively unlimited unless explicitly configured.
        60_000,
    );
    if let Some(n) = settings.connection_rate_limit_per_minute {
        info!("Per-connection rate limit: {} events/minute", n);
    }
    if let Some(n) = settings.global_rate_limit_per_minute {
        info!("Global rate limit: {} events/minute", n);
    }
    let rate_limiter = RateLimitMiddleware::<()>::with_global_limit(per_conn_quota, global_quota);

    // Build the relay service
    let handler_factory = Arc::new(
        RelayBuilder::<(), GroupsRelayProcessor>::new(relay_config)
            .cancellation_token(cancellation_token.clone())
            .connection_counter(connection_counter.clone())
            .metrics(SampledMetricsHandler::new(10))
            .subscription_metrics(PrometheusSubscriptionMetricsHandler)
            .event_processor(groups_processor)
            .relay_info(_relay_info.clone())
            .build_with(|chain| {
                chain
                    .with(rate_limiter)
                    .with(Nip40ExpirationMiddleware::new())
                    .with(Nip70Middleware)
            })
            .await?,
    );

    let app_state = Arc::new(ServerState {
        http_state: http_state.clone(),
        cancellation_token: cancellation_token.clone(),
        metrics_handle: metrics_handle.clone(),
        connection_counter: connection_counter.clone(),
        relay_url: settings.relay_url.clone(),
        relay_pubkey: relay_keys.public_key.to_hex(),
        whitelist: whitelist.clone(),
        reference_accounts: reference_accounts.clone(),
        start_time: std::time::Instant::now(),
        config_dir: "config".to_string(),
        db_path: settings.db_path.clone(),
        pruner_stats,
        pruner_config: pruner_config_opt,
        relay_name: relay_name.clone(),
        relay_description: relay_description.clone(),
    });

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // Metrics handler without state
    let metrics_handler = move || async move { metrics_handle.render() };

    // Create a unified handler that supports both WebSocket and HTTP on the same route
    let root_handler = {
        let handler_factory = handler_factory.clone();
        let relay_info = _relay_info.clone();
        move |ws: Option<WebSocketUpgrade>,
              axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<SocketAddr>,
              headers: axum::http::HeaderMap| {
            let handler_factory = handler_factory.clone();
            let relay_info = relay_info.clone();

            async move {
                match ws {
                    Some(ws) => {
                        // Handle WebSocket upgrade
                        let handler = handler_factory.create(&headers);
                        handle_upgrade(ws, addr, handler).await
                    }
                    None => {
                        // Check for NIP-11 JSON request
                        if accepts_nostr_json(&headers) {
                            return relay_info_response(&relay_info);
                        }

                        // Serve frontend
                        handler::serve_frontend().await.into_response()
                    }
                }
            }
        }
    };

    // Create API routes with state and timeout protection
    // Note: Timeout is applied only to API routes, not WebSocket connections
    let api_routes = Router::new()
        .route("/api/subdomains", get(handler::handle_subdomains))
        .route("/api/config", get(handler::handle_config))
        .nest("/api/admin", admin::admin_routes())
        .nest("/api", admin::public_api_routes())
        .layer(TimeoutLayer::new(Duration::from_secs(30)))
        .with_state(app_state);

    // Build router (WebSocket and static files do not have timeouts)
    let router = Router::new()
        .route("/", get(root_handler))
        .route("/health", get(|| async { "OK" }))
        .route("/metrics", get(metrics_handler))
        .merge(api_routes)
        .fallback_service(ServeDir::new("frontend/dist").fallback(tower_http::services::ServeFile::new("frontend/dist/index.html")))
        .layer(cors);

    let addr = settings.local_addr.parse::<SocketAddr>()?;
    let handle = axum_server::Handle::new();
    let handle_clone = handle.clone();

    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.unwrap();
        info!("Shutdown signal received");
        handle_clone.graceful_shutdown(Some(std::time::Duration::from_secs(5)));
        cancellation_token.cancel();
    });

    // Start metrics loop
    let groups_for_metrics = Arc::clone(&groups);
    tokio::spawn(async move {
        let mut interval = time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;

            // Update total groups by privacy settings
            for (private, closed, count) in groups_for_metrics.count_groups_by_privacy() {
                metrics::groups_by_privacy(private, closed).set(count as f64);
            }
        }
    });

    info!("Starting server on {}", addr);
    axum_server::bind(addr)
        .handle(handle.clone())
        .serve(router.into_make_service_with_connect_info::<SocketAddr>())
        .await
        .unwrap();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn test_relay_info() -> RelayInfo {
        RelayInfo {
            name: "Test Relay".to_string(),
            description: "Test relay information".to_string(),
            pubkey: "0000000000000000000000000000000000000000000000000000000000000000"
                .to_string(),
            contact: "mailto:ops@example.com".to_string(),
            supported_nips: SUPPORTED_NIPS.to_vec(),
            software: "https://example.com/relay".to_string(),
            version: "0.0.0-test".to_string(),
            icon: None,
        }
    }

    #[test]
    fn accepts_nostr_json_media_type() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("text/html, application/nostr+json; q=1"),
        );

        assert!(accepts_nostr_json(&headers));
    }

    #[test]
    fn rejects_non_nostr_json_accept_header() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, HeaderValue::from_static("text/html"));

        assert!(!accepts_nostr_json(&headers));
    }

    #[test]
    fn relay_info_response_uses_nip11_content_type() {
        let response = relay_info_response(&test_relay_info());

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE),
            Some(&HeaderValue::from_static(NOSTR_JSON_CONTENT_TYPE))
        );
    }
}
