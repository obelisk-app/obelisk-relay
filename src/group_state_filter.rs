//! Makes NIP-29 group discovery index-backed.
//!
//! nostr-lmdb keeps six indexes: `(created_at, id)`, `(author, created_at)`,
//! `(tag, created_at)`, `(author, kind, created_at)`, `(author, tag,
//! created_at)` and `(kind, tag, created_at)`. Every one of them is keyed on an
//! author or a tag, so a filter naming only `kinds` matches none of them and
//! falls through to `query_by_scraping`, which the storage layer itself
//! documents as "INEFFICIENT as it scans through many events". The cost is how
//! far that scan runs, not how much it matches — so the *rarer* a kind is, the
//! slower it is to ask for, and a kind with no events at all is the worst case
//! of all.
//!
//! That is precisely the query every NIP-29 client opens a session with:
//! `{"kinds":[39000]}`, to find out which groups exist. Group state is the
//! rarest thing in the database, which puts the channel list on the slowest
//! path there is. Measured against public.obelisk.ar holding 5.2GB:
//!
//! ```text
//! {"kinds":[39000,39001,39002]}                  62 events   29.3s
//! {"kinds":[39000,39001,39002],"authors":[...]}  62 events    0.45s
//! ```
//!
//! Identical results, 66x apart, because supplying an author moves the query
//! onto `(author, kind, created_at)`.
//!
//! A client cannot do this for itself. Discovery means it does not yet know any
//! group id, so it has no `d` or `h` tag to offer; and it cannot know which keys
//! signed the group state it is looking for, because a relay that rotates its
//! identity leaves the events signed by the old key in place. This relay
//! rotated on 2026-08-11 and four distinct pubkeys have signed live group state
//! as a result. Only the relay knows that set, so the relay supplies it.
//!
//! The rewrite is deliberately narrow — it applies only when a filter names
//! *nothing but* addressable group-state kinds, so it can add an author set
//! without changing which events match. Anything else is passed through
//! untouched.

use std::collections::BTreeSet;
use std::sync::Arc;

use nostr_sdk::prelude::*;
use parking_lot::RwLock;
use relay_builder::nostr_middleware::{InboundContext, NostrMiddleware};
use tracing::{debug, info, warn};

use crate::groups::{Groups, ADDRESSABLE_EVENT_KINDS};

/// The pubkeys that have ever signed group state on this relay.
///
/// Starts out holding just the relay's current key — correct but incomplete —
/// and is replaced once the startup scan finishes. Until then filters are
/// passed through unrewritten rather than rewritten against a partial set:
/// a slow answer is recoverable, a silently incomplete channel list is not.
#[derive(Debug, Clone)]
pub struct GroupStateAuthors {
    authors: Arc<RwLock<Option<BTreeSet<PublicKey>>>>,
}

impl GroupStateAuthors {
    pub fn new() -> Self {
        Self {
            authors: Arc::new(RwLock::new(None)),
        }
    }

    fn get(&self) -> Option<BTreeSet<PublicKey>> {
        self.authors.read().clone()
    }

    fn set(&self, authors: BTreeSet<PublicKey>) {
        *self.authors.write() = Some(authors);
    }

    /// Scan for every pubkey that has signed group state, then publish the set.
    ///
    /// This is one full pass over the addressable kinds, which on a large
    /// database is the very query this module exists to avoid — so it runs once,
    /// detached, and the relay serves (slowly) until it lands. `relay_pubkey` is
    /// always included even if the scan finds nothing signed by it yet, because
    /// every group created from now on will be.
    pub fn spawn_refresh(&self, groups: Arc<Groups>, relay_pubkey: PublicKey) {
        let slot = self.clone();
        tokio::spawn(async move {
            let started = std::time::Instant::now();
            match groups.group_state_authors().await {
                Ok(mut found) => {
                    found.insert(relay_pubkey);
                    info!(
                        "Group discovery: {} author(s) signed group state, found in {:?}; \
                         kinds-only queries for {:?} are now index-backed",
                        found.len(),
                        started.elapsed(),
                        ADDRESSABLE_EVENT_KINDS,
                    );
                    slot.set(found);
                }
                Err(e) => {
                    // Leaving the set unpublished keeps the relay correct and
                    // slow, which is the right way to fail here.
                    warn!("Group discovery: author scan failed, queries stay unindexed: {e}");
                }
            }
        });
    }
}

impl Default for GroupStateAuthors {
    fn default() -> Self {
        Self::new()
    }
}

/// True when adding an author set cannot change which events match.
///
/// Requires that the filter names kinds, that every one of them is an
/// addressable group-state kind, and that it offers no other selector. A filter
/// carrying ids, authors or tags is either already index-backed or means
/// something we must not alter.
fn is_bare_group_state_query(filter: &Filter) -> bool {
    let Some(kinds) = &filter.kinds else {
        return false;
    };

    !kinds.is_empty()
        && kinds.iter().all(|k| ADDRESSABLE_EVENT_KINDS.contains(k))
        && filter.ids.is_none()
        && filter.authors.is_none()
        && filter.generic_tags.is_empty()
}

#[derive(Debug, Clone)]
pub struct GroupStateFilterMiddleware {
    authors: GroupStateAuthors,
}

impl GroupStateFilterMiddleware {
    pub fn new(authors: GroupStateAuthors) -> Self {
        Self { authors }
    }
}

impl NostrMiddleware<()> for GroupStateFilterMiddleware {
    async fn process_inbound<Next>(
        &self,
        mut ctx: InboundContext<'_, (), Next>,
    ) -> Result<(), anyhow::Error>
    where
        Next: relay_builder::nostr_middleware::InboundProcessor<()>,
    {
        let Some(ClientMessage::Req { filters, .. }) = &mut ctx.message else {
            return ctx.next().await;
        };

        if !filters.iter().any(|f| is_bare_group_state_query(f)) {
            return ctx.next().await;
        }

        // Nothing to add until the scan has published a complete set.
        let Some(authors) = self.authors.get() else {
            return ctx.next().await;
        };

        let mut rewritten = 0usize;
        for filter in filters.iter_mut() {
            if is_bare_group_state_query(filter) {
                // to_mut clones only if this filter is still borrowed.
                filter.to_mut().authors = Some(authors.clone());
                rewritten += 1;
            }
        }

        debug!(
            "[{}] Group discovery: scoped {} filter(s) to {} known author(s)",
            ctx.connection_id,
            rewritten,
            authors.len(),
        );

        ctx.next().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare(kinds: Vec<Kind>) -> Filter {
        Filter::new().kinds(kinds)
    }

    #[test]
    fn rewrites_a_bare_group_state_query() {
        assert!(is_bare_group_state_query(&bare(vec![Kind::Custom(39000)])));
        assert!(is_bare_group_state_query(&bare(vec![
            Kind::Custom(39000),
            Kind::Custom(39001),
            Kind::Custom(39002),
            Kind::Custom(39003),
        ])));
    }

    #[test]
    fn leaves_filters_that_already_have_an_index_alone() {
        let with_author = bare(vec![Kind::Custom(39000)]).author(Keys::generate().public_key());
        assert!(!is_bare_group_state_query(&with_author));

        let with_tag = bare(vec![Kind::Custom(39000)])
            .custom_tag(SingleLetterTag::lowercase(Alphabet::D), "some-group");
        assert!(!is_bare_group_state_query(&with_tag));

        let with_id = bare(vec![Kind::Custom(39000)]).id(EventId::all_zeros());
        assert!(!is_bare_group_state_query(&with_id));
    }

    #[test]
    fn leaves_non_group_state_kinds_alone() {
        // Chat. Clients send these with an h tag, which is already indexed.
        assert!(!is_bare_group_state_query(&bare(vec![Kind::Custom(9)])));
        // Gift wraps.
        assert!(!is_bare_group_state_query(&bare(vec![Kind::Custom(1059)])));
    }

    #[test]
    fn refuses_a_mixed_filter_it_cannot_safely_narrow() {
        // 9 is authored by members, not by the relay: scoping this to the
        // group-state authors would silently drop every chat message.
        assert!(!is_bare_group_state_query(&bare(vec![
            Kind::Custom(39000),
            Kind::Custom(9),
        ])));
    }

    #[test]
    fn ignores_a_filter_with_no_kinds() {
        assert!(!is_bare_group_state_query(&Filter::new()));
        assert!(!is_bare_group_state_query(&bare(vec![])));
    }

    #[test]
    fn publishes_nothing_until_the_scan_lands() {
        let slot = GroupStateAuthors::new();
        assert!(
            slot.get().is_none(),
            "must not rewrite against a partial set"
        );

        let pk = Keys::generate().public_key();
        slot.set(BTreeSet::from([pk]));
        assert_eq!(slot.get().unwrap().len(), 1);
    }
}
