//! Reports subscriptions that the storage layer cannot serve from an index.
//!
//! The channel list on public.obelisk.ar spent weeks answering in 29 seconds,
//! and nothing in the relay said so. There was no error, no warning and no
//! metric — the query succeeded, slowly, and the only signal was a user
//! reporting that the app hung on "Loading channels…". Diagnosing it meant
//! measuring by hand from outside.
//!
//! The relay can see this coming, because the condition is decidable from the
//! filter alone. nostr-lmdb picks its access path in `QueryFilterPattern`, and
//! every index it has — `(author, kind, created_at)`, `(kind, tag,
//! created_at)`, `(author, created_at)`, `(tag, created_at)` — is keyed on an
//! author or a tag. A filter offering neither, and not naming explicit ids,
//! falls through to `query_by_scraping`, which walks the event table. `kinds`
//! alone does not help: there is no kind-only index.
//!
//! So this mirrors that one condition and makes the fallback visible — a
//! counter to graph and alert on, and a throttled log line naming the kinds
//! responsible. It deliberately does not block or rewrite anything: a scraping
//! query is legal, is cheap on a small database, and refusing it would break
//! clients. The goal is that the *next* time one appears it shows up as a
//! number on a dashboard rather than as weeks of silent degradation.
//!
//! See `crate::group_state_filter` for the rewrite that removed the one query
//! this relay could not otherwise avoid.

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use nostr_sdk::prelude::*;
use relay_builder::nostr_middleware::{InboundContext, NostrMiddleware};
use tracing::warn;

/// Minimum gap between warnings, so one busy client cannot flood the log.
/// The counter is incremented on every occurrence regardless.
const WARN_EVERY_SECS: i64 = 300;

static LAST_WARNED_UNIX: AtomicI64 = AtomicI64::new(0);

/// True when the storage layer will have to scan rather than seek.
///
/// Mirrors `QueryFilterPattern::from_filter`: anything with ids, authors or
/// tags selects an index; everything else scrapes. Kept as a free function so
/// the rule is testable on its own and reads as the single fact it encodes.
pub fn would_scrape(filter: &Filter) -> bool {
    filter.ids.is_none() && filter.authors.is_none() && filter.generic_tags.is_empty()
}

fn describe(filter: &Filter) -> String {
    match &filter.kinds {
        Some(kinds) if !kinds.is_empty() => {
            let mut list: Vec<String> = kinds.iter().map(|k| k.as_u16().to_string()).collect();
            list.sort();
            format!("kinds=[{}]", list.join(","))
        }
        // No kinds either: this asks the relay to walk everything it stores.
        _ => "no kinds, no authors, no tags — full table".to_string(),
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Default)]
pub struct UnindexedQueryMiddleware;

impl NostrMiddleware<()> for UnindexedQueryMiddleware {
    async fn process_inbound<Next>(
        &self,
        ctx: InboundContext<'_, (), Next>,
    ) -> Result<(), anyhow::Error>
    where
        Next: relay_builder::nostr_middleware::InboundProcessor<()>,
    {
        let Some(ClientMessage::Req { filters, .. }) = &ctx.message else {
            return ctx.next().await;
        };

        let offenders: Vec<String> = filters
            .iter()
            .filter(|f| would_scrape(f))
            .map(|f| describe(f))
            .collect();

        if !offenders.is_empty() {
            crate::metrics::unindexed_queries().increment(offenders.len() as u64);

            // Throttled: the metric is the signal, the log line is the detail
            // you want once while working out which client is responsible.
            let now = now_unix();
            let last = LAST_WARNED_UNIX.load(Ordering::Relaxed);
            if now - last >= WARN_EVERY_SECS
                && LAST_WARNED_UNIX
                    .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                    .is_ok()
            {
                warn!(
                    "[{}] Unindexed REQ: {} — no author, tag or id, so the storage layer \
                     must scan. Cost grows with the database, not with the result size. \
                     Counted as unindexed_queries.",
                    ctx.connection_id,
                    offenders.join("; "),
                );
            }
        }

        ctx.next().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kinds_only_filter_scrapes() {
        // The exact shape that made the channel list take 29 seconds.
        assert!(would_scrape(&Filter::new().kind(Kind::Custom(39000))));
        // And the worst case: nothing to narrow on at all.
        assert!(would_scrape(&Filter::new()));
        assert!(would_scrape(&Filter::new().limit(100)));
    }

    #[test]
    fn an_author_or_tag_selects_an_index() {
        let pk = Keys::generate().public_key();
        assert!(!would_scrape(&Filter::new().author(pk)));
        assert!(!would_scrape(
            &Filter::new().kind(Kind::Custom(39000)).author(pk)
        ));
        assert!(!would_scrape(
            &Filter::new()
                .kind(Kind::Custom(9))
                .custom_tag(SingleLetterTag::lowercase(Alphabet::H), "group-id")
        ));
    }

    #[test]
    fn explicit_ids_are_a_direct_lookup() {
        assert!(!would_scrape(&Filter::new().id(EventId::all_zeros())));
    }

    #[test]
    fn a_time_window_alone_does_not_make_a_query_indexed() {
        // since/until narrow the scan but do not select an index, so this is
        // still a scrape and should still be reported.
        let f = Filter::new()
            .kind(Kind::Custom(1059))
            .since(Timestamp::from(1_700_000_000))
            .until(Timestamp::from(1_800_000_000));
        assert!(would_scrape(&f));
    }
}
