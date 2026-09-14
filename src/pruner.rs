//! Background event pruner.
//!
//! Periodically deletes events older than a per-kind retention window.
//! Designed for a public relay where chat-like content (kinds 9/11/12) is ephemeral
//! and should not accumulate forever, while NIP-29 group state events (9000-series and
//! 39000-series) are kept indefinitely so groups don't get destroyed.

use nostr_sdk::prelude::*;
use parking_lot::RwLock;
use relay_builder::RelayDatabase;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// Default kinds to prune when none are configured: NIP-29 chat, threads, replies.
pub const DEFAULT_PRUNE_KINDS: &[u16] = &[9, 11, 12];

/// Kinds that the pruner refuses to delete under any configuration. Group state
/// (NIP-29 management 9000-9009 + replaceable 39000-39003) lives in the relay's
/// LMDB and reconstitutes group identity, membership, roles, and metadata; if
/// any of these are pruned, groups silently disappear or lose their admins.
/// Misconfigured `prune_kinds` entries that overlap this set are dropped at
/// startup with a warning — defense in depth.
pub const NEVER_PRUNE_KINDS: &[u16] = &[
    9000, 9001, 9002, 9003, 9004, 9005, 9006, 9007, 9008, 9009, 9010, 9011, 39000, 39001, 39002,
    39003,
];

#[derive(Debug, Default)]
pub struct PrunerStats {
    /// Cumulative events deleted since process start (across all runs).
    pub total_pruned: AtomicU64,
    /// Unix seconds of the last completed prune run, or 0 if never.
    pub last_run_unix: AtomicI64,
    /// Number of completed prune runs.
    pub runs: AtomicU64,
    /// Deleted counts keyed by kind, so the UI can attribute deletions to the
    /// policy that caused them rather than showing one opaque total.
    pub per_kind: RwLock<BTreeMap<u16, u64>>,
}

impl PrunerStats {
    pub fn per_kind_snapshot(&self) -> BTreeMap<u16, u64> {
        self.per_kind.read().clone()
    }
}

#[derive(Clone)]
pub struct PrunerConfig {
    /// Retention window per kind. A kind absent from this map is never pruned.
    ///
    /// Replaces the previous single `retention` + `kinds` pair: a relay storing
    /// both ephemeral game moves and long-lived chat cannot express both with one
    /// window, and forcing a shared window means either keeping throwaway events
    /// for a year or deleting conversations after a week.
    pub policies: BTreeMap<Kind, Duration>,
    pub interval: Duration,
}

impl PrunerConfig {
    /// Build from explicit per-kind policies.
    ///
    /// Protected kinds are dropped here with a warning, so a hand-edited config
    /// cannot arm a policy against group state.
    pub fn from_policies(
        policies: BTreeMap<u16, Duration>,
        interval: Option<Duration>,
    ) -> Option<Self> {
        let (allowed, denied): (Vec<_>, Vec<_>) = policies
            .into_iter()
            .partition(|(k, _)| !NEVER_PRUNE_KINDS.contains(k));

        if !denied.is_empty() {
            warn!(
                "Pruner: refusing to prune protected NIP-29 management/state kinds {:?}; \
                 these are required for group identity and will never be deleted.",
                denied.iter().map(|(k, _)| *k).collect::<Vec<_>>()
            );
        }

        // A zero window would mean "delete everything immediately"; treat it as
        // unset rather than as a catastrophic instruction.
        let policies: BTreeMap<Kind, Duration> = allowed
            .into_iter()
            .filter(|(_, d)| d.as_secs() > 0)
            .map(|(k, d)| (Kind::from(k), d))
            .collect();

        if policies.is_empty() {
            return None;
        }

        // Cadence follows the SHORTEST window: a 7-day policy alongside a 1-year
        // one must still be enforced with 7-day granularity.
        let interval = interval.unwrap_or_else(|| {
            let shortest = policies
                .values()
                .map(|d| d.as_secs())
                .min()
                .unwrap_or(u64::MAX);
            Duration::from_secs((shortest / 48).clamp(60, 6 * 3600))
        });

        Some(Self { policies, interval })
    }

    /// Build from the pre-per-kind config shape: one retention applied to a list
    /// of kinds. Kept so existing `event_retention` + `prune_kinds` deployments
    /// behave identically without a config edit.
    pub fn from_legacy_settings(
        retention: Duration,
        interval: Option<Duration>,
        kinds: Option<Vec<u16>>,
    ) -> Option<Self> {
        let kinds = kinds.unwrap_or_else(|| DEFAULT_PRUNE_KINDS.to_vec());
        let policies = kinds.into_iter().map(|k| (k, retention)).collect();
        Self::from_policies(policies, interval)
    }

    pub fn kinds_as_u16(&self) -> Vec<u16> {
        self.policies.keys().map(|k| k.as_u16()).collect()
    }

    /// Policies grouped by window, so a run issues one filter per distinct
    /// duration rather than one per kind.
    fn by_window(&self) -> BTreeMap<u64, Vec<Kind>> {
        let mut grouped: BTreeMap<u64, Vec<Kind>> = BTreeMap::new();
        for (kind, window) in &self.policies {
            grouped.entry(window.as_secs()).or_default().push(*kind);
        }
        grouped
    }

    /// Retention as `{kind: seconds}` for API responses.
    pub fn policies_as_secs(&self) -> BTreeMap<u16, u64> {
        self.policies
            .iter()
            .map(|(k, d)| (k.as_u16(), d.as_secs()))
            .collect()
    }
}

pub fn spawn(
    database: Arc<RelayDatabase>,
    config: PrunerConfig,
    stats: Arc<PrunerStats>,
    cancel: CancellationToken,
) {
    info!(
        "Event pruner enabled: interval={:?}, policies={:?}",
        config.interval,
        config.policies_as_secs()
    );

    tokio::spawn(async move {
        // Stagger the first run a bit so startup isn't slammed.
        let first_delay = std::cmp::min(config.interval, Duration::from_secs(60));
        tokio::select! {
            _ = tokio::time::sleep(first_delay) => {}
            _ = cancel.cancelled() => return,
        }

        let mut ticker = tokio::time::interval(config.interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await; // consume the immediate first tick

        loop {
            run_once(&database, &config, &stats).await;
            tokio::select! {
                _ = ticker.tick() => {}
                _ = cancel.cancelled() => {
                    info!("Pruner shutting down");
                    return;
                }
            }
        }
    });
}

async fn run_once(database: &RelayDatabase, config: &PrunerConfig, stats: &PrunerStats) {
    let now_secs = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_secs(),
        Err(e) => {
            warn!("Pruner: clock error: {e}");
            return;
        }
    };

    let scopes = match database.list_scopes().await {
        Ok(s) => s,
        Err(e) => {
            error!("Pruner: list_scopes failed: {e}");
            return;
        }
    };

    // Kinds sharing a window are deleted together, so the number of filters is
    // the number of distinct retention values -- not the number of kinds.
    let windows = config.by_window();
    let mut deleted_total: u64 = 0;
    let mut deleted_by_kind: BTreeMap<u16, u64> = BTreeMap::new();

    for (window_secs, kinds) in &windows {
        let cutoff = Timestamp::from(now_secs.saturating_sub(*window_secs));

        for scope in &scopes {
            // Counted per kind rather than per window: attributing deletions to a
            // specific policy is the whole point of having separate policies, and
            // a lumped total cannot be split after the fact.
            for kind in kinds {
                let filter = Filter::new().kind(*kind).until(cutoff);

                // Count first so we can report volume; .count() and .delete() share
                // the same filter, so the count is a tight upper bound.
                let count = match database.count(vec![filter.clone()], scope).await {
                    Ok(n) => n as u64,
                    Err(e) => {
                        warn!("Pruner: count failed for scope {:?}: {e}", scope);
                        continue;
                    }
                };

                if count == 0 {
                    continue;
                }

                match database.delete(filter, scope).await {
                    Ok(()) => {
                        deleted_total = deleted_total.saturating_add(count);
                        *deleted_by_kind.entry(kind.as_u16()).or_insert(0) += count;
                    }
                    Err(e) => {
                        error!("Pruner: delete failed for scope {:?}: {e}", scope);
                    }
                }
            }
        }
    }

    stats
        .total_pruned
        .fetch_add(deleted_total, Ordering::Relaxed);
    stats.runs.fetch_add(1, Ordering::Relaxed);
    if !deleted_by_kind.is_empty() {
        let mut per_kind = stats.per_kind.write();
        for (kind, count) in &deleted_by_kind {
            *per_kind.entry(*kind).or_insert(0) += count;
        }
    }
    stats
        .last_run_unix
        .store(now_secs as i64, Ordering::Relaxed);

    if deleted_total > 0 {
        info!(
            "Pruner run complete: deleted {} events across {} scopes ({} policies); by kind: {:?}",
            deleted_total,
            scopes.len(),
            config.policies.len(),
            deleted_by_kind
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn days(n: u64) -> Duration {
        Duration::from_secs(n * 86_400)
    }

    #[test]
    fn protected_kinds_cannot_be_armed() {
        let policies = [(9u16, days(30)), (9007, days(1)), (39000, days(1))]
            .into_iter()
            .collect();
        let cfg = PrunerConfig::from_policies(policies, None).expect("policy survives");
        assert_eq!(cfg.kinds_as_u16(), vec![9], "only kind 9 is prunable");
    }

    #[test]
    fn a_policy_set_of_only_protected_kinds_disables_the_pruner() {
        let policies = [(9007u16, days(1)), (39001, days(1))].into_iter().collect();
        assert!(
            PrunerConfig::from_policies(policies, None).is_none(),
            "nothing prunable means no pruner at all"
        );
    }

    #[test]
    fn zero_windows_are_treated_as_unset_not_as_delete_everything() {
        let policies = [(9u16, Duration::ZERO), (11, days(7))]
            .into_iter()
            .collect();
        let cfg = PrunerConfig::from_policies(policies, None).expect("kind 11 survives");
        assert_eq!(cfg.kinds_as_u16(), vec![11]);
    }

    #[test]
    fn legacy_settings_apply_one_window_to_every_listed_kind() {
        let cfg = PrunerConfig::from_legacy_settings(days(30), None, Some(vec![9, 11, 12]))
            .expect("legacy config still arms the pruner");
        let policies = cfg.policies_as_secs();
        assert_eq!(policies.len(), 3);
        for kind in [9u16, 11, 12] {
            assert_eq!(
                policies.get(&kind).copied(),
                Some(days(30).as_secs()),
                "kind {kind} keeps the single legacy window"
            );
        }
    }

    #[test]
    fn legacy_settings_without_kinds_fall_back_to_the_chat_defaults() {
        let cfg = PrunerConfig::from_legacy_settings(days(30), None, None).expect("arms");
        assert_eq!(cfg.kinds_as_u16(), DEFAULT_PRUNE_KINDS.to_vec());
    }

    #[test]
    fn kinds_sharing_a_window_are_grouped_into_one_filter() {
        let policies = [
            (9u16, days(365)),
            (11, days(365)),
            (1059, days(30)),
            (2390, days(7)),
        ]
        .into_iter()
        .collect();
        let cfg = PrunerConfig::from_policies(policies, None).expect("arms");
        let windows = cfg.by_window();

        assert_eq!(windows.len(), 3, "three distinct windows, not four kinds");
        assert_eq!(
            windows.get(&days(365).as_secs()).map(Vec::len),
            Some(2),
            "the two year-long kinds share a filter"
        );
    }

    #[test]
    fn cadence_follows_the_shortest_window() {
        let policies = [(9u16, days(365)), (2390, days(7))].into_iter().collect();
        let cfg = PrunerConfig::from_policies(policies, None).expect("arms");

        // 7 days / 48, not 365 days / 48 -- a long policy must not slow down
        // enforcement of a short one.
        assert_eq!(cfg.interval.as_secs(), (days(7).as_secs() / 48).max(60));
    }

    #[test]
    fn an_explicit_interval_is_respected() {
        let policies = [(9u16, days(30))].into_iter().collect();
        let cfg =
            PrunerConfig::from_policies(policies, Some(Duration::from_secs(900))).expect("arms");
        assert_eq!(cfg.interval.as_secs(), 900);
    }
}
