//! Web-of-Trust admission.
//!
//! Asks a [`nostr-wot-oracle`] how many follow-graph hops separate a connecting
//! pubkey from this relay's roots (its reference accounts, by default). A key
//! within `max_hops` of any root is admitted as a whitelist tier — see
//! [`crate::whitelist::Whitelist::contains`].
//!
//! [`nostr-wot-oracle`]: https://github.com/nostr-wot/nostr-wot-oracle
//!
//! # The wire format
//!
//! `GET /distance?from=<hex>&to=<hex>&max_hops=<n>` returns 200 with
//! `{"from","to","hops":<number|null>,"path_count","mutual_follow"}`. An
//! unreachable key is `hops: null` on a **200**, not a 404 — so any non-success
//! status is a broken endpoint, not a verdict, and must count as a failure. The
//! `nostr-wot-sdk` JS client documents a different `/api/distance/{from}/{to}`
//! shape; that is not what the oracle serves.
//!
//! # Why the split between [`WotOracle::resolve`] and [`WotOracle::cached`]
//!
//! `EventProcessor::verify_filters` and `can_see_event` are synchronous, so the
//! admission check on the hot path cannot await an HTTP request. Resolution
//! therefore happens once per connection in `WotAdmissionMiddleware`, which runs
//! after NIP-42 auth and before the first REQ/EVENT reaches the processor; by
//! then the answer is in the cache and the hot path only does a map lookup.
//!
//! # Failure behaviour
//!
//! An unreachable oracle **never admits**. Failures are not cached, so a blip
//! does not lock a legitimate key out for a whole TTL, and after
//! [`FAILURE_THRESHOLD`] consecutive failures the breaker opens so a dead
//! sidecar costs one timeout per minute instead of one per connection. The
//! oracle can only ever *add* access: manual and follow-derived entries are
//! checked first, and the blacklist overrides everything.

use crate::wot_graph::FollowGraph;
use dashmap::DashMap;
use nostr_sdk::prelude::PublicKey;
use parking_lot::RwLock;
use serde::Deserialize;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Host the `nostr-wot-sdk` names as the public oracle.
///
/// Kept for documentation only — **do not default to it**. As of writing it
/// serves the project's marketing site and answers 404 on `/distance` and even
/// `/health`, so pointing the relay at it would refuse every key while looking
/// like a working oracle. Run the sidecar instead.
pub const SDK_PUBLIC_ORACLE_HOST: &str = "https://wot-oracle.mappingbitcoin.com";

/// Consecutive oracle failures before the breaker opens.
const FAILURE_THRESHOLD: u32 = 3;

/// How long the breaker stays open before one probe is allowed through.
const BREAKER_COOLDOWN: Duration = Duration::from_secs(60);

/// How many hops separate a pubkey from the nearest root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verdict {
    /// Shortest hop count to any root, or `None` when no root reaches the key
    /// within `max_hops`.
    pub hops: Option<u8>,
}

impl Verdict {
    const UNREACHABLE: Self = Self { hops: None };

    fn reachable(hops: u8) -> Self {
        Self { hops: Some(hops) }
    }
}

#[derive(Debug, Clone)]
struct CachedVerdict {
    verdict: Verdict,
    expires_at: Instant,
}

/// Everything the oracle client needs, resolved from `relay.wot` in config.
#[derive(Debug, Clone)]
pub struct WotConfig {
    /// Base URL of the oracle to try first, e.g. `http://wot-oracle:3000`.
    pub oracle_url: String,
    /// Tried when `oracle_url` errors. `None` disables the fallback.
    pub fallback_oracle_url: Option<String>,
    /// Keys further than this from every root are refused.
    pub max_hops: u8,
    /// Per-request timeout.
    pub timeout: Duration,
    /// How long an admission is trusted. Membership is stable, so this is long.
    pub cache_ttl: Duration,
    /// How long a refusal is trusted. Short, so someone who was just followed
    /// gets in without waiting out the positive TTL.
    pub negative_cache_ttl: Duration,
    /// Compute distances from a follow graph this relay builds itself instead
    /// of asking an oracle. Removes the sidecar, the HTTP hop and the
    /// third-party dependency; see [`crate::wot_graph`].
    pub local: bool,
}

impl Default for WotConfig {
    fn default() -> Self {
        Self {
            oracle_url: "http://wot-oracle:8080".to_string(),
            fallback_oracle_url: None,
            max_hops: 2,
            timeout: Duration::from_secs(5),
            cache_ttl: Duration::from_secs(6 * 60 * 60),
            negative_cache_ttl: Duration::from_secs(10 * 60),
            local: true,
        }
    }
}

/// Distance queries against a WoT oracle, with a decision cache the sync
/// admission path can read.
#[derive(Debug)]
pub struct WotOracle {
    http: reqwest::Client,
    primary: String,
    fallback: Option<String>,
    config: WotConfig,
    /// Graph roots. Defaults to the relay's reference accounts, which are
    /// already the accounts whose follows this relay trusts.
    roots: RwLock<Vec<PublicKey>>,
    cache: DashMap<PublicKey, CachedVerdict>,
    consecutive_failures: AtomicU32,
    breaker_opened_at: RwLock<Option<Instant>>,
    /// Present when running in local mode. Consulted instead of the oracle.
    graph: Option<Arc<FollowGraph>>,
}

impl WotOracle {
    /// Build a client. Fails only if the HTTP client cannot be constructed.
    pub fn new(config: WotConfig, roots: Vec<PublicKey>) -> Result<Arc<Self>, String> {
        let http = reqwest::Client::builder()
            .timeout(config.timeout)
            .user_agent("obelisk-relay-wot/1")
            .build()
            .map_err(|e| format!("failed to build WoT HTTP client: {e}"))?;

        let config_local = config.local;
        let primary = normalize_base(&config.oracle_url);
        let fallback = config
            .fallback_oracle_url
            .as_deref()
            .map(normalize_base)
            // A fallback identical to the primary would just double every
            // timeout for no extra coverage.
            .filter(|f| f != &primary);

        Ok(Arc::new(Self {
            http,
            primary,
            fallback,
            config,
            roots: RwLock::new(roots),
            cache: DashMap::new(),
            consecutive_failures: AtomicU32::new(0),
            breaker_opened_at: RwLock::new(None),
            graph: if config_local {
                Some(FollowGraph::new())
            } else {
                None
            },
        }))
    }

    pub fn max_hops(&self) -> u8 {
        self.config.max_hops
    }

    pub fn oracle_url(&self) -> &str {
        &self.primary
    }

    /// Replace the graph roots, e.g. after the operator edits reference
    /// accounts. Clears the cache, since every cached verdict was measured
    /// against the old roots.
    pub fn set_roots(&self, roots: Vec<PublicKey>) {
        *self.roots.write() = roots;
        self.cache.clear();
    }

    pub fn roots(&self) -> Vec<PublicKey> {
        self.roots.read().clone()
    }

    /// True when the oracle has failed enough times that queries are being
    /// skipped. Surfaced in the admin panel — a degraded oracle silently
    /// admitting nobody is exactly the state an operator needs to see.
    pub fn is_degraded(&self) -> bool {
        // A local graph cannot be "degraded"; it is either built or not yet.
        if self.graph.is_some() {
            return false;
        }
        self.consecutive_failures.load(Ordering::Relaxed) >= FAILURE_THRESHOLD
    }

    /// The local follow graph, when running in local mode.
    pub fn graph(&self) -> Option<&Arc<FollowGraph>> {
        self.graph.as_ref()
    }

    pub fn is_local(&self) -> bool {
        self.graph.is_some()
    }

    /// Cached verdict for `pk`, or `None` when absent or expired.
    ///
    /// Synchronous: this is what the admission hot path calls.
    pub fn cached(&self, pk: &PublicKey) -> Option<Verdict> {
        let entry = self.cache.get(pk)?;
        if entry.expires_at <= Instant::now() {
            return None;
        }
        Some(entry.verdict)
    }

    /// Whether `pk` is currently admitted by the WoT tier. Synchronous, and
    /// false for anything not yet resolved.
    pub fn admits(&self, pk: &PublicKey) -> bool {
        self.cached(pk)
            .and_then(|v| v.hops)
            .is_some_and(|hops| hops <= self.config.max_hops)
    }

    /// Pubkeys currently admitted through the WoT tier, with their hop count.
    /// Display only — this is a cache of keys that have connected, not the set
    /// of everyone the graph would admit.
    pub fn admitted(&self) -> Vec<(PublicKey, u8)> {
        let now = Instant::now();
        let mut out: Vec<(PublicKey, u8)> = self
            .cache
            .iter()
            .filter(|e| e.expires_at > now)
            .filter_map(|e| e.verdict.hops.map(|hops| (*e.key(), hops)))
            .filter(|(_, hops)| *hops <= self.config.max_hops)
            .collect();
        out.sort_by_key(|(_, hops)| *hops);
        out
    }

    /// Drop expired entries. Called periodically so a long-running relay does
    /// not accumulate one entry per pubkey that ever connected.
    /// Drop every cached verdict, e.g. after the follow graph is rebuilt.
    pub fn clear_cache(&self) {
        self.cache.clear();
    }

    pub fn prune_cache(&self) {
        let now = Instant::now();
        self.cache.retain(|_, v| v.expires_at > now);
    }

    pub fn cache_len(&self) -> usize {
        self.prune_cache();
        self.cache.len()
    }

    /// Resolve `pk` against every root, caching the result.
    ///
    /// Returns the cached verdict when one is live. On oracle failure returns
    /// [`Verdict::UNREACHABLE`] **without caching**, so the next attempt retries.
    pub async fn resolve(&self, pk: &PublicKey) -> Verdict {
        if let Some(verdict) = self.cached(pk) {
            return verdict;
        }

        let roots = self.roots.read().clone();
        if roots.is_empty() {
            // No roots means no graph to measure against. Cache it negatively
            // so we do not rebuild this conclusion on every connection.
            self.store(pk, Verdict::UNREACHABLE);
            return Verdict::UNREACHABLE;
        }

        // Local mode answers from our own graph: no network, no breaker, and
        // nothing to be unreachable.
        if let Some(graph) = &self.graph {
            let verdict = match graph.distance(&roots, pk, self.config.max_hops) {
                Some(hops) => Verdict::reachable(hops),
                None => Verdict::UNREACHABLE,
            };
            // A path that was found is real: the graph can be incomplete but
            // never invents edges, so a positive answer is safe to cache even
            // mid-build. A *negative* one is not — caching "unreachable"
            // against a half-built graph would lock people out for the whole
            // negative TTL for no reason.
            if verdict.hops.is_some() || graph.is_built() {
                self.store(pk, verdict);
            }
            return verdict;
        }

        if self.breaker_open() {
            debug!(
                target: "wot",
                "Breaker open, refusing {} without querying the oracle",
                pk.to_hex()
            );
            return Verdict::UNREACHABLE;
        }

        let mut best: Option<u8> = None;
        for root in &roots {
            // A key can be its own root, and the oracle need not special-case it.
            if root == pk {
                best = Some(0);
                break;
            }
            match self.query_distance(root, pk).await {
                Ok(Some(hops)) => {
                    best = Some(best.map_or(hops, |b: u8| b.min(hops)));
                    // Nothing can beat a direct follow, so stop paying for
                    // the remaining roots.
                    if best == Some(1) {
                        break;
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    self.record_failure(&e);
                    return Verdict::UNREACHABLE;
                }
            }
        }

        self.record_success();

        let verdict = match best {
            Some(hops) if hops <= self.config.max_hops => Verdict::reachable(hops),
            _ => Verdict::UNREACHABLE,
        };
        self.store(pk, verdict);

        match verdict.hops {
            Some(hops) => debug!(target: "wot", "{} admitted at {} hop(s)", pk.to_hex(), hops),
            None => debug!(
                target: "wot",
                "{} is beyond {} hop(s) from every root",
                pk.to_hex(),
                self.config.max_hops
            ),
        }
        verdict
    }

    fn store(&self, pk: &PublicKey, verdict: Verdict) {
        let ttl = if verdict.hops.is_some() {
            self.config.cache_ttl
        } else {
            self.config.negative_cache_ttl
        };
        self.cache.insert(
            *pk,
            CachedVerdict {
                verdict,
                expires_at: Instant::now() + ttl,
            },
        );
    }

    /// One pairwise distance query, falling back to the secondary oracle when
    /// the primary errors. `Ok(None)` means "not reachable", which is an answer
    /// rather than a failure.
    async fn query_distance(&self, from: &PublicKey, to: &PublicKey) -> Result<Option<u8>, String> {
        let path = format!(
            "/distance?from={}&to={}&max_hops={}",
            from.to_hex(),
            to.to_hex(),
            self.config.max_hops
        );

        let primary_err = match self.get_distance(&self.primary, &path).await {
            Ok(distance) => return Ok(distance),
            Err(e) => e,
        };

        let Some(fallback) = &self.fallback else {
            return Err(primary_err);
        };

        warn!(target: "wot", "Primary oracle failed ({primary_err}), trying fallback");
        self.get_distance(fallback, &path)
            .await
            .map_err(|e| format!("primary: {primary_err}; fallback: {e}"))
    }

    async fn get_distance(&self, base: &str, path: &str) -> Result<Option<u8>, String> {
        /// The subset of the oracle's `DistanceResult` we act on. `hops: null`
        /// means "no path within max_hops".
        #[derive(Deserialize)]
        struct DistanceResponse {
            hops: Option<u32>,
        }

        let url = format!("{base}{path}");
        let response = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| format!("request to {base} failed: {e}"))?;

        // Every status other than success is a broken endpoint. In particular a
        // 404 is *not* "key not found" — the oracle reports that as `hops: null`
        // on a 200. Treating 404 as a verdict would let a host that serves a web
        // page at this URL silently refuse every key without ever looking
        // degraded.
        if !response.status().is_success() {
            return Err(format!("{base} returned HTTP {}", response.status()));
        }

        let body: DistanceResponse = response
            .json()
            .await
            .map_err(|e| format!("{base} returned an unreadable body: {e}"))?;

        Ok(body.hops.map(|d| u8::try_from(d).unwrap_or(u8::MAX)))
    }

    /// Ask the oracle's `/health` endpoint whether it is up. Used by the admin
    /// panel so an operator can tell "nobody is in my WoT" from "the oracle is
    /// down".
    pub async fn health_check(&self) -> Result<(), String> {
        if let Some(graph) = &self.graph {
            let (accounts, edges) = graph.size();
            return if graph.is_built() {
                Ok(())
            } else {
                Err(format!(
                    "the follow graph is still being built ({accounts} accounts, {edges} edges so far)"
                ))
            };
        }

        let response = self
            .http
            .get(format!("{}/health", self.primary))
            .send()
            .await
            .map_err(|e| format!("{}: {e}", self.primary))?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(format!(
                "{} returned HTTP {}",
                self.primary,
                response.status()
            ))
        }
    }

    fn breaker_open(&self) -> bool {
        if self.consecutive_failures.load(Ordering::Relaxed) < FAILURE_THRESHOLD {
            return false;
        }
        // Once the cooldown elapses, let one request through to find out
        // whether the oracle came back.
        match *self.breaker_opened_at.read() {
            Some(opened) => opened.elapsed() < BREAKER_COOLDOWN,
            None => false,
        }
    }

    fn record_failure(&self, reason: &str) {
        let failures = self.consecutive_failures.fetch_add(1, Ordering::Relaxed) + 1;
        if failures == FAILURE_THRESHOLD {
            warn!(
                target: "wot",
                "WoT oracle degraded after {failures} consecutive failures ({reason}); \
                 admitting nobody through the WoT tier until it recovers"
            );
        } else {
            debug!(target: "wot", "WoT oracle query failed: {reason}");
        }
        if failures >= FAILURE_THRESHOLD {
            *self.breaker_opened_at.write() = Some(Instant::now());
        }
    }

    fn record_success(&self) {
        if self.consecutive_failures.swap(0, Ordering::Relaxed) >= FAILURE_THRESHOLD {
            info!(target: "wot", "WoT oracle recovered");
        }
        *self.breaker_opened_at.write() = None;
    }
}

/// Strip a trailing slash so `{base}{path}` never produces a double slash.
fn normalize_base(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::Query;
    use axum::http::StatusCode;
    use axum::routing::get;
    use axum::Router;
    use nostr_sdk::prelude::Keys;
    use std::collections::HashMap;
    use std::sync::atomic::AtomicUsize;

    /// Counts requests so tests can assert the breaker and cache actually
    /// suppress traffic rather than just returning the right answer.
    #[derive(Clone, Default)]
    struct Hits(Arc<AtomicUsize>);

    impl Hits {
        fn get(&self) -> usize {
            self.0.load(Ordering::Relaxed)
        }
    }

    /// Spawn a stub oracle speaking the real `nostr-wot-oracle` wire format:
    /// `GET /distance?from=&to=&max_hops=` answering 200 with a `hops` field.
    /// `behaviour` maps the target pubkey hex to the response it should get.
    async fn spawn_oracle(
        behaviour: impl Fn(&str) -> StubResponse + Clone + Send + Sync + 'static,
    ) -> (String, Hits) {
        let hits = Hits::default();
        let counter = hits.clone();

        let app = Router::new().route(
            "/distance",
            get(move |Query(params): Query<HashMap<String, String>>| {
                let behaviour = behaviour.clone();
                let counter = counter.clone();
                async move {
                    counter.0.fetch_add(1, Ordering::Relaxed);
                    let to = params.get("to").cloned().unwrap_or_default();
                    match behaviour(&to) {
                        StubResponse::Hops(h) => (
                            StatusCode::OK,
                            format!(
                                "{{\"from\":\"a\",\"to\":\"b\",\"hops\":{h},\
                                     \"path_count\":1,\"mutual_follow\":false}}"
                            ),
                        ),
                        StubResponse::Null => (
                            StatusCode::OK,
                            "{\"from\":\"a\",\"to\":\"b\",\"hops\":null,\
                                 \"path_count\":0,\"mutual_follow\":false}"
                                .to_string(),
                        ),
                        StubResponse::NotFound => {
                            (StatusCode::NOT_FOUND, "<html>nope</html>".to_string())
                        }
                        StubResponse::ServerError => {
                            (StatusCode::INTERNAL_SERVER_ERROR, "boom".to_string())
                        }
                    }
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        (format!("http://{addr}"), hits)
    }

    #[derive(Clone, Copy)]
    enum StubResponse {
        Hops(u32),
        Null,
        NotFound,
        ServerError,
    }

    /// Config for the oracle path. `local: false` explicitly: local mode is the
    /// default now, and these tests are about the HTTP client.
    fn config(oracle_url: String) -> WotConfig {
        WotConfig {
            oracle_url,
            fallback_oracle_url: None,
            max_hops: 2,
            local: false,
            timeout: Duration::from_secs(2),
            ..WotConfig::default()
        }
    }

    #[tokio::test]
    async fn admits_within_max_hops_and_refuses_beyond() {
        let near = Keys::generate().public_key();
        let far = Keys::generate().public_key();
        let near_hex = near.to_hex();

        let (url, _) = spawn_oracle(move |to| {
            if to == near_hex {
                StubResponse::Hops(2)
            } else {
                StubResponse::Hops(5)
            }
        })
        .await;

        let root = Keys::generate().public_key();
        let oracle = WotOracle::new(config(url), vec![root]).unwrap();

        assert_eq!(oracle.resolve(&near).await, Verdict::reachable(2));
        assert!(oracle.admits(&near));

        assert_eq!(oracle.resolve(&far).await, Verdict::UNREACHABLE);
        assert!(!oracle.admits(&far));
    }

    /// Regression guard for the failure mode that the public
    /// `wot-oracle.mappingbitcoin.com` host actually exhibits: it serves a web
    /// page and 404s every API path. If 404 were read as "unreachable" the
    /// relay would refuse every single key while reporting a healthy oracle.
    #[tokio::test]
    async fn a_404_is_a_broken_endpoint_not_a_verdict() {
        let (url, _) = spawn_oracle(|_| StubResponse::NotFound).await;
        let oracle = WotOracle::new(config(url), vec![Keys::generate().public_key()]).unwrap();

        for _ in 0..FAILURE_THRESHOLD {
            assert_eq!(
                oracle.resolve(&Keys::generate().public_key()).await,
                Verdict::UNREACHABLE
            );
        }
        assert!(
            oracle.is_degraded(),
            "a host that 404s the API must surface as degraded, not as an empty WoT"
        );
    }

    /// The real oracle reports "no path" as `hops: null` on a 200.
    #[tokio::test]
    async fn null_hops_is_a_verdict_and_keeps_the_oracle_healthy() {
        let (url, _) = spawn_oracle(|_| StubResponse::Null).await;
        let oracle = WotOracle::new(config(url), vec![Keys::generate().public_key()]).unwrap();

        for _ in 0..FAILURE_THRESHOLD + 2 {
            assert_eq!(
                oracle.resolve(&Keys::generate().public_key()).await,
                Verdict::UNREACHABLE
            );
        }
        assert!(!oracle.is_degraded());
    }

    #[tokio::test]
    async fn null_distance_means_unreachable() {
        let (url, _) = spawn_oracle(|_| StubResponse::Null).await;
        let oracle = WotOracle::new(config(url), vec![Keys::generate().public_key()]).unwrap();

        assert_eq!(
            oracle.resolve(&Keys::generate().public_key()).await,
            Verdict::UNREACHABLE
        );
        assert!(!oracle.is_degraded());
    }

    #[tokio::test]
    async fn positive_verdicts_are_cached() {
        let (url, hits) = spawn_oracle(|_| StubResponse::Hops(1)).await;
        let oracle = WotOracle::new(config(url), vec![Keys::generate().public_key()]).unwrap();

        let target = Keys::generate().public_key();
        oracle.resolve(&target).await;
        oracle.resolve(&target).await;
        oracle.resolve(&target).await;

        assert_eq!(hits.get(), 1, "cached verdict should not re-query");
    }

    #[tokio::test]
    async fn expired_verdicts_are_re_queried() {
        let (url, hits) = spawn_oracle(|_| StubResponse::Hops(1)).await;
        let mut cfg = config(url);
        cfg.cache_ttl = Duration::from_millis(50);
        let oracle = WotOracle::new(cfg, vec![Keys::generate().public_key()]).unwrap();

        let target = Keys::generate().public_key();
        oracle.resolve(&target).await;
        assert!(oracle.admits(&target));

        tokio::time::sleep(Duration::from_millis(80)).await;

        assert!(!oracle.admits(&target), "expired entry must not admit");
        oracle.resolve(&target).await;
        assert_eq!(hits.get(), 2);
    }

    #[tokio::test]
    async fn negative_verdicts_expire_sooner_than_positive_ones() {
        let (url, _) = spawn_oracle(|_| StubResponse::Hops(9)).await;
        let mut cfg = config(url);
        cfg.cache_ttl = Duration::from_secs(3600);
        cfg.negative_cache_ttl = Duration::from_millis(50);
        let oracle = WotOracle::new(cfg, vec![Keys::generate().public_key()]).unwrap();

        let target = Keys::generate().public_key();
        assert_eq!(oracle.resolve(&target).await, Verdict::UNREACHABLE);
        assert!(oracle.cached(&target).is_some());

        tokio::time::sleep(Duration::from_millis(80)).await;
        assert!(
            oracle.cached(&target).is_none(),
            "a refusal should lapse quickly so a newly-followed key gets in"
        );
    }

    #[tokio::test]
    async fn failures_fail_closed_and_are_not_cached() {
        let (url, hits) = spawn_oracle(|_| StubResponse::ServerError).await;
        let oracle = WotOracle::new(config(url), vec![Keys::generate().public_key()]).unwrap();

        let target = Keys::generate().public_key();
        assert_eq!(oracle.resolve(&target).await, Verdict::UNREACHABLE);
        assert!(!oracle.admits(&target));
        assert!(
            oracle.cached(&target).is_none(),
            "an outage must not be cached as a refusal"
        );
        assert_eq!(hits.get(), 1);
    }

    #[tokio::test]
    async fn breaker_opens_after_repeated_failures() {
        let (url, hits) = spawn_oracle(|_| StubResponse::ServerError).await;
        let oracle = WotOracle::new(config(url), vec![Keys::generate().public_key()]).unwrap();

        for _ in 0..FAILURE_THRESHOLD {
            oracle.resolve(&Keys::generate().public_key()).await;
        }
        assert!(oracle.is_degraded());

        let before = hits.get();
        for _ in 0..5 {
            assert_eq!(
                oracle.resolve(&Keys::generate().public_key()).await,
                Verdict::UNREACHABLE
            );
        }
        assert_eq!(
            hits.get(),
            before,
            "an open breaker should not reach the oracle at all"
        );
    }

    #[tokio::test]
    async fn falls_back_to_the_secondary_oracle() {
        let (dead_url, dead_hits) = spawn_oracle(|_| StubResponse::ServerError).await;
        let (live_url, live_hits) = spawn_oracle(|_| StubResponse::Hops(1)).await;

        let mut cfg = config(dead_url);
        cfg.fallback_oracle_url = Some(live_url);
        let oracle = WotOracle::new(cfg, vec![Keys::generate().public_key()]).unwrap();

        let target = Keys::generate().public_key();
        assert_eq!(oracle.resolve(&target).await, Verdict::reachable(1));
        assert_eq!(dead_hits.get(), 1);
        assert_eq!(live_hits.get(), 1);
        // The fallback answered, so this is not a failure.
        assert!(!oracle.is_degraded());
    }

    #[tokio::test]
    async fn takes_the_shortest_distance_across_roots() {
        let (url, _) = spawn_oracle(|_| StubResponse::Hops(2)).await;
        // Three roots all answering 2; the key is its own root in none of them.
        let roots = vec![
            Keys::generate().public_key(),
            Keys::generate().public_key(),
            Keys::generate().public_key(),
        ];
        let oracle = WotOracle::new(config(url), roots).unwrap();

        assert_eq!(
            oracle.resolve(&Keys::generate().public_key()).await,
            Verdict::reachable(2)
        );
    }

    /// Local mode must decide from the graph and never touch the network.
    #[tokio::test]
    async fn local_mode_answers_from_the_graph_without_http() {
        let (url, hits) = spawn_oracle(|_| StubResponse::Hops(1)).await;
        let root = Keys::generate().public_key();
        let friend = Keys::generate().public_key();
        let stranger = Keys::generate().public_key();

        let mut cfg = config(url);
        cfg.local = true;
        let oracle = WotOracle::new(cfg, vec![root]).unwrap();

        let graph = oracle.graph().expect("local mode builds a graph").clone();
        graph.replace(
            std::collections::HashMap::from([(root, std::collections::HashSet::from([friend]))]),
            crate::wot_graph::Coverage::default(),
        );

        assert_eq!(oracle.resolve(&friend).await, Verdict::reachable(1));
        assert!(oracle.admits(&friend));
        assert_eq!(oracle.resolve(&stranger).await, Verdict::UNREACHABLE);
        assert!(!oracle.admits(&stranger));

        assert_eq!(hits.get(), 0, "local mode must not call the oracle");
        assert!(oracle.is_local());
        // Nothing can be "degraded" when there is no remote dependency.
        assert!(!oracle.is_degraded());
    }

    /// A root is admitted immediately, before the graph has been built --
    /// otherwise the operator's own key is locked out during the first rebuild.
    #[tokio::test]
    async fn local_mode_admits_a_root_before_the_graph_is_built() {
        let (url, _) = spawn_oracle(|_| StubResponse::Hops(1)).await;
        let root = Keys::generate().public_key();

        let mut cfg = config(url);
        cfg.local = true;
        let oracle = WotOracle::new(cfg, vec![root]).unwrap();

        assert!(!oracle.graph().unwrap().is_built());
        assert_eq!(oracle.resolve(&root).await, Verdict::reachable(0));
        assert!(
            oracle.admits(&root),
            "a positive answer is cacheable even mid-build"
        );
    }

    /// A refusal from a half-built graph must not stick: the missing edge may
    /// simply not have been fetched yet.
    #[tokio::test]
    async fn local_mode_does_not_cache_refusals_before_the_graph_is_built() {
        let (url, _) = spawn_oracle(|_| StubResponse::Hops(1)).await;
        let mut cfg = config(url);
        cfg.local = true;
        let oracle = WotOracle::new(cfg, vec![Keys::generate().public_key()]).unwrap();

        let target = Keys::generate().public_key();
        assert_eq!(oracle.resolve(&target).await, Verdict::UNREACHABLE);
        assert!(
            oracle.cached(&target).is_none(),
            "an unbuilt graph must not produce a sticky refusal"
        );
    }

    #[tokio::test]
    async fn a_root_is_admitted_without_querying() {
        let (url, hits) = spawn_oracle(|_| StubResponse::ServerError).await;
        let root = Keys::generate().public_key();
        let oracle = WotOracle::new(config(url), vec![root]).unwrap();

        assert_eq!(oracle.resolve(&root).await, Verdict::reachable(0));
        assert_eq!(hits.get(), 0);
    }

    #[tokio::test]
    async fn no_roots_admits_nobody() {
        let (url, hits) = spawn_oracle(|_| StubResponse::Hops(1)).await;
        let oracle = WotOracle::new(config(url), vec![]).unwrap();

        assert_eq!(
            oracle.resolve(&Keys::generate().public_key()).await,
            Verdict::UNREACHABLE
        );
        assert_eq!(hits.get(), 0);
    }

    #[tokio::test]
    async fn changing_roots_clears_stale_verdicts() {
        let (url, _) = spawn_oracle(|_| StubResponse::Hops(1)).await;
        let oracle = WotOracle::new(config(url), vec![Keys::generate().public_key()]).unwrap();

        let target = Keys::generate().public_key();
        oracle.resolve(&target).await;
        assert!(oracle.admits(&target));

        oracle.set_roots(vec![Keys::generate().public_key()]);
        assert!(
            !oracle.admits(&target),
            "verdicts measured against old roots must not survive"
        );
    }

    #[tokio::test]
    async fn admitted_lists_only_live_in_range_entries() {
        let (url, _) = spawn_oracle(|_| StubResponse::Hops(1)).await;
        let oracle = WotOracle::new(config(url), vec![Keys::generate().public_key()]).unwrap();

        let admitted = Keys::generate().public_key();
        oracle.resolve(&admitted).await;

        let listed = oracle.admitted();
        assert_eq!(listed, vec![(admitted, 1)]);
    }
}
