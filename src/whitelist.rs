use crate::blacklist::Blacklist;
use crate::wot::WotOracle;
use nostr_sdk::prelude::PublicKey;
use parking_lot::RwLock;
use std::path::Path;
use std::sync::Arc;
use tracing::{info, warn};

const RUNTIME_FILE: &str = "whitelist_runtime.json";

/// Why a pubkey is allowed, ordered from most to least trusted.
///
/// Admission alone is too blunt a signal to budget on. Someone three hops out
/// is admitted on the strength of a follow chain nobody vouched for directly,
/// and should not get the same publishing budget as the operator. Making the
/// reason explicit is what lets rate limits scale with distance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessTier {
    /// The relay's own key, or a configured admin.
    Relay,
    /// Added by hand to the allowlist.
    Manual,
    /// Followed by a reference account, via follow sync.
    FollowSync,
    /// Reached through the follow graph, at this many hops.
    WebOfTrust(u8),
    /// Admitted only because nothing restricts access.
    Open,
    /// Not admitted at all.
    Denied,
}

impl AccessTier {
    /// Stable identifier for the API and the UI.
    pub fn as_str(self) -> &'static str {
        match self {
            AccessTier::Relay => "relay",
            AccessTier::Manual => "manual",
            AccessTier::FollowSync => "follow_sync",
            AccessTier::WebOfTrust(_) => "web_of_trust",
            AccessTier::Open => "open_relay",
            AccessTier::Denied => "denied",
        }
    }

    /// Share of the configured per-pubkey budget this tier may use, in
    /// hundredths.
    ///
    /// Trust decays with distance because the evidence does: a direct follow is
    /// a person vouching, three hops is a chain of strangers. An open relay
    /// grants the least of all — nobody vouched at all — which is what keeps
    /// "open" survivable rather than merely permitted.
    /// Which UI tier a hop count belongs to.
    ///
    /// The console presents admission as Tier 1/2/3, and the mapping has to be
    /// exhaustive or accounts vanish: hops 0 and 1 both land in Tier 1, because
    /// a reference account and the people it follows are trusted at full
    /// budget and `budget_percent` already groups them that way.
    ///
    /// Getting this wrong is not cosmetic. An earlier version of the tier
    /// listing treated Tier 1 as only the manual and follow-sync *lists*, so on
    /// a relay whose graph roots are pinned directly -- where follow sync never
    /// runs -- the root and everyone it followed appeared in no tier at all:
    /// seven accounts admitted, four listed.
    pub fn tier_for_hops(hops: u8) -> u8 {
        match hops {
            0 | 1 => 1,
            other => other,
        }
    }

    pub fn budget_percent(self) -> u32 {
        match self {
            AccessTier::Relay | AccessTier::Manual => 100,
            // Follow sync and "one hop" are the same population by definition —
            // follow sync *is* the one-hop set — so they share a budget rather
            // than appearing as two rungs with identical numbers.
            AccessTier::FollowSync | AccessTier::WebOfTrust(0..=1) => 100,
            AccessTier::WebOfTrust(2) => 50,
            AccessTier::WebOfTrust(_) => 25,
            AccessTier::Open => 10,
            AccessTier::Denied => 0,
        }
    }

    /// Key for the per-tier rate-limit buckets. Hop counts collapse to one of
    /// three bands, because a separate bucket per hop would be indistinguishable
    /// in practice and unbounded in principle.
    pub fn as_budget_key(self) -> &'static str {
        match self {
            AccessTier::Relay | AccessTier::Manual => "manual",
            // One bucket: follow sync and one hop are the same people.
            AccessTier::FollowSync | AccessTier::WebOfTrust(0..=1) => "direct",
            AccessTier::WebOfTrust(2) => "wot_mid",
            AccessTier::WebOfTrust(_) => "wot_far",
            AccessTier::Open => "open",
            AccessTier::Denied => "denied",
        }
    }

    /// Plain-language label for the console.
    pub fn label(self) -> String {
        match self {
            AccessTier::Relay => "Relay or admin".to_string(),
            AccessTier::Manual => "Added by hand".to_string(),
            AccessTier::FollowSync => "Followed by a reference account".to_string(),
            AccessTier::WebOfTrust(0) => "A reference account".to_string(),
            AccessTier::WebOfTrust(1) => "Followed by a reference account".to_string(),
            AccessTier::WebOfTrust(hops) => format!("{hops} hops away in the follow graph"),
            AccessTier::Open => "Anyone (open relay)".to_string(),
            AccessTier::Denied => "Not admitted".to_string(),
        }
    }
}

/// Thread-safe shared whitelist that supports runtime modifications, persistence,
/// follow-derived entries from reference accounts, Web-of-Trust admission, and
/// blacklist exclusion.
#[derive(Debug, Clone)]
pub struct Whitelist {
    /// Manually added pubkeys (config + runtime overrides)
    inner: Arc<RwLock<Vec<PublicKey>>>,
    /// Follow-derived pubkeys (from reference account sync)
    follow_derived: Arc<RwLock<Vec<PublicKey>>>,
    /// Blacklist that overrides whitelist membership
    blacklist: Blacklist,
    /// Optional Web-of-Trust tier. When present, a pubkey the oracle has
    /// resolved to within `max_hops` of a root is admitted even though it
    /// appears on neither list. `None` disables the tier entirely.
    wot: Arc<RwLock<Option<Arc<WotOracle>>>>,
}

impl Whitelist {
    /// Create a new whitelist from initial pubkeys, merging with any persisted runtime overrides.
    pub fn new(initial: Vec<PublicKey>, config_dir: Option<&Path>, blacklist: Blacklist) -> Self {
        let mut pubkeys = initial;

        // Merge runtime overrides if they exist
        if let Some(dir) = config_dir {
            let runtime_path = dir.join(RUNTIME_FILE);
            if runtime_path.exists() {
                match std::fs::read_to_string(&runtime_path) {
                    Ok(contents) => match serde_json::from_str::<Vec<String>>(&contents) {
                        Ok(hex_keys) => {
                            for hex in &hex_keys {
                                if let Ok(pk) = PublicKey::from_hex(hex) {
                                    if !pubkeys.contains(&pk) {
                                        pubkeys.push(pk);
                                    }
                                }
                            }
                            info!(
                                "Loaded {} runtime whitelist overrides from {}",
                                hex_keys.len(),
                                runtime_path.display()
                            );
                        }
                        Err(e) => warn!("Failed to parse {}: {}", runtime_path.display(), e),
                    },
                    Err(e) => warn!("Failed to read {}: {}", runtime_path.display(), e),
                }
            }
        }

        Self {
            inner: Arc::new(RwLock::new(pubkeys)),
            follow_derived: Arc::new(RwLock::new(Vec::new())),
            blacklist,
            wot: Arc::new(RwLock::new(None)),
        }
    }

    /// Attach (or with `None`, detach) the Web-of-Trust tier.
    pub fn set_wot(&self, wot: Option<Arc<WotOracle>>) {
        *self.wot.write() = wot;
    }

    /// The attached WoT oracle, if the tier is enabled.
    pub fn wot(&self) -> Option<Arc<WotOracle>> {
        self.wot.read().clone()
    }

    /// Check if a pubkey may use this relay: blacklisted keys are always
    /// refused, then manual, follow-derived, and Web-of-Trust admission are
    /// tried in turn.
    ///
    /// The WoT check only reads the oracle's cache — resolution happens in
    /// `WotAdmissionMiddleware` after NIP-42 auth, because this is called from
    /// synchronous code on the hot path.
    pub fn contains(&self, pk: &PublicKey) -> bool {
        if self.blacklist.contains(pk) {
            return false;
        }
        if self.inner.read().contains(pk) || self.follow_derived.read().contains(pk) {
            return true;
        }
        self.wot.read().as_ref().is_some_and(|wot| wot.admits(pk))
    }

    /// Which tier admits `pk`, or [`AccessTier::Denied`].
    ///
    /// Deliberately walks the same ladder as [`Self::contains`] in the same
    /// order: a tier used for budgeting that disagreed with the tier used for
    /// admission would throttle people by a rule they were not let in under.
    pub fn tier_of(&self, pk: &PublicKey) -> AccessTier {
        if self.blacklist.contains(pk) {
            return AccessTier::Denied;
        }
        if self.inner.read().contains(pk) {
            return AccessTier::Manual;
        }
        if self.follow_derived.read().contains(pk) {
            return AccessTier::FollowSync;
        }
        if let Some(wot) = self.wot.read().clone() {
            if let Some(hops) = wot.cached(pk).and_then(|v| v.hops) {
                if hops <= wot.max_hops() {
                    return AccessTier::WebOfTrust(hops);
                }
            }
            // The tier is on, so an unresolved key is not "open" -- it is
            // simply not admitted yet.
            return AccessTier::Denied;
        }
        if self.inner.read().is_empty() && self.follow_derived.read().is_empty() {
            return AccessTier::Open;
        }
        AccessTier::Denied
    }

    /// Check if the whitelist imposes no restriction at all.
    ///
    /// An enabled WoT tier is a restriction even with both lists empty: the
    /// caller (`GroupsRelayProcessor::is_allowed`) treats `true` as "admit
    /// everyone", so returning `true` here with WoT on would leave the relay
    /// wide open — the exact opposite of what enabling it asks for.
    pub fn is_empty(&self) -> bool {
        if self.wot.read().is_some() {
            return false;
        }
        self.inner.read().is_empty() && self.follow_derived.read().is_empty()
    }

    /// The whole admission rule for a connection: blacklist first, then an
    /// unrestricted relay admits everyone, then the tiers or relay-admin status.
    ///
    /// Lives here rather than in `GroupsRelayProcessor` so that anything which
    /// has to predict the processor's verdict — the unindexed-query budget,
    /// which must not charge for a REQ that admission is about to refuse — asks
    /// the same question instead of re-deriving it. `is_admin` is supplied by
    /// the caller because relay-admin status is not whitelist state.
    ///
    /// The blacklist is consulted before the open-relay short-circuit; see
    /// `GroupsRelayProcessor::is_allowed` for the bug that ordering fixed.
    pub fn admits(
        &self,
        pubkey: Option<&PublicKey>,
        is_admin: impl Fn(&PublicKey) -> bool,
    ) -> bool {
        if let Some(pk) = pubkey {
            if self.blacklist.contains(pk) {
                return false;
            }
        }
        if self.is_empty() {
            return true;
        }
        pubkey.is_some_and(|pk| self.contains(pk) || is_admin(pk))
    }

    /// Return a snapshot of all whitelisted pubkeys (manual + follow-derived union), excluding blacklisted.
    pub fn list(&self) -> Vec<PublicKey> {
        let manual = self.inner.read().clone();
        let follows = self.follow_derived.read().clone();
        let mut combined = manual;
        for pk in follows {
            if !combined.contains(&pk) {
                combined.push(pk);
            }
        }
        combined.retain(|pk| !self.blacklist.contains(pk));
        combined
    }

    /// Return only the manually added pubkeys.
    pub fn list_manual(&self) -> Vec<PublicKey> {
        self.inner.read().clone()
    }

    /// Return only the follow-derived pubkeys.
    pub fn list_follow_derived(&self) -> Vec<PublicKey> {
        self.follow_derived.read().clone()
    }

    /// Whether `pk` still needs an oracle lookup before [`Self::contains`] can
    /// give a meaningful answer.
    ///
    /// False when the tier is off, when the key is already settled by a cheaper
    /// tier, or when the oracle has a live verdict — so the common case costs a
    /// couple of lock reads and no network.
    pub fn needs_wot_resolution(&self, pk: &PublicKey) -> bool {
        let Some(wot) = self.wot.read().clone() else {
            return false;
        };
        if self.blacklist.contains(pk) {
            return false;
        }
        if self.inner.read().contains(pk) || self.follow_derived.read().contains(pk) {
            return false;
        }
        wot.cached(pk).is_none()
    }

    /// Resolve `pk` against the oracle, populating the cache that
    /// [`Self::contains`] reads. No-op when the tier is disabled.
    pub async fn resolve_wot(&self, pk: &PublicKey) {
        // Clone the handle out so no lock guard is held across the await.
        let wot = self.wot.read().clone();
        if let Some(wot) = wot {
            wot.resolve(pk).await;
        }
    }

    /// Pubkeys currently admitted by the Web-of-Trust tier, with hop counts,
    /// excluding blacklisted keys.
    ///
    /// Deliberately not folded into [`Self::list`]: that backs the whitelist
    /// screen and the `len()` count, and the WoT tier is not an enumerable set
    /// — this is only the subset of keys that have actually connected and been
    /// resolved.
    pub fn list_wot_admitted(&self) -> Vec<(PublicKey, u8)> {
        let Some(wot) = self.wot.read().clone() else {
            return Vec::new();
        };
        let mut admitted = wot.admitted();
        admitted.retain(|(pk, _)| !self.blacklist.contains(pk));
        admitted
    }

    /// Get a reference to the blacklist.
    pub fn blacklist(&self) -> &Blacklist {
        &self.blacklist
    }

    /// Add a pubkey to the manual whitelist. Returns true if it was added (not already present).
    pub fn add(&self, pk: PublicKey) -> bool {
        let mut guard = self.inner.write();
        if guard.contains(&pk) {
            return false;
        }
        guard.push(pk);
        true
    }

    /// Remove a pubkey from the manual whitelist. Returns true if it was removed.
    pub fn remove(&self, pk: &PublicKey) -> bool {
        let mut guard = self.inner.write();
        let len_before = guard.len();
        guard.retain(|p| p != pk);
        guard.len() < len_before
    }

    /// Replace manually configured pubkeys.
    pub fn replace_manual(&self, pubkeys: Vec<PublicKey>) {
        let mut guard = self.inner.write();
        *guard = pubkeys;
    }

    /// Number of whitelisted pubkeys (manual + follow-derived, deduplicated, excluding blacklisted).
    pub fn len(&self) -> usize {
        self.list().len()
    }

    /// Replace the entire follow-derived set.
    pub fn set_follow_derived(&self, pubkeys: Vec<PublicKey>) {
        let mut guard = self.follow_derived.write();
        *guard = pubkeys;
    }

    /// Persist the manual whitelist to `config/whitelist_runtime.json`.
    pub fn persist(&self, config_dir: &Path) -> Result<(), std::io::Error> {
        let hex_keys: Vec<String> = self.inner.read().iter().map(|pk| pk.to_hex()).collect();
        let json = serde_json::to_string_pretty(&hex_keys)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let path = config_dir.join(RUNTIME_FILE);
        std::fs::write(&path, json)?;
        info!(
            "Persisted {} whitelist entries to {}",
            hex_keys.len(),
            path.display()
        );
        Ok(())
    }
}

#[cfg(test)]
mod tier_mapping_tests {
    use super::AccessTier;

    /// Every hop the graph can report must land in exactly one tier, or
    /// accounts disappear from the console while still being admitted.
    #[test]
    fn hop_to_tier_is_exhaustive_and_stable() {
        assert_eq!(
            AccessTier::tier_for_hops(0),
            1,
            "a reference account is Tier 1"
        );
        assert_eq!(AccessTier::tier_for_hops(1), 1, "so is anyone it follows");
        assert_eq!(AccessTier::tier_for_hops(2), 2);
        assert_eq!(AccessTier::tier_for_hops(3), 3);
        assert_eq!(AccessTier::tier_for_hops(5), 5, "max_hops is capped at 5");
    }

    /// The tiers must agree with the publishing budget, or an operator sees one
    /// grouping on the Access screen and a different one in the rate ladder.
    #[test]
    fn tiers_agree_with_the_budget_grouping() {
        let tier1: Vec<u32> = [0u8, 1]
            .iter()
            .map(|h| AccessTier::WebOfTrust(*h).budget_percent())
            .collect();
        assert_eq!(tier1, vec![100, 100], "Tier 1 is the full-budget tier");
        assert_eq!(AccessTier::WebOfTrust(2).budget_percent(), 50);
        assert_eq!(AccessTier::WebOfTrust(3).budget_percent(), 25);
    }
}

#[cfg(test)]
mod tests {
    use super::AccessTier;
    use super::*;
    use crate::wot::{WotConfig, WotOracle};
    use nostr_sdk::prelude::Keys;

    /// An oracle pointed at a port nothing is listening on: every query fails,
    /// so it admits nobody. Enough to exercise tier precedence and `is_empty`,
    /// which never depend on a successful lookup.
    fn unreachable_oracle(roots: Vec<PublicKey>) -> Arc<WotOracle> {
        WotOracle::new(
            WotConfig {
                oracle_url: "http://127.0.0.1:1".to_string(),
                fallback_oracle_url: None,
                // Exercise the oracle path: these tests are about tier
                // precedence, and a dead oracle is the simplest way to get a
                // tier that admits only what it is told to.
                local: false,
                timeout: std::time::Duration::from_millis(50),
                ..WotConfig::default()
            },
            roots,
        )
        .unwrap()
    }

    fn whitelist(manual: Vec<PublicKey>) -> Whitelist {
        Whitelist::new(manual, None, Blacklist::new(None))
    }

    #[test]
    fn empty_lists_and_no_wot_means_no_restriction() {
        assert!(whitelist(vec![]).is_empty());
    }

    #[test]
    fn enabling_wot_is_itself_a_restriction() {
        let list = whitelist(vec![]);
        list.set_wot(Some(unreachable_oracle(vec![])));

        assert!(
            !list.is_empty(),
            "an empty manual list plus WoT must not read as an open relay"
        );
    }

    #[test]
    fn detaching_wot_restores_the_open_reading() {
        let list = whitelist(vec![]);
        list.set_wot(Some(unreachable_oracle(vec![])));
        list.set_wot(None);

        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn wot_admits_a_key_on_neither_list() {
        let root = Keys::generate().public_key();
        let oracle = unreachable_oracle(vec![root]);
        let list = whitelist(vec![]);
        list.set_wot(Some(oracle.clone()));

        // A root resolves to zero hops without touching the network.
        assert!(!list.contains(&root), "not yet resolved");
        oracle.resolve(&root).await;
        assert!(list.contains(&root));
    }

    #[tokio::test]
    async fn blacklist_beats_a_wot_admission() {
        let root = Keys::generate().public_key();
        let blacklist = Blacklist::new(None);
        let list = Whitelist::new(vec![], None, blacklist.clone());
        let oracle = unreachable_oracle(vec![root]);
        list.set_wot(Some(oracle.clone()));

        oracle.resolve(&root).await;
        assert!(list.contains(&root));

        blacklist.add(root);
        assert!(
            !list.contains(&root),
            "the blacklist must override every admission tier"
        );
        assert!(list.list_wot_admitted().is_empty());
    }

    #[tokio::test]
    async fn an_unresolved_key_is_refused() {
        let list = whitelist(vec![]);
        list.set_wot(Some(unreachable_oracle(
            vec![Keys::generate().public_key()],
        )));

        assert!(
            !list.contains(&Keys::generate().public_key()),
            "a key the oracle has not answered for must fail closed"
        );
    }

    #[tokio::test]
    async fn wot_admissions_stay_out_of_list_and_len() {
        let root = Keys::generate().public_key();
        let manual = Keys::generate().public_key();
        let list = whitelist(vec![manual]);
        let oracle = unreachable_oracle(vec![root]);
        list.set_wot(Some(oracle.clone()));

        oracle.resolve(&root).await;
        assert!(list.contains(&root));

        assert_eq!(list.list(), vec![manual]);
        assert_eq!(list.len(), 1, "the WoT tier is not an enumerable set");
        assert_eq!(list.list_wot_admitted(), vec![(root, 0)]);
    }

    /// The tier used for budgeting must agree with the tier that admitted
    /// someone, or people get throttled under a rule they were not let in by.
    #[tokio::test]
    async fn tier_of_follows_the_same_ladder_as_contains() {
        let manual = Keys::generate().public_key();
        let followed = Keys::generate().public_key();
        let root = Keys::generate().public_key();
        let blocked = Keys::generate().public_key();

        let blacklist = Blacklist::new(None);
        let list = Whitelist::new(vec![manual], None, blacklist.clone());
        list.set_follow_derived(vec![followed]);
        let oracle = unreachable_oracle(vec![root]);
        list.set_wot(Some(oracle.clone()));
        oracle.resolve(&root).await;

        assert_eq!(list.tier_of(&manual), AccessTier::Manual);
        assert_eq!(list.tier_of(&followed), AccessTier::FollowSync);
        assert_eq!(list.tier_of(&root), AccessTier::WebOfTrust(0));
        assert_eq!(
            list.tier_of(&Keys::generate().public_key()),
            AccessTier::Denied
        );

        // Blacklist wins over every tier, exactly as in `contains`.
        blacklist.add(manual);
        assert_eq!(list.tier_of(&manual), AccessTier::Denied);
        assert!(!list.contains(&manual));
        assert_eq!(list.tier_of(&blocked), AccessTier::Denied);
    }

    #[test]
    fn an_unrestricted_relay_reports_the_open_tier() {
        let list = whitelist(vec![]);
        assert!(list.is_empty());
        assert_eq!(
            list.tier_of(&Keys::generate().public_key()),
            AccessTier::Open
        );
    }

    /// Budget falls off with distance, and never to zero: a tier admitted but
    /// unable to publish is a confusing way to say "denied".
    #[test]
    fn publishing_budget_decays_with_distance() {
        assert_eq!(AccessTier::Manual.budget_percent(), 100);
        assert_eq!(AccessTier::FollowSync.budget_percent(), 100);
        assert_eq!(AccessTier::WebOfTrust(1).budget_percent(), 100);
        assert_eq!(AccessTier::WebOfTrust(2).budget_percent(), 50);
        assert_eq!(AccessTier::WebOfTrust(3).budget_percent(), 25);
        assert_eq!(AccessTier::WebOfTrust(5).budget_percent(), 25);

        // An open relay trusts nobody, so it grants the least.
        assert!(AccessTier::Open.budget_percent() < AccessTier::WebOfTrust(3).budget_percent());
        assert_eq!(AccessTier::Denied.budget_percent(), 0);
    }

    /// Distinct buckets where the budget differs, shared where it does not.
    #[test]
    fn budget_keys_separate_the_tiers_that_differ() {
        assert_ne!(
            AccessTier::WebOfTrust(2).as_budget_key(),
            AccessTier::WebOfTrust(3).as_budget_key()
        );
        // Follow sync and one hop are the same people, so one bucket.
        assert_eq!(
            AccessTier::FollowSync.as_budget_key(),
            AccessTier::WebOfTrust(1).as_budget_key()
        );
        assert_eq!(
            AccessTier::WebOfTrust(3).as_budget_key(),
            AccessTier::WebOfTrust(9).as_budget_key(),
            "distant hops share a bucket rather than growing one per hop"
        );
        assert_ne!(
            AccessTier::Open.as_budget_key(),
            AccessTier::Manual.as_budget_key()
        );
    }

    #[test]
    fn manual_and_follow_derived_still_work_without_wot() {
        let manual = Keys::generate().public_key();
        let followed = Keys::generate().public_key();
        let list = whitelist(vec![manual]);
        list.set_follow_derived(vec![followed]);

        assert!(list.contains(&manual));
        assert!(list.contains(&followed));
        assert!(!list.contains(&Keys::generate().public_key()));
    }
}
