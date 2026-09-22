//! A follow graph built from data this relay can reach, so Web-of-Trust
//! admission needs no external oracle.
//!
//! # Why local
//!
//! The oracle sidecar is a second service to run, keep alive and trust, and
//! when it is down the tier admits nobody. But a relay already holds kind-3
//! contact lists, and `follow_sync` already fetches them for the reference
//! accounts. The same BFS the oracle performs is a few hundred lines here,
//! against data we have.
//!
//! # What it can and cannot see
//!
//! An edge exists only where a kind-3 event is available. Sources, in order of
//! preference:
//!
//! 1. Contact lists stored on this relay.
//! 2. Contact lists fetched from public relays for the roots and, to reach two
//!    hops, for the accounts those roots follow.
//!
//! A pubkey whose contact list is nowhere reachable looks like it follows
//! nobody. That makes the graph *incomplete rather than wrong*: it can fail to
//! find a path that exists, never invent one. Since a missing path means "not
//! admitted", incompleteness is the safe direction.

use nostr_lmdb::Scope;
use nostr_sdk::prelude::*;
use parking_lot::RwLock;
use relay_builder::RelayDatabase;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Relays to pull contact lists from when this one does not hold them.
const FOLLOW_RELAYS: &[&str] = &[
    "wss://relay.damus.io",
    "wss://nos.lol",
    "wss://purplepag.es",
];

/// Default cap on contact lists fetched per rebuild.
///
/// Sizing matters more than it looks. To answer "is X within N hops" the graph
/// needs the contact list of every account at hops 0..N-1. One root following
/// 500 people needs ~500 lists to cover two hops — comfortable. Covering three
/// needs the lists of everyone *those* 500 follow, which is tens of thousands.
/// Past the budget the outermost hop is sampled rather than known, so a refusal
/// there means "no path was fetched", not "no path exists".
pub const DEFAULT_MAX_REMOTE_FETCHES: usize = 25_000;

/// How many pubkeys to ask for in a single `REQ`.
const FETCH_BATCH: usize = 200;

/// Page size ceiling for `admitted_page`. The full admitted set runs to six
/// figures, so the console asks for a window rather than the phone book.
pub const ADMITTED_PAGE_MAX: usize = 500;

/// How completely each hop was covered on the last rebuild.
#[derive(Debug, Clone, Default)]
pub struct Coverage {
    /// Per hop: `(contact lists needed, contact lists known)`.
    pub hops: Vec<(usize, usize)>,
    /// The fetch budget ran out, so the outermost hop is incomplete and some
    /// refusals are "unknown" rather than "not connected".
    pub truncated: bool,
    /// Deepest hop the graph can answer for with confidence.
    pub complete_to_hop: u8,
    /// How many accounts the graph would admit at the configured hop limit.
    ///
    /// The number an operator actually wants. "Admitted so far" only ever
    /// counts keys that have connected and been resolved, so on a quiet relay
    /// it stays near zero while tens of thousands of accounts are reachable --
    /// which reads as "the tier is not working".
    pub reachable_total: usize,
}

/// Directed follow edges: who follows whom.
#[derive(Debug, Default)]
pub struct FollowGraph {
    edges: RwLock<HashMap<PublicKey, HashSet<PublicKey>>>,
    built_at: RwLock<Option<Instant>>,
    coverage: RwLock<Coverage>,
    /// Everyone the graph admits, with their exact hop count, sorted
    /// nearest-first.
    ///
    /// `reachable()` already computes this on every rebuild and it used to be
    /// thrown away -- only the length survived, plus a 500-entry sample -- which
    /// meant the console could show a number for the web-of-trust tiers but
    /// never a list. Keeping it costs roughly 33 bytes per entry (~5MB at 150k
    /// accounts) against a relay whose resident set is already near 1GB, and it
    /// is what lets the tier screens page and search without re-running a
    /// breadth-first search over millions of edges on every UI click.
    ///
    /// Sorted once here rather than per request, so paging is a slice.
    admitted: RwLock<Vec<(PublicKey, u8)>>,
}

impl FollowGraph {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Shortest hop count from any root to `target`, or `None` beyond
    /// `max_hops`.
    ///
    /// Breadth-first from every root at once, so the first time the target is
    /// reached is by definition the shortest path. A root is zero hops from
    /// itself.
    pub fn distance(&self, roots: &[PublicKey], target: &PublicKey, max_hops: u8) -> Option<u8> {
        if roots.contains(target) {
            return Some(0);
        }

        let edges = self.edges.read();
        let mut seen: HashSet<PublicKey> = roots.iter().copied().collect();
        let mut frontier: VecDeque<PublicKey> = roots.iter().copied().collect();

        for hop in 1..=max_hops {
            let mut next = VecDeque::new();
            while let Some(node) = frontier.pop_front() {
                let Some(follows) = edges.get(&node) else {
                    continue;
                };
                for followed in follows {
                    if followed == target {
                        return Some(hop);
                    }
                    // `seen` prevents revisiting, which also bounds the walk on
                    // a graph full of mutual follows.
                    if seen.insert(*followed) {
                        next.push_back(*followed);
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        None
    }

    /// Everyone reachable within `max_hops`, with their distance. Used to
    /// report coverage, not on the admission path.
    pub fn reachable(&self, roots: &[PublicKey], max_hops: u8) -> HashMap<PublicKey, u8> {
        let edges = self.edges.read();
        let mut distances: HashMap<PublicKey, u8> = roots.iter().map(|r| (*r, 0)).collect();
        let mut frontier: VecDeque<PublicKey> = roots.iter().copied().collect();

        for hop in 1..=max_hops {
            let mut next = VecDeque::new();
            while let Some(node) = frontier.pop_front() {
                let Some(follows) = edges.get(&node) else {
                    continue;
                };
                for followed in follows {
                    if !distances.contains_key(followed) {
                        distances.insert(*followed, hop);
                        next.push_back(*followed);
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        distances
    }

    pub fn replace(&self, edges: HashMap<PublicKey, HashSet<PublicKey>>, coverage: Coverage) {
        *self.edges.write() = edges;
        *self.coverage.write() = coverage;
        *self.built_at.write() = Some(Instant::now());
    }

    pub fn coverage(&self) -> Coverage {
        self.coverage.read().clone()
    }

    /// How many accounts the graph admits, and how many sit at each hop.
    ///
    /// Returned together because a caller showing tier counts needs both, and
    /// should not take the lock twice for an answer that has to be consistent.
    pub fn admitted_totals(&self) -> (usize, Vec<(u8, usize)>) {
        let admitted = self.admitted.read();
        let mut per_hop: Vec<(u8, usize)> = Vec::new();
        for (_, hops) in admitted.iter() {
            match per_hop.iter_mut().find(|(h, _)| h == hops) {
                Some((_, count)) => *count += 1,
                None => per_hop.push((*hops, 1)),
            }
        }
        per_hop.sort_by_key(|(h, _)| *h);
        (admitted.len(), per_hop)
    }

    /// One page of the accounts admitted at exactly `hops`.
    ///
    /// `matches` filters on the hex or npub encoding *before* paging, so a
    /// search narrows the whole result set rather than just the visible window.
    /// Filtering after paging would mean "find this key" only ever looked at
    /// the first 500 rows.
    ///
    /// Returns `(total at this hop after filtering, the page)`.
    pub fn admitted_page(
        &self,
        hops: u8,
        offset: usize,
        limit: usize,
        matches: Option<&str>,
    ) -> (usize, Vec<(PublicKey, u8)>) {
        let admitted = self.admitted.read();
        let needle = matches
            .map(|m| m.trim().to_ascii_lowercase())
            .filter(|m| !m.is_empty());

        let filtered: Vec<(PublicKey, u8)> = admitted
            .iter()
            .filter(|(_, h)| *h == hops)
            .filter(|(pk, _)| match &needle {
                None => true,
                Some(n) => {
                    pk.to_hex().contains(n)
                        || pk
                            .to_bech32()
                            .map(|npub: String| npub.to_ascii_lowercase().contains(n))
                            .unwrap_or(false)
                }
            })
            .copied()
            .collect();

        let total = filtered.len();
        let page = filtered
            .into_iter()
            .skip(offset)
            .take(limit.min(ADMITTED_PAGE_MAX))
            .collect();
        (total, page)
    }

    /// Nearest-first preview across *all* hops, for the "who is in" card.
    ///
    /// Distinct from `admitted_page`, which is scoped to one hop because the
    /// tier screens each show a single tier. This one deliberately mixes hops:
    /// the card's job is "here are the closest accounts the graph admits".
    pub fn admitted_preview(&self, limit: usize) -> Vec<(PublicKey, u8)> {
        self.admitted
            .read()
            .iter()
            .take(limit.min(ADMITTED_PAGE_MAX))
            .copied()
            .collect()
    }

    pub fn set_admitted(&self, mut admitted: Vec<(PublicKey, u8)>) {
        // Nearest-first, so one hop's accounts are a contiguous slice and the
        // closest are what an operator sees first.
        admitted.sort_by_key(|(_, hops)| *hops);
        *self.admitted.write() = admitted;
    }

    /// `(accounts with a known follow list, total edges)`.
    pub fn size(&self) -> (usize, usize) {
        let edges = self.edges.read();
        (edges.len(), edges.values().map(|f| f.len()).sum())
    }

    pub fn is_built(&self) -> bool {
        self.built_at.read().is_some()
    }

    pub fn age(&self) -> Option<Duration> {
        self.built_at.read().map(|t| t.elapsed())
    }
}

/// Pull every contact list this relay already stores.
///
/// Newest wins per author: kind 3 is replaceable, and a scope can still hold an
/// older copy.
async fn edges_from_database(db: &Arc<RelayDatabase>) -> HashMap<PublicKey, HashSet<PublicKey>> {
    let mut edges: HashMap<PublicKey, HashSet<PublicKey>> = HashMap::new();
    let mut newest: HashMap<PublicKey, Timestamp> = HashMap::new();

    let scopes = match db.list_scopes().await {
        Ok(scopes) => scopes,
        Err(e) => {
            warn!(target: "wot", "Could not list scopes for the follow graph: {e}");
            vec![Scope::Default]
        }
    };

    for scope in &scopes {
        let events = match db
            .query(vec![Filter::new().kind(Kind::ContactList)], scope)
            .await
        {
            Ok(events) => events,
            Err(e) => {
                warn!(target: "wot", "Contact-list query failed: {e}");
                continue;
            }
        };

        for event in events {
            if newest
                .get(&event.pubkey)
                .is_some_and(|t| *t >= event.created_at)
            {
                continue;
            }
            newest.insert(event.pubkey, event.created_at);
            edges.insert(event.pubkey, follows_in(&event));
        }
    }

    debug!(target: "wot", "Follow graph: {} contact lists from local storage", edges.len());
    edges
}

/// The `p` tags of a contact list.
fn follows_in(event: &Event) -> HashSet<PublicKey> {
    event
        .tags
        .iter()
        .filter(|tag| tag.kind() == TagKind::p())
        .filter_map(|tag| tag.content())
        .filter_map(|hex| PublicKey::from_hex(hex).ok())
        .collect()
}

/// Fetch contact lists for `wanted` from public relays, in batches.
async fn fetch_contact_lists(wanted: Vec<PublicKey>) -> HashMap<PublicKey, HashSet<PublicKey>> {
    let mut found = HashMap::new();
    if wanted.is_empty() {
        return found;
    }

    // The relay builder may already have installed a provider; this client is
    // independent, so make sure one exists before opening WSS connections.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let client = Client::default();
    for relay in FOLLOW_RELAYS {
        if let Err(e) = client.add_relay(*relay).await {
            warn!(target: "wot", "Follow-graph relay {relay} unavailable: {e}");
        }
    }
    client.connect().await;
    tokio::time::sleep(Duration::from_secs(2)).await;

    let mut newest: HashMap<PublicKey, Timestamp> = HashMap::new();
    for batch in wanted.chunks(FETCH_BATCH) {
        let filter = Filter::new()
            .kind(Kind::ContactList)
            .authors(batch.to_vec());

        match tokio::time::timeout(
            Duration::from_secs(20),
            client.fetch_events(filter, Duration::from_secs(12)),
        )
        .await
        {
            Ok(Ok(events)) => {
                for event in events.iter() {
                    if newest
                        .get(&event.pubkey)
                        .is_some_and(|t| *t >= event.created_at)
                    {
                        continue;
                    }
                    newest.insert(event.pubkey, event.created_at);
                    found.insert(event.pubkey, follows_in(event));
                }
            }
            Ok(Err(e)) => warn!(target: "wot", "Contact-list fetch failed: {e}"),
            Err(_) => warn!(target: "wot", "Contact-list fetch timed out"),
        }
    }

    let _ = client.disconnect().await;
    found
}

/// Rebuild the graph: local storage first, then fetch what is missing to reach
/// `max_hops` from the roots.
///
/// Only the lists actually needed are fetched — the roots, then the accounts
/// they follow, and so on outward — rather than crawling the network.
pub async fn rebuild(
    graph: &Arc<FollowGraph>,
    db: &Arc<RelayDatabase>,
    roots: &[PublicKey],
    max_hops: u8,
    max_fetches: usize,
) {
    if roots.is_empty() {
        info!(target: "wot", "Follow graph not rebuilt: no roots configured");
        graph.replace(HashMap::new(), Coverage::default());
        return;
    }

    let started = Instant::now();
    let mut edges = edges_from_database(db).await;
    let from_db = edges.len();
    let mut budget = max_fetches;
    let mut coverage = Coverage {
        complete_to_hop: max_hops,
        ..Coverage::default()
    };

    // Walk outward one hop at a time, fetching only the lists that would
    // extend the frontier. The last hop needs no lists of its own: we only
    // need to know that its members are reachable, not who they follow.
    let mut frontier: Vec<PublicKey> = roots.to_vec();
    let mut known: HashSet<PublicKey> = roots.iter().copied().collect();

    for hop in 0..max_hops {
        let needed: Vec<PublicKey> = frontier
            .iter()
            .filter(|pk| !edges.contains_key(pk))
            .copied()
            .collect();
        let missing: Vec<PublicKey> = needed.iter().take(budget).copied().collect();

        // Record what this hop would have needed against what it got. A hop
        // that could not be fully fetched cannot answer "not connected" -- only
        // "no path was fetched" -- and the operator has to be able to see that.
        let known_at_hop = frontier.len() - needed.len() + missing.len();
        coverage.hops.push((frontier.len(), known_at_hop));
        if missing.len() < needed.len() {
            coverage.truncated = true;
            coverage.complete_to_hop = coverage.complete_to_hop.min(hop);
        }

        if !missing.is_empty() {
            budget = budget.saturating_sub(missing.len());
            let fetched = fetch_contact_lists(missing).await;
            debug!(
                target: "wot",
                "Follow graph hop {hop}: fetched {} contact list(s)",
                fetched.len()
            );
            edges.extend(fetched);
        }

        let mut next = Vec::new();
        for node in &frontier {
            if let Some(follows) = edges.get(node) {
                for followed in follows {
                    if known.insert(*followed) {
                        next.push(*followed);
                    }
                }
            }
        }
        if next.is_empty() || budget == 0 {
            break;
        }
        frontier = next;
    }

    let accounts = edges.len();
    let total_edges: usize = edges.values().map(|f| f.len()).sum();
    let truncated = coverage.truncated;
    let complete_to = coverage.complete_to_hop;

    // Compute the admitted set once, here, rather than on every settings-page
    // load: it is a BFS over hundreds of thousands of edges.
    graph.replace(edges, coverage.clone());
    let reachable = graph.reachable(roots, max_hops);
    let reachable_total = reachable.len();

    // Kept in full rather than truncated to 500: this is what the tier screens
    // page and search over, and recomputing it per request would put a
    // breadth-first search across millions of edges behind a UI click.
    graph.set_admitted(reachable.into_iter().collect());

    coverage.reachable_total = reachable_total;
    *graph.coverage.write() = coverage;

    info!(
        target: "wot",
        "Follow graph rebuilt in {:?}: {accounts} contact lists ({from_db} local, \
         {} fetched), {total_edges} follow edges, {reachable_total} accounts \
         admitted within {max_hops} hop(s)",
        started.elapsed(),
        accounts.saturating_sub(from_db),
    );

    if truncated {
        warn!(
            target: "wot",
            "Follow graph is complete only to {complete_to} hop(s): the {max_fetches} \
             contact-list budget ran out before hop {}. Beyond {complete_to} hops a \
             refusal means the path was never fetched, not that it does not exist -- \
             lower relay.wot.max_hops to {complete_to}, add roots, or raise the budget.",
            complete_to + 1,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everyone the graph admits, bucketed and paged.
    ///
    /// These exist because the admitted set used to be truncated to 500 for
    /// display, so the web-of-trust tiers could show a count but never a list.
    #[test]
    fn admitted_totals_bucket_by_hop() {
        let graph = FollowGraph::new();
        let k = keys(6);
        graph.set_admitted(vec![
            (k[0], 1),
            (k[1], 2),
            (k[2], 2),
            (k[3], 3),
            (k[4], 3),
            (k[5], 3),
        ]);

        let (total, per_hop) = graph.admitted_totals();
        assert_eq!(total, 6);
        assert_eq!(per_hop, vec![(1, 1), (2, 2), (3, 3)]);
    }

    #[test]
    fn a_page_is_scoped_to_one_hop() {
        let graph = FollowGraph::new();
        let k = keys(5);
        graph.set_admitted(vec![(k[0], 1), (k[1], 2), (k[2], 2), (k[3], 3), (k[4], 3)]);

        let (total, page) = graph.admitted_page(2, 0, 50, None);
        assert_eq!(total, 2, "only hop-2 accounts count toward a hop-2 page");
        assert!(page.iter().all(|(_, h)| *h == 2));
    }

    #[test]
    fn paging_walks_the_whole_hop_without_gaps_or_repeats() {
        let graph = FollowGraph::new();
        let k = keys(25);
        graph.set_admitted(k.iter().map(|pk| (*pk, 2)).collect());

        let mut seen = Vec::new();
        let mut offset = 0;
        loop {
            let (total, page) = graph.admitted_page(2, offset, 10, None);
            assert_eq!(total, 25);
            if page.is_empty() {
                break;
            }
            offset += page.len();
            seen.extend(page.into_iter().map(|(pk, _)| pk));
        }

        assert_eq!(seen.len(), 25, "every account appears exactly once");
        let unique: std::collections::HashSet<_> = seen.iter().collect();
        assert_eq!(unique.len(), 25);
    }

    #[test]
    fn a_page_cannot_exceed_the_ceiling() {
        let graph = FollowGraph::new();
        graph.set_admitted(keys(900).into_iter().map(|pk| (pk, 2)).collect());

        // A client asking for everything must not be able to make the relay
        // serialise six figures of keys into one response.
        let (total, page) = graph.admitted_page(2, 0, 100_000, None);
        assert_eq!(total, 900);
        assert_eq!(page.len(), ADMITTED_PAGE_MAX);
    }

    #[test]
    fn search_filters_before_paging() {
        // The point of filtering server-side: "find this key" has to look at
        // the whole tier, not just the rows that happen to be on screen.
        let graph = FollowGraph::new();
        let mut admitted: Vec<(PublicKey, u8)> = keys(600).into_iter().map(|pk| (pk, 2)).collect();
        let needle = Keys::generate().public_key();
        admitted.push((needle, 2));
        graph.set_admitted(admitted);

        let (total, page) = graph.admitted_page(2, 0, 50, Some(&needle.to_hex()));
        assert_eq!(total, 1, "the filter applies to the tier, not the page");
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].0, needle);
    }

    #[test]
    fn search_matches_npub_as_well_as_hex() {
        let graph = FollowGraph::new();
        let target = Keys::generate().public_key();
        graph.set_admitted(vec![(target, 2)]);

        let npub: String = target.to_bech32().unwrap();
        let (total, _) = graph.admitted_page(2, 0, 10, Some(&npub));
        assert_eq!(total, 1, "operators paste npubs, not hex");

        // Case and whitespace are what a paste actually carries.
        let (padded, _) =
            graph.admitted_page(2, 0, 10, Some(&format!("  {}  ", npub.to_uppercase())));
        assert_eq!(padded, 1);
    }

    #[test]
    fn an_empty_search_is_not_a_filter() {
        let graph = FollowGraph::new();
        graph.set_admitted(keys(3).into_iter().map(|pk| (pk, 2)).collect());
        assert_eq!(graph.admitted_page(2, 0, 10, Some("   ")).0, 3);
    }

    #[test]
    fn the_preview_mixes_hops_nearest_first() {
        let graph = FollowGraph::new();
        let k = keys(4);
        // Deliberately out of order going in.
        graph.set_admitted(vec![(k[0], 3), (k[1], 1), (k[2], 2), (k[3], 3)]);

        let preview = graph.admitted_preview(3);
        assert_eq!(
            preview.iter().map(|(_, h)| *h).collect::<Vec<_>>(),
            vec![1, 2, 3],
            "the card shows the closest accounts first, across every hop"
        );
    }

    fn keys(n: usize) -> Vec<PublicKey> {
        (0..n).map(|_| Keys::generate().public_key()).collect()
    }

    /// Build a graph from explicit edges, skipping all I/O.
    fn graph_of(pairs: &[(PublicKey, Vec<PublicKey>)]) -> Arc<FollowGraph> {
        let graph = FollowGraph::new();
        graph.replace(
            pairs
                .iter()
                .map(|(from, to)| (*from, to.iter().copied().collect()))
                .collect(),
            Coverage::default(),
        );
        graph
    }

    #[test]
    fn a_root_is_zero_hops_from_itself() {
        let k = keys(1);
        let graph = graph_of(&[]);
        assert_eq!(graph.distance(&k, &k[0], 2), Some(0));
    }

    #[test]
    fn counts_hops_along_the_follow_chain() {
        let k = keys(4);
        // root -> a -> b -> c
        let graph = graph_of(&[(k[0], vec![k[1]]), (k[1], vec![k[2]]), (k[2], vec![k[3]])]);
        let roots = vec![k[0]];

        assert_eq!(graph.distance(&roots, &k[1], 3), Some(1));
        assert_eq!(graph.distance(&roots, &k[2], 3), Some(2));
        assert_eq!(graph.distance(&roots, &k[3], 3), Some(3));
    }

    #[test]
    fn refuses_anything_past_max_hops() {
        let k = keys(4);
        let graph = graph_of(&[(k[0], vec![k[1]]), (k[1], vec![k[2]]), (k[2], vec![k[3]])]);
        let roots = vec![k[0]];

        assert_eq!(graph.distance(&roots, &k[2], 2), Some(2));
        assert_eq!(
            graph.distance(&roots, &k[3], 2),
            None,
            "three hops away must not be admitted at max_hops 2"
        );
    }

    /// Breadth-first from every root at once, so the answer is the shortest
    /// path from the nearest root rather than whichever root was listed first.
    #[test]
    fn takes_the_shortest_path_across_roots() {
        let k = keys(4);
        let graph = graph_of(&[
            (k[0], vec![k[2]]), // far root: two hops to the target
            (k[2], vec![k[3]]),
            (k[1], vec![k[3]]), // near root: one hop
        ]);

        assert_eq!(graph.distance(&[k[0], k[1]], &k[3], 3), Some(1));
        assert_eq!(graph.distance(&[k[0]], &k[3], 3), Some(2));
    }

    #[test]
    fn a_follow_cycle_terminates() {
        let k = keys(3);
        let graph = graph_of(&[
            (k[0], vec![k[1]]),
            (k[1], vec![k[0], k[2]]),
            (k[2], vec![k[0], k[1]]),
        ]);
        assert_eq!(graph.distance(&[k[0]], &k[2], 5), Some(2));
        // An unrelated key is simply unreachable; the walk must still finish.
        assert_eq!(graph.distance(&[k[0]], &keys(1)[0], 5), None);
    }

    /// A pubkey whose contact list is unavailable follows nobody as far as the
    /// graph is concerned. That must not admit them, and must not crash.
    #[test]
    fn an_unknown_account_has_no_outgoing_edges() {
        let k = keys(3);
        let graph = graph_of(&[(k[0], vec![k[1]])]);
        assert_eq!(graph.distance(&[k[0]], &k[1], 3), Some(1));
        assert_eq!(graph.distance(&[k[0]], &k[2], 3), None);
    }

    #[test]
    fn no_roots_reaches_nobody() {
        let k = keys(2);
        let graph = graph_of(&[(k[0], vec![k[1]])]);
        assert_eq!(graph.distance(&[], &k[1], 3), None);
    }

    #[test]
    fn reports_its_size_and_coverage() {
        let k = keys(3);
        let graph = graph_of(&[(k[0], vec![k[1], k[2]])]);
        assert_eq!(graph.size(), (1, 2));
        assert!(graph.is_built());
        // Root plus the two it follows.
        assert_eq!(graph.reachable(&[k[0]], 2).len(), 3);
    }
}
