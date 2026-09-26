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
//! responsible.
//!
//! It also *rations* them, which it did not originally do. The reasoning for
//! reporting-only still holds and is preserved: a scraping query is legal, is
//! cheap on a small database, and refusing it outright would break clients. But
//! "legal and sometimes cheap" is not the same as "unlimited". At 29.3s of
//! single-query cost, a client that issues them in a loop is the cheapest
//! denial-of-service this relay has — it needs no special privilege beyond being
//! admitted, and every request is individually legitimate.
//!
//! So the rule is a per-connection budget rather than a ban: a burst is fine,
//! sustained scraping is not. Over budget, the subscription is closed with a
//! reason the client can act on, and the connection keeps working for everything
//! else. A client doing something reasonable will never see it.
//!
//! Two kinds of REQ are deliberately never charged:
//!
//! - **Ephemeral-only REQs** (every filter asks only for kinds 20000-29999).
//!   Nothing of those kinds is ever stored, so the historical query can only
//!   return nothing — yet with no kind index it would still walk the whole
//!   event table to prove it. These are live feeds (mesh-voice presence 20078,
//!   voice signalling 25050, NIP-46 24133) that clients re-issue on every
//!   reconnect, and charging them closed voice calls' subscriptions with
//!   `rate-limited`. Exempting them is only safe because the scan is removed
//!   too: each filter is rewritten to `limit: 0`, which the storage layer
//!   answers without reading a row. The rule is *every* filter, because
//!   relay_builder applies the smallest limit in a REQ to all of its filters,
//!   so a `limit: 0` on one filter would silently empty the others.
//! - **REQs from connections admission will refuse.** `verify_filters` answers
//!   those with `auth-required`, after this middleware; charging them meant a
//!   client that subscribed before finishing NIP-42 spent budget on REQs that
//!   never ran, and found it gone when it retried after AUTH.
//!
//! See `crate::group_state_filter` for the rewrite that removed the one query
//! this relay could not otherwise avoid.

use std::num::NonZeroU32;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use governor::clock::DefaultClock;
use governor::state::keyed::DefaultKeyedStateStore;
use governor::{Quota, RateLimiter};
use nostr_sdk::prelude::*;
use relay_builder::nostr_middleware::{InboundContext, NostrMiddleware};
use tracing::warn;

use crate::whitelist::Whitelist;

/// Minimum gap between warnings, so one busy client cannot flood the log.
/// The counter is incremented on every occurrence regardless.
const WARN_EVERY_SECS: i64 = 300;

static LAST_WARNED_UNIX: AtomicI64 = AtomicI64::new(0);

/// Sustained budget, per connection, for queries the storage layer must scan for.
///
/// Sized from what honest use looks like rather than from what the database can
/// survive: a client legitimately issues a few of these when it opens (and this
/// relay's own worst offender, group discovery, is now index-backed and does not
/// count at all). A client that needs more than one every six seconds, forever,
/// is not browsing.
const SCRAPE_QUERIES_PER_MINUTE: u32 = 10;

/// Allowance for the opening burst, so a normal client never trips the limit.
const SCRAPE_BURST: u32 = 20;

const SCRAPE_BUDGET_EXCEEDED: &str =
    "rate-limited: too many unindexed queries. Narrow the filter with an author, \
     an id or a tag, or slow down.";

type ScrapeLimiter = RateLimiter<String, DefaultKeyedStateStore<String>, DefaultClock>;

/// True when the storage layer will have to scan rather than seek.
///
/// Mirrors `QueryFilterPattern::from_filter`: anything with ids, authors or
/// tags selects an index; everything else scrapes. Kept as a free function so
/// the rule is testable on its own and reads as the single fact it encodes.
pub fn would_scrape(filter: &Filter) -> bool {
    filter.ids.is_none() && filter.authors.is_none() && filter.generic_tags.is_empty()
}

/// True when the filter can only ever match ephemeral events.
///
/// A filter with no `kinds` matches everything, so it is never ephemeral-only.
pub fn is_ephemeral_only(filter: &Filter) -> bool {
    filter
        .kinds
        .as_ref()
        .is_some_and(|kinds| !kinds.is_empty() && kinds.iter().all(|k| k.is_ephemeral()))
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

#[derive(Clone)]
pub struct UnindexedQueryMiddleware {
    limiter: Arc<ScrapeLimiter>,
    /// Admission, so a REQ that `verify_filters` is about to refuse is not
    /// charged. The same rule the processor applies — see [`Whitelist::admits`].
    whitelist: Whitelist,
    relay_pubkey: PublicKey,
    admin_pubkeys: Arc<Vec<PublicKey>>,
}

impl std::fmt::Debug for UnindexedQueryMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnindexedQueryMiddleware").finish()
    }
}

impl UnindexedQueryMiddleware {
    pub fn new(
        whitelist: Whitelist,
        relay_pubkey: PublicKey,
        admin_pubkeys: Vec<PublicKey>,
    ) -> Self {
        let per_minute =
            NonZeroU32::new(SCRAPE_QUERIES_PER_MINUTE).expect("scrape budget must be non-zero");
        let burst = NonZeroU32::new(SCRAPE_BURST).expect("scrape burst must be non-zero");
        Self {
            limiter: Arc::new(RateLimiter::keyed(
                Quota::per_minute(per_minute).allow_burst(burst),
            )),
            whitelist,
            relay_pubkey,
            admin_pubkeys: Arc::new(admin_pubkeys),
        }
    }

    fn admits(&self, pubkey: Option<&PublicKey>) -> bool {
        self.whitelist.admits(pubkey, |pk| {
            *pk == self.relay_pubkey || self.admin_pubkeys.contains(pk)
        })
    }

    /// Whether this connection may run another scraping query right now.
    ///
    /// Keyed on connection rather than pubkey so it applies before and after
    /// AUTH alike, and so one client cannot spread the cost across sockets any
    /// cheaper than the connection limits already allow.
    fn allow(&self, connection_id: &str) -> bool {
        // Bounded: governor keeps one small cell per live key, and keys are
        // dropped once they have been idle long enough to be irrelevant.
        self.limiter.retain_recent();
        self.limiter.check_key(&connection_id.to_string()).is_ok()
    }
}

impl NostrMiddleware<()> for UnindexedQueryMiddleware {
    async fn process_inbound<Next>(
        &self,
        mut ctx: InboundContext<'_, (), Next>,
    ) -> Result<(), anyhow::Error>
    where
        Next: relay_builder::nostr_middleware::InboundProcessor<()>,
    {
        let Some(ClientMessage::Req {
            filters,
            subscription_id,
        }) = &mut ctx.message
        else {
            return ctx.next().await;
        };

        if !filters.is_empty() && filters.iter().all(|f| is_ephemeral_only(f)) {
            for filter in filters.iter_mut() {
                filter.to_mut().limit = Some(0);
            }
            return ctx.next().await;
        }

        let offenders: Vec<String> = filters
            .iter()
            .filter(|f| would_scrape(f))
            .map(|f| describe(f))
            .collect();

        if !offenders.is_empty() {
            crate::metrics::unindexed_queries().increment(offenders.len() as u64);

            let authed_pubkey = ctx.state.read().await.authed_pubkey;
            if !self.admits(authed_pubkey.as_ref()) {
                // Admission will refuse this REQ; do not bill for it.
                return ctx.next().await;
            }

            if !self.allow(ctx.connection_id) {
                let subscription_id = subscription_id.as_ref().clone();
                warn!(
                    "[{}] Refusing unindexed REQ over budget: {}",
                    ctx.connection_id,
                    offenders.join("; "),
                );
                // Close this subscription only. The connection stays usable, and
                // the reason names the fix so a client author can act on it.
                ctx.send_message(RelayMessage::closed(
                    subscription_id,
                    SCRAPE_BUDGET_EXCEEDED,
                ))?;
                return Ok(());
            }

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
    use crate::blacklist::Blacklist;

    fn open_relay_mw() -> UnindexedQueryMiddleware {
        UnindexedQueryMiddleware::new(
            Whitelist::new(vec![], None, Blacklist::new(None)),
            Keys::generate().public_key(),
            vec![],
        )
    }

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
    fn a_connection_may_burst_then_is_throttled() {
        let mw = open_relay_mw();

        // The opening burst a normal client is entitled to.
        for i in 0..SCRAPE_BURST {
            assert!(
                mw.allow("conn-a"),
                "scrape {i} of the burst allowance should be permitted"
            );
        }

        // Past it, without waiting for replenishment.
        assert!(
            !mw.allow("conn-a"),
            "a connection past its budget must be refused"
        );
    }

    #[test]
    fn the_budget_is_per_connection() {
        let mw = open_relay_mw();
        for _ in 0..SCRAPE_BURST {
            assert!(mw.allow("noisy"));
        }
        assert!(!mw.allow("noisy"));

        // One client exhausting its allowance must not throttle anybody else --
        // that would turn a rate limit into a denial-of-service of its own.
        assert!(
            mw.allow("quiet"),
            "an unrelated connection keeps its own budget"
        );
    }

    #[test]
    fn an_indexed_query_never_consumes_budget() {
        // Only filters that would actually scrape reach the limiter; this is the
        // predicate that decides, so pin it against the query shapes the app
        // relies on most.
        let pk = Keys::generate().public_key();
        assert!(!would_scrape(
            &Filter::new()
                .kind(Kind::Custom(9))
                .custom_tag(SingleLetterTag::lowercase(Alphabet::H), "group-id")
        ));
        assert!(!would_scrape(&Filter::new().author(pk)));
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

    #[test]
    fn live_feeds_of_ephemeral_kinds_are_ephemeral_only() {
        // Mesh-voice presence, voice signalling, NIP-46: the REQs this exists for.
        for kind in [20078u16, 25050, 24133] {
            assert!(is_ephemeral_only(&Filter::new().kind(Kind::from(kind))));
        }
        assert!(is_ephemeral_only(
            &Filter::new()
                .kind(Kind::from(25050u16))
                .since(Timestamp::from(1_700_000_000))
        ));
    }

    #[test]
    fn a_stored_kind_or_no_kinds_is_not_ephemeral_only() {
        // One stored kind is enough for the query to have history to scan.
        assert!(!is_ephemeral_only(
            &Filter::new().kinds(vec![Kind::from(20078u16), Kind::from(1u16)])
        ));
        // No kinds means every kind.
        assert!(!is_ephemeral_only(&Filter::new()));
        assert!(!is_ephemeral_only(&Filter::new().kinds(Vec::<Kind>::new())));
        // Replaceable (10000-19999) and addressable (30000+) are stored.
        assert!(!is_ephemeral_only(
            &Filter::new().kind(Kind::from(10002u16))
        ));
        assert!(!is_ephemeral_only(
            &Filter::new().kind(Kind::from(30000u16))
        ));
    }

    #[test]
    fn admission_matches_the_processor_rule() {
        let member = Keys::generate().public_key();
        let admin = Keys::generate().public_key();
        let relay = Keys::generate().public_key();
        let mw = UnindexedQueryMiddleware::new(
            Whitelist::new(vec![member], None, Blacklist::new(None)),
            relay,
            vec![admin],
        );

        // Unauthenticated on a closed relay: verify_filters will answer
        // auth-required, so the budget must not be charged for it.
        assert!(!mw.admits(None));
        assert!(!mw.admits(Some(&Keys::generate().public_key())));

        assert!(mw.admits(Some(&member)));
        assert!(mw.admits(Some(&admin)));
        assert!(mw.admits(Some(&relay)));

        // An open relay admits everyone, authenticated or not.
        assert!(open_relay_mw().admits(None));
    }
}
