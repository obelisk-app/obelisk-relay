//! Background event pruner.
//!
//! Periodically deletes events older than `retention` for a configurable set of kinds.
//! Designed for a public relay where chat-like content (kinds 9/11/12) is ephemeral
//! and should not accumulate forever, while NIP-29 group state events (9000-series and
//! 39000-series) are kept indefinitely so groups don't get destroyed.

use nostr_sdk::prelude::*;
use relay_builder::RelayDatabase;
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
}

#[derive(Clone)]
pub struct PrunerConfig {
    pub retention: Duration,
    pub interval: Duration,
    pub kinds: Vec<Kind>,
}

impl PrunerConfig {
    pub fn from_settings(
        retention: Duration,
        interval: Option<Duration>,
        kinds: Option<Vec<u16>>,
    ) -> Self {
        let interval = interval.unwrap_or_else(|| {
            // Default: scan ~48 times across the retention window, clamped 60s..6h.
            let secs = (retention.as_secs() / 48).clamp(60, 6 * 3600);
            Duration::from_secs(secs)
        });
        let raw_kinds = kinds.unwrap_or_else(|| DEFAULT_PRUNE_KINDS.to_vec());
        let (allowed, denied): (Vec<u16>, Vec<u16>) = raw_kinds
            .into_iter()
            .partition(|k| !NEVER_PRUNE_KINDS.contains(k));
        if !denied.is_empty() {
            warn!(
                "Pruner: refusing to prune protected NIP-29 management/state kinds {:?}; \
                 these are required for group identity and will never be deleted.",
                denied
            );
        }
        let kinds: Vec<Kind> = allowed.into_iter().map(Kind::from).collect();
        Self {
            retention,
            interval,
            kinds,
        }
    }

    pub fn kinds_as_u16(&self) -> Vec<u16> {
        self.kinds.iter().map(|k| k.as_u16()).collect()
    }
}

pub fn spawn(
    database: Arc<RelayDatabase>,
    config: PrunerConfig,
    stats: Arc<PrunerStats>,
    cancel: CancellationToken,
) {
    info!(
        "Event pruner enabled: retention={:?}, interval={:?}, kinds={:?}",
        config.retention,
        config.interval,
        config.kinds_as_u16()
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
    let cutoff_secs = match std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs().saturating_sub(config.retention.as_secs()))
    {
        Ok(s) => s,
        Err(e) => {
            warn!("Pruner: clock error: {e}");
            return;
        }
    };
    let cutoff = Timestamp::from(cutoff_secs);

    let scopes = match database.list_scopes().await {
        Ok(s) => s,
        Err(e) => {
            error!("Pruner: list_scopes failed: {e}");
            return;
        }
    };

    let mut deleted_total: u64 = 0;
    for scope in &scopes {
        let filter = Filter::new()
            .kinds(config.kinds.iter().copied())
            .until(cutoff);

        // Count first so we can report deletion volume; .count() and .delete() are
        // both bounded by the same filter, so the count is a tight upper bound.
        let count = match database.count(vec![filter.clone()], scope).await {
            Ok(n) => n as u64,
            Err(e) => {
                warn!("Pruner: count failed for scope {:?}: {e}", scope);
                0
            }
        };

        if count == 0 {
            continue;
        }

        match database.delete(filter, scope).await {
            Ok(()) => {
                deleted_total = deleted_total.saturating_add(count);
            }
            Err(e) => {
                error!("Pruner: delete failed for scope {:?}: {e}", scope);
            }
        }
    }

    stats
        .total_pruned
        .fetch_add(deleted_total, Ordering::Relaxed);
    stats.runs.fetch_add(1, Ordering::Relaxed);
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    stats.last_run_unix.store(now_secs, Ordering::Relaxed);

    if deleted_total > 0 {
        info!(
            "Pruner run complete: deleted {} events older than {:?} across {} scopes",
            deleted_total,
            config.retention,
            scopes.len()
        );
    }
}
