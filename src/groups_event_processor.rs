use crate::groups::{
    Group, ADDRESSABLE_EVENT_KINDS, KIND_GROUP_ADD_USER_9000, KIND_GROUP_CREATE_9007,
    KIND_GROUP_CREATE_INVITE_9009, KIND_GROUP_DELETE_9008, KIND_GROUP_DELETE_EVENT_9005,
    KIND_GROUP_EDIT_METADATA_9002, KIND_GROUP_REMOVE_USER_9001, KIND_GROUP_SET_ROLES_9006,
    KIND_GROUP_USER_JOIN_REQUEST_9021, KIND_GROUP_USER_LEAVE_REQUEST_9022, NON_GROUP_ALLOWED_KINDS,
};
use crate::obelisk_index::ObeliskIndex;
use crate::whitelist::{AccessTier, Whitelist};
use crate::Groups;
use governor::{DefaultKeyedRateLimiter, Quota, RateLimiter};
use nostr_lmdb::Scope;
use nostr_sdk::prelude::*;
use relay_builder::{EventContext, EventProcessor, Result, StoreCommand};
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, warn};

/// NIP-46 remote-signing coordination (nostr-connect).
///
/// Exempt from the whitelist, deliberately. The admin console's login uses this
/// relay as its first NIP-46 rendezvous, so a bunker's ephemeral client key has
/// to be able to publish and subscribe here *before* anyone is authenticated.
/// Without the exemption, turning on any access restriction locks the operator
/// out of the console that would let them turn it off again — which is exactly
/// what happened when Web-of-Trust admission was first enabled.
///
/// Safe to exempt: kind 24133 is ephemeral, so it is never stored, and its
/// payloads are NIP-44 encrypted between two keys that already know each other.
const KIND_NIP46_SIGNER: u16 = 24133;

/// Per-pubkey token-bucket rate limiter. Keyed by pubkey hex; spammers reconnecting
/// or rotating connections still hit the same bucket as long as they sign with the same key.
pub type PubkeyLimiter = DefaultKeyedRateLimiter<PublicKey>;

/// Groups event processor implementing NIP-29 (Relay-based Groups) functionality.
///
/// This implementation provides all the business logic for managing groups, including:
/// - Group creation and management
/// - Member access control and permissions
/// - Group content validation and storage
/// - Deletion and moderation events
/// - Unmanaged group support
///
/// The processor is extracted from the original Nip29Middleware to enable reusability
/// and better testability while maintaining identical functionality.
/// Sustained report budget per pubkey. Genuine reporting is occasional; this is
/// generous for a person and useless for a flood.
const REPORTS_PER_HOUR: NonZeroU32 = NonZeroU32::new(20).unwrap();

/// Allowance for someone working through a spam wave in one sitting.
const REPORTS_BURST: NonZeroU32 = NonZeroU32::new(40).unwrap();

#[derive(Clone)]
pub struct GroupsRelayProcessor {
    groups: Arc<Groups>,
    relay_pubkey: PublicKey,
    admin_pubkeys: Vec<PublicKey>,
    whitelist: Whitelist,
    /// Optional per-pubkey rate limiter. None disables per-pubkey rate limiting.
    /// Kept as the fallback bucket for tiers with no entry of their own.
    pubkey_limiter: Option<Arc<PubkeyLimiter>>,
    /// One bucket per access tier, so publishing budget falls off with distance.
    tier_limiters: Option<Arc<HashMap<&'static str, Arc<PubkeyLimiter>>>>,
    /// Separate, far tighter budget for kind 1984. Protects the moderation queue
    /// rather than the database; see `with_pubkey_rate_limit`.
    report_limiter: Option<Arc<PubkeyLimiter>>,
    /// Snapshots of reported content, captured as reports arrive so the evidence
    /// survives the message being deleted or pruned.
    report_evidence: Option<Arc<crate::reports::EvidenceStore>>,
    /// Where to persist those snapshots.
    config_dir: Option<Arc<std::path::PathBuf>>,
    /// Optional optimized Obelisk read index. Normal relay behavior does not
    /// depend on this; it is updated only after events pass relay validation.
    obelisk_index: Option<Arc<ObeliskIndex>>,
}

impl std::fmt::Debug for GroupsRelayProcessor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroupsRelayProcessor")
            .field("relay_pubkey", &self.relay_pubkey)
            .field("admin_pubkey_count", &self.admin_pubkeys.len())
            .field("whitelist_empty", &self.whitelist.is_empty())
            .field("pubkey_limiter", &self.pubkey_limiter.is_some())
            .field("obelisk_index", &self.obelisk_index.is_some())
            .finish()
    }
}

impl GroupsRelayProcessor {
    /// Create a new groups event processor instance.
    pub fn new(groups: Arc<Groups>, relay_pubkey: PublicKey, whitelist: Whitelist) -> Self {
        Self::with_admin_pubkeys(groups, relay_pubkey, Vec::new(), whitelist)
    }

    pub fn with_admin_pubkeys(
        groups: Arc<Groups>,
        relay_pubkey: PublicKey,
        admin_pubkeys: Vec<PublicKey>,
        whitelist: Whitelist,
    ) -> Self {
        Self {
            groups,
            relay_pubkey,
            admin_pubkeys,
            whitelist,
            pubkey_limiter: None,
            tier_limiters: None,
            report_limiter: None,
            report_evidence: None,
            config_dir: None,
            obelisk_index: None,
        }
    }

    /// Attach per-pubkey rate limiters, one bucket per access tier.
    ///
    /// `events_per_minute` is the budget for a fully trusted key; every other
    /// tier gets a share of it via [`AccessTier::budget_percent`]. One limiter
    /// for everyone would mean an account admitted on a three-hop follow chain
    /// publishing as freely as the operator, which is the whole point of
    /// measuring distance. `0` disables rate limiting entirely.
    pub fn with_pubkey_rate_limit(mut self, events_per_minute: u32) -> Self {
        if events_per_minute == 0 {
            return self;
        }

        let mut tiers = HashMap::new();
        for tier in [
            AccessTier::Manual,
            AccessTier::FollowSync,
            AccessTier::WebOfTrust(2),
            AccessTier::WebOfTrust(3),
            AccessTier::Open,
        ] {
            // At least one event a minute: a tier throttled to zero would be
            // admitted and then unable to say anything, which is a confusing
            // way to express "denied".
            let budget = (events_per_minute * tier.budget_percent() / 100).max(1);
            if let Some(n) = NonZeroU32::new(budget) {
                tiers.insert(
                    tier.as_budget_key(),
                    Arc::new(RateLimiter::keyed(Quota::per_minute(n))),
                );
            }
        }

        self.tier_limiters = Some(Arc::new(tiers));
        if let Some(n) = NonZeroU32::new(events_per_minute) {
            self.pubkey_limiter = Some(Arc::new(RateLimiter::keyed(Quota::per_minute(n))));
        }

        // Reports get their own, much smaller budget, independent of the general
        // one. The thing being protected is not the database -- a 1984 is a tiny
        // event and the general limit already bounds write volume -- it is the
        // moderation queue. Reports group by target, so flooding it means
        // reporting many *different* things, which no honest user does: genuine
        // reporting is occasional and considered. Left on the general budget, one
        // account could file thousands of reports against legitimate messages and
        // bury the real ones, which is a denial of service against moderation
        // rather than against the relay.
        self.report_limiter = Some(Arc::new(RateLimiter::keyed(
            Quota::per_hour(REPORTS_PER_HOUR).allow_burst(REPORTS_BURST),
        )));
        self
    }

    /// The bucket for whichever tier admitted this pubkey.
    fn limiter_for(&self, pubkey: &PublicKey) -> Option<&Arc<PubkeyLimiter>> {
        let tiers = self.tier_limiters.as_ref()?;
        let tier = self.whitelist.tier_of(pubkey);
        tiers
            .get(&tier.as_budget_key())
            .or(self.pubkey_limiter.as_ref())
    }

    /// Capture what a report points at, as it arrives.
    pub fn with_report_evidence(
        mut self,
        evidence: Arc<crate::reports::EvidenceStore>,
        config_dir: std::path::PathBuf,
    ) -> Self {
        self.report_evidence = Some(evidence);
        self.config_dir = Some(Arc::new(config_dir));
        self
    }

    /// Snapshot every event a report names, before anything can remove it.
    ///
    /// Runs on the way in rather than when the queue is read, because by read
    /// time the message may be gone -- which is the whole problem. Failures are
    /// logged and swallowed: a report must still be accepted even if the relay
    /// cannot find what it refers to, since the reported event may live on
    /// another relay entirely.
    async fn capture_report_evidence(&self, report: &Event, scope: &Scope) {
        let (Some(store), Some(config_dir)) = (&self.report_evidence, &self.config_dir) else {
            return;
        };

        let targets: Vec<EventId> = report
            .tags
            .iter()
            .filter_map(|t| {
                let v = t.as_slice();
                (v.first().map(String::as_str) == Some("e") && v.len() >= 2)
                    .then(|| EventId::from_hex(&v[1]).ok())
                    .flatten()
            })
            .collect();

        let mut captured_any = false;
        for target in targets {
            let hex = target.to_hex();
            if store.get(&hex).is_some() {
                continue;
            }

            let found = match self
                .groups
                .database()
                .query(vec![Filter::new().id(target).limit(1)], scope)
                .await
            {
                Ok(events) => events.into_iter().next(),
                Err(e) => {
                    debug!("Could not look up reported event {hex}: {e}");
                    continue;
                }
            };

            let Some(event) = found else { continue };

            let captured = store.capture(
                hex.clone(),
                crate::reports::Evidence {
                    author: event.pubkey.to_hex(),
                    // Bounded: this is retained indefinitely, and a moderator
                    // needs enough to judge rather than the whole payload.
                    content: event.content.chars().take(2000).collect(),
                    group_id: event
                        .tags
                        .find(TagKind::h())
                        .and_then(|t| t.content())
                        .map(str::to_string),
                    kind: event.kind.as_u16(),
                    created_at: event.created_at.as_secs(),
                    captured_at: Timestamp::now().as_secs(),
                },
            );
            captured_any |= captured;
        }

        if captured_any {
            if let Err(e) = store.persist(config_dir.as_path()) {
                warn!("Failed to persist report evidence: {e}");
            }
        }
    }

    pub fn with_obelisk_index(mut self, obelisk_index: Arc<ObeliskIndex>) -> Self {
        self.obelisk_index = Some(obelisk_index);
        self
    }

    /// Check if a pubkey is allowed to use this relay.
    ///
    /// The blacklist is consulted *before* the open-relay short-circuit. It used
    /// to come after, which meant that on a relay with no whitelist and no
    /// Web-of-Trust tier -- i.e. an open one -- `is_empty()` returned true and
    /// admission returned early, so blacklisting somebody silently did nothing.
    /// The entry appeared in the console and in blacklist.json, and the account
    /// carried on publishing. `Whitelist::contains` already honours the
    /// blacklist; the bug was that it never got asked.
    ///
    /// A ban has to mean the same thing in every configuration, and "blocked"
    /// is the one answer that must never depend on how permissive the relay is.
    fn is_allowed(&self, pubkey: &Option<PublicKey>) -> bool {
        if let Some(pk) = pubkey {
            if self.whitelist.blacklist().contains(pk) {
                return false;
            }
        }

        if self.whitelist.is_empty() {
            return true;
        }
        match pubkey {
            Some(pk) => self.whitelist.contains(pk) || self.is_relay_admin(pk),
            None => false,
        }
    }

    fn is_relay_admin(&self, pubkey: &PublicKey) -> bool {
        *pubkey == self.relay_pubkey || self.admin_pubkeys.contains(pubkey)
    }

    /// Get a reference to the groups state manager
    pub fn groups(&self) -> &Arc<Groups> {
        &self.groups
    }

    /// Get the relay public key
    pub fn relay_pubkey(&self) -> &PublicKey {
        &self.relay_pubkey
    }

    /// Checks if a filter is querying group-related data
    fn is_group_query(&self, filter: &Filter) -> bool {
        filter
            .generic_tags
            .contains_key(&SingleLetterTag::lowercase(Alphabet::H))
            || filter
                .generic_tags
                .contains_key(&SingleLetterTag::lowercase(Alphabet::D))
    }

    /// Checks if a filter is querying addressable event kinds
    fn is_addressable_query(&self, filter: &Filter) -> bool {
        filter
            .kinds
            .as_ref()
            .is_some_and(|kinds| kinds.iter().any(|k| ADDRESSABLE_EVENT_KINDS.contains(k)))
    }

    /// Gets all group tags from a filter
    fn get_group_tags<'a>(&self, filter: &'a Filter) -> impl Iterator<Item = String> + 'a {
        filter
            .generic_tags
            .iter()
            .filter(|(k, _)| k == &&SingleLetterTag::lowercase(Alphabet::H))
            .flat_map(|(_, tag_set)| tag_set.iter())
            .cloned()
    }
}

impl EventProcessor for GroupsRelayProcessor {
    fn verify_filters(
        &self,
        filters: &[Filter],
        _custom_state: Arc<RwLock<()>>,
        context: &EventContext,
    ) -> Result<()> {
        // A NIP-46 signer subscribes for its replies before anyone has
        // authenticated, so a filter that asks only for signer traffic is
        // exempt. Anything broader still needs admission.
        let signer_handshake = !filters.is_empty()
            && filters.iter().all(|f| {
                f.kinds
                    .as_ref()
                    .is_some_and(|kinds| kinds.iter().all(|k| k.as_u16() == KIND_NIP46_SIGNER))
            });

        // Enforce pubkey whitelist
        if !signer_handshake && !self.is_allowed(&context.authed_pubkey) {
            return Err(relay_builder::Error::auth_required(
                "Authentication required: this relay only accepts whitelisted pubkeys".to_string(),
            ));
        }

        // For groups relay, we need to verify access to group queries
        for filter in filters {
            // Moderation reports are readable only by relay admins.
            //
            // Kind 1984 has to be *accepted* without an `h` tag for reports to be
            // filable at all, but accepting it must not make the queue public.
            // NIP-56 treats reports as public by convention; on a relay where
            // admission is a social graph, that convention means the reported
            // person can look up who reported them. That is a retaliation
            // channel, so this relay diverges deliberately.
            //
            // Refused rather than silently emptied: a client asking for reports
            // should learn it may not have them, not conclude there are none.
            if filter
                .kinds
                .as_ref()
                .is_some_and(|kinds| kinds.contains(&crate::reports::KIND_REPORT_1984))
            {
                let is_admin = context
                    .authed_pubkey
                    .as_ref()
                    .is_some_and(|pk| self.is_relay_admin(pk));
                if !is_admin {
                    return Err(relay_builder::Error::restricted(
                        "Moderation reports are only readable by relay admins".to_string(),
                    ));
                }
            }

            // Check if this filter queries group-related data
            if self.is_group_query(filter) {
                // Get all group tags from the filter
                let group_tags: Vec<String> = self.get_group_tags(filter).collect();

                // Verify access to each group mentioned in the filter
                for group_tag in group_tags {
                    if let Some(group_ref) = self.groups.get_group(&context.subdomain, &group_tag) {
                        // Managed group - check if the user can read from this group
                        let group = group_ref.value();
                        if group.metadata.private {
                            // Private group - user must be a member or relay admin
                            if let Some(pubkey) = &context.authed_pubkey {
                                // Relay admin has access to all groups
                                if !self.is_relay_admin(pubkey) && !group.is_member(pubkey) {
                                    return Err(relay_builder::Error::restricted(
                                        "Access denied to private group".to_string(),
                                    ));
                                }
                            } else {
                                return Err(relay_builder::Error::auth_required(
                                    "Authentication required to access private groups".to_string(),
                                ));
                            }
                        }
                        // Public groups allow everyone to read
                    }
                    // Unmanaged groups are allowed (everyone can read from them)
                }
            }

            // For addressable events, verify the user can access the groups they reference
            if self.is_addressable_query(filter) {
                // Addressable events might reference groups in their identifiers
                // For now, we'll allow these queries and rely on visibility filtering
                // during event delivery to handle access control
            }
        }

        Ok(())
    }

    fn can_see_event(
        &self,
        event: &Event,
        _custom_state: Arc<RwLock<()>>,
        context: &EventContext,
    ) -> Result<bool> {
        // Check if this is a group event
        if let Some(group_ref) = self.groups.find_group_from_event(event, &context.subdomain) {
            // Group event - check access control using the group's can_see_event method
            group_ref
                .value()
                .can_see_event(&context.authed_pubkey, &context.relay_pubkey, event)
        } else {
            // Not a group event or unmanaged group - allow it through
            Ok(true)
        }
    }

    async fn handle_event(
        &self,
        event: Event,
        _custom_state: Arc<RwLock<()>>,
        context: &EventContext,
    ) -> Result<Vec<StoreCommand>> {
        // Signer coordination is exempt: it is how an operator authenticates in
        // the first place. See KIND_NIP46_SIGNER.
        let is_signer_traffic = event.kind.as_u16() == KIND_NIP46_SIGNER;

        // Enforce pubkey whitelist
        // Check the *event's* author against the blacklist, not just the
        // authenticated identity.
        //
        // `is_allowed` below keys on `context.authed_pubkey`, which is None
        // whenever the client has not completed NIP-42 -- and on an open relay
        // it never has to, because nothing forces an AUTH. So a banned key could
        // simply not authenticate and publish freely: the ban applied to a
        // session identity the spammer had no reason to establish.
        //
        // The signature is the identity here. Knowing who signed an event does
        // not require them to have announced themselves first.
        if !is_signer_traffic && self.whitelist.blacklist().contains(&event.pubkey) {
            return Err(relay_builder::Error::restricted(
                "This pubkey is blocked from this relay".to_string(),
            ));
        }

        if !is_signer_traffic && !self.is_allowed(&context.authed_pubkey) {
            return Err(relay_builder::Error::restricted(
                "Access denied: your pubkey is not whitelisted on this relay".to_string(),
            ));
        }

        // Per-pubkey rate limit: spammers signing with the same key share one bucket.
        // Relay's own pubkey is exempt (used for replaceable group state events).
        if let Some(limiter) = self.limiter_for(&event.pubkey) {
            if !is_signer_traffic
                && event.pubkey != self.relay_pubkey
                && limiter.check_key(&event.pubkey).is_err()
            {
                return Err(relay_builder::Error::restricted(
                    "rate limit exceeded for this pubkey".to_string(),
                ));
            }
        }

        // Reports carry a second, much tighter budget on top of the general one.
        // See `with_pubkey_rate_limit`: the resource being protected is the
        // admin's attention, not the database. Relay admins are exempt so a
        // moderator sweeping a spam wave is never throttled out of their own
        // tooling.
        if event.kind == crate::reports::KIND_REPORT_1984 {
            if let Some(limiter) = &self.report_limiter {
                if !self.is_relay_admin(&event.pubkey) && limiter.check_key(&event.pubkey).is_err()
                {
                    return Err(relay_builder::Error::restricted(
                        "report rate limit exceeded; reports are limited per account".to_string(),
                    ));
                }
            }
        }

        let subdomain = context.subdomain.clone();

        // Snapshot what this report points at, now, while it still exists.
        if event.kind == crate::reports::KIND_REPORT_1984 {
            self.capture_report_evidence(&event, &subdomain).await;
        }

        // Allow events through for unmanaged groups (groups not in relay state)
        // Per NIP-29: In unmanaged groups, everyone is considered a member
        // These groups can later be converted to managed groups by the relay admin
        if event.tags.find(TagKind::h()).is_some()
            && !Group::is_group_management_kind(event.kind)
            && self
                .groups
                .find_group_from_event(&event, &subdomain)
                .is_none()
        {
            debug!(target: "groups_relay_logic", "Processing unmanaged group event: kind={}, id={}", event.kind, event.id);
            return Ok(vec![StoreCommand::SaveSignedEvent(
                Box::new(event),
                (*subdomain).clone(),
                None,
            )]);
        }

        let events_to_save = match event.kind {
            k if k == KIND_GROUP_CREATE_9007 => {
                debug!(target: "groups_relay_logic", "Processing group create event: id={}", event.id);
                let commands = self
                    .groups
                    .handle_group_create(Box::new(event), &subdomain)
                    .await?;
                debug!(target: "groups_relay_logic", "Group create generated {} commands", commands.len());
                for cmd in &commands {
                    match cmd {
                        StoreCommand::SaveSignedEvent(_, _, _) => {
                            debug!(target: "groups_relay_logic", "  - SaveSignedEvent");
                        }
                        StoreCommand::SaveUnsignedEvent(evt, _, _) => {
                            debug!(target: "groups_relay_logic", "  - SaveUnsignedEvent: kind={}", evt.kind);
                        }
                        StoreCommand::DeleteEvents(_, _, _) => {
                            debug!(target: "groups_relay_logic", "  - DeleteEvents");
                        }
                    }
                }
                commands
            }

            k if k == KIND_GROUP_EDIT_METADATA_9002 => {
                debug!(target: "groups_relay_logic", "Processing group edit metadata event: id={}", event.id);
                self.groups
                    .handle_edit_metadata(Box::new(event), &subdomain)?
            }

            k if k == KIND_GROUP_USER_JOIN_REQUEST_9021 => {
                debug!(target: "groups_relay_logic", "Processing group join request: id={}", event.id);
                self.groups
                    .handle_join_request(Box::new(event), &subdomain)?
            }

            k if k == KIND_GROUP_USER_LEAVE_REQUEST_9022 => {
                debug!(target: "groups_relay_logic", "Processing group leave request: id={}", event.id);
                self.groups
                    .handle_leave_request(Box::new(event), &subdomain)?
            }

            k if k == KIND_GROUP_SET_ROLES_9006 => {
                debug!(target: "groups_relay_logic", "Processing group set roles event: id={}", event.id);
                self.groups.handle_set_roles(Box::new(event), &subdomain)?
            }

            k if k == KIND_GROUP_ADD_USER_9000 => {
                debug!(target: "groups_relay_logic", "Processing group add user event: id={}", event.id);
                self.groups.handle_put_user(Box::new(event), &subdomain)?
            }

            k if k == KIND_GROUP_REMOVE_USER_9001 => {
                debug!(target: "groups_relay_logic", "Processing group remove user event: id={}", event.id);
                self.groups
                    .handle_remove_user(Box::new(event), &subdomain)?
            }

            k if k == KIND_GROUP_DELETE_9008 => {
                debug!(target: "groups_relay_logic", "Processing group deletion event: id={}", event.id);
                self.groups
                    .handle_delete_group(Box::new(event), &subdomain)?
            }

            k if k == KIND_GROUP_DELETE_EVENT_9005 => {
                debug!(target: "groups_relay_logic", "Processing group content event deletion: id={}", event.id);
                self.groups
                    .handle_delete_event(Box::new(event), &subdomain)?
            }

            k if k == KIND_GROUP_CREATE_INVITE_9009 => {
                debug!(target: "groups_relay_logic", "Processing group create invite event: id={}", event.id);
                self.groups
                    .handle_create_invite(Box::new(event), &subdomain)?
            }

            k if !NON_GROUP_ALLOWED_KINDS.contains(&k)
                && event.tags.find(TagKind::h()).is_some() =>
            {
                debug!(target: "groups_relay_logic", "Processing group content event: kind={}, id={}", event.kind, event.id);
                self.groups
                    .handle_group_content(Box::new(event), &subdomain)?
            }

            _ => {
                debug!(target: "groups_relay_logic", "Processing non-group event: kind={}, id={}", event.kind, event.id);
                vec![StoreCommand::SaveSignedEvent(
                    Box::new(event),
                    (*subdomain).clone(),
                    None,
                )]
            }
        };

        if let Some(obelisk_index) = &self.obelisk_index {
            obelisk_index.apply_store_commands(&events_to_save);
        }

        debug!(target: "groups_relay_logic", "Returning {} store commands from handle_event", events_to_save.len());
        Ok(events_to_save)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{create_test_event, create_test_keys, setup_test};
    use crate::whitelist::{AccessTier, Whitelist};
    use nostr_lmdb::Scope;

    fn empty_state() -> Arc<RwLock<()>> {
        Arc::new(RwLock::new(()))
    }

    #[tokio::test]
    async fn test_groups_relay_logic_creation() {
        let (_tmp_dir, database, admin_keys) = setup_test().await;
        let groups = Arc::new(
            Groups::load_groups(
                database.clone(),
                admin_keys.public_key(),
                "wss://test.relay.com".to_string(),
                false,
            )
            .await
            .unwrap(),
        );

        let processor = GroupsRelayProcessor::new(
            groups.clone(),
            admin_keys.public_key(),
            Whitelist::new(vec![], None, crate::blacklist::Blacklist::new(None)),
        );

        // Verify the logic was created correctly
        assert_eq!(processor.relay_pubkey(), &admin_keys.public_key());
        assert!(Arc::ptr_eq(processor.groups(), &groups));
    }

    /// Reports have to be accepted without an `h` tag for anyone to file one,
    /// which makes it easy to accidentally publish the moderation queue: the
    /// reported party subscribes to kind 1984 and reads who reported them.
    #[tokio::test]
    async fn the_report_queue_is_not_readable_by_the_people_in_it() {
        let (_tmp_dir, database, admin_keys) = setup_test().await;
        let groups = Arc::new(
            Groups::load_groups(
                database.clone(),
                admin_keys.public_key(),
                "wss://test.relay.com".to_string(),
                false,
            )
            .await
            .unwrap(),
        );

        // Open relay: admission is not what is being tested here.
        let member = Keys::generate();
        let processor = GroupsRelayProcessor::new(
            groups,
            admin_keys.public_key(),
            Whitelist::new(vec![], None, crate::blacklist::Blacklist::new(None)),
        );

        let reports = vec![Filter::new().kind(crate::reports::KIND_REPORT_1984)];

        let as_member = EventContext {
            authed_pubkey: Some(member.public_key()),
            subdomain: Arc::new(Scope::Default),
            relay_pubkey: admin_keys.public_key(),
        };
        assert!(
            processor
                .verify_filters(&reports, empty_state(), &as_member)
                .is_err(),
            "an ordinary admitted user must not be able to read reports"
        );

        let anonymous = EventContext {
            authed_pubkey: None,
            subdomain: Arc::new(Scope::Default),
            relay_pubkey: admin_keys.public_key(),
        };
        assert!(
            processor
                .verify_filters(&reports, empty_state(), &anonymous)
                .is_err(),
            "nor an unauthenticated one"
        );

        let as_admin = EventContext {
            authed_pubkey: Some(admin_keys.public_key()),
            subdomain: Arc::new(Scope::Default),
            relay_pubkey: admin_keys.public_key(),
        };
        assert!(
            processor
                .verify_filters(&reports, empty_state(), &as_admin)
                .is_ok(),
            "the relay admin is the one who has to read them"
        );

        // Mixing 1984 into a wider filter must not launder it past the check.
        let smuggled = vec![Filter::new()
            .kind(Kind::Custom(9))
            .kind(crate::reports::KIND_REPORT_1984)];
        assert!(
            processor
                .verify_filters(&smuggled, empty_state(), &as_member)
                .is_err(),
            "asking for reports alongside chat must not slip through"
        );
    }

    /// Found by exercising the moderation queue end to end: blacklisting from a
    /// report said "done", wrote the entry to disk, and the account kept
    /// publishing. `is_allowed` short-circuited on `whitelist.is_empty()` before
    /// the blacklist was ever consulted, so a ban was inert on exactly the
    /// configuration where it is the *only* control available -- an open relay.
    #[tokio::test]
    async fn a_blacklisted_key_is_refused_even_on_an_open_relay() {
        let (_tmp_dir, database, admin_keys) = setup_test().await;
        let groups = Arc::new(
            Groups::load_groups(
                database.clone(),
                admin_keys.public_key(),
                "wss://test.relay.com".to_string(),
                false,
            )
            .await
            .unwrap(),
        );

        let blacklist = crate::blacklist::Blacklist::new(None);
        let whitelist = Whitelist::new(vec![], None, blacklist.clone());
        assert!(
            whitelist.is_empty(),
            "this test is only meaningful on an open relay"
        );

        let processor = GroupsRelayProcessor::new(groups, admin_keys.public_key(), whitelist);

        let spammer = Keys::generate();
        // Unauthenticated, which is how clients actually connect to an open
        // relay: nothing forces a NIP-42 exchange, so `authed_pubkey` is None
        // and the ban has only the event signature to go on.
        let context = EventContext {
            authed_pubkey: None,
            subdomain: Arc::new(Scope::Default),
            relay_pubkey: admin_keys.public_key(),
        };

        let before = create_test_event(&spammer, 9, vec![Tag::custom(TagKind::h(), ["g"])]).await;
        assert!(
            processor
                .handle_event(before, empty_state(), &context)
                .await
                .is_ok(),
            "an open relay admits anyone to begin with"
        );

        blacklist.add(spammer.public_key());

        let after = create_test_event(&spammer, 9, vec![Tag::custom(TagKind::h(), ["g"])]).await;
        assert!(
            processor
                .handle_event(after, empty_state(), &context)
                .await
                .is_err(),
            "a blacklisted key must be refused however permissive the relay is"
        );

        // And when the client *has* authenticated, the ban must hold there too.
        let authed = EventContext {
            authed_pubkey: Some(spammer.public_key()),
            subdomain: Arc::new(Scope::Default),
            relay_pubkey: admin_keys.public_key(),
        };
        assert!(
            processor
                .verify_filters(
                    &[Filter::new().kind(Kind::Custom(9))],
                    empty_state(),
                    &authed
                )
                .is_err(),
            "a blacklisted key must not be able to read either"
        );
    }

    /// The queue is the scarce resource. Reports group by target, so flooding it
    /// means reporting many *different* things -- which is why this budget is
    /// separate from, and far tighter than, the general publishing one.
    #[tokio::test]
    async fn report_flooding_is_throttled_separately_from_publishing() {
        let (_tmp_dir, database, admin_keys) = setup_test().await;
        let groups = Arc::new(
            Groups::load_groups(
                database.clone(),
                admin_keys.public_key(),
                "wss://test.relay.com".to_string(),
                false,
            )
            .await
            .unwrap(),
        );

        // A generous general budget: this must not be what stops the flood.
        let processor = GroupsRelayProcessor::new(
            groups,
            admin_keys.public_key(),
            Whitelist::new(vec![], None, crate::blacklist::Blacklist::new(None)),
        )
        .with_pubkey_rate_limit(100_000);

        let flooder = Keys::generate();
        let context = EventContext {
            authed_pubkey: Some(flooder.public_key()),
            subdomain: Arc::new(Scope::Default),
            relay_pubkey: admin_keys.public_key(),
        };

        let mut accepted = 0;
        let mut refused = 0;
        // Each report names a distinct target, so grouping does not absorb them.
        for _ in 0..80 {
            let report = create_test_event(
                &flooder,
                1984,
                vec![Tag::parse(["p", &Keys::generate().public_key().to_hex(), "spam"]).unwrap()],
            )
            .await;
            match processor
                .handle_event(report, empty_state(), &context)
                .await
            {
                Ok(_) => accepted += 1,
                Err(_) => refused += 1,
            }
        }

        assert!(
            refused > 0,
            "a report flood must be throttled even when the general budget is huge"
        );
        assert!(
            accepted > 0,
            "and honest reporting must still get through: {accepted} accepted"
        );
    }

    /// Filing one, on the other hand, has to work for anybody admitted --
    /// otherwise the queue is empty by construction.
    #[tokio::test]
    async fn anyone_admitted_can_file_a_report() {
        let (_tmp_dir, database, admin_keys) = setup_test().await;
        let groups = Arc::new(
            Groups::load_groups(
                database.clone(),
                admin_keys.public_key(),
                "wss://test.relay.com".to_string(),
                false,
            )
            .await
            .unwrap(),
        );
        let processor = GroupsRelayProcessor::new(
            groups,
            admin_keys.public_key(),
            Whitelist::new(vec![], None, crate::blacklist::Blacklist::new(None)),
        );

        let reporter = Keys::generate();
        let context = EventContext {
            authed_pubkey: Some(reporter.public_key()),
            subdomain: Arc::new(Scope::Default),
            relay_pubkey: admin_keys.public_key(),
        };

        // No `h` tag: a report is about a person or an event, not a group.
        let report = create_test_event(
            &reporter,
            1984,
            vec![Tag::parse(["p", &Keys::generate().public_key().to_hex(), "spam"]).unwrap()],
        )
        .await;

        assert!(
            processor
                .handle_event(report, empty_state(), &context)
                .await
                .is_ok(),
            "a report without an h tag must be storable"
        );
    }

    /// The console logs in over NIP-46 using this relay as its rendezvous, so
    /// signer traffic has to pass before anyone is authenticated. Without this,
    /// enabling any access restriction locks the operator out of the console
    /// that would let them lift it -- which is precisely what happened when
    /// Web-of-Trust admission was first switched on in production.
    #[tokio::test]
    async fn nip46_signer_traffic_bypasses_a_closed_whitelist() {
        let (_tmp_dir, database, admin_keys) = setup_test().await;
        let groups = Arc::new(
            Groups::load_groups(
                database.clone(),
                admin_keys.public_key(),
                "wss://test.relay.com".to_string(),
                false,
            )
            .await
            .unwrap(),
        );

        // A whitelist with someone on it: closed to everyone else.
        let stranger = Keys::generate();
        let whitelist = Whitelist::new(
            vec![Keys::generate().public_key()],
            None,
            crate::blacklist::Blacklist::new(None),
        );
        assert!(!whitelist.is_empty());
        let processor = GroupsRelayProcessor::new(groups, admin_keys.public_key(), whitelist);

        let context = EventContext {
            authed_pubkey: None,
            subdomain: Arc::new(Scope::Default),
            relay_pubkey: admin_keys.public_key(),
        };

        // Subscribing for signer replies must be allowed unauthenticated.
        let signer_filter = vec![Filter::new().kind(Kind::from(24133u16))];
        assert!(
            processor
                .verify_filters(&signer_filter, empty_state(), &context)
                .is_ok(),
            "a NIP-46 handshake subscription must not require admission"
        );

        // Publishing a signer message likewise.
        let signer_event = create_test_event(&stranger, 24133, vec![]).await;
        assert!(
            processor
                .handle_event(signer_event, empty_state(), &context)
                .await
                .is_ok(),
            "a NIP-46 signer message must not require admission"
        );

        // Everything else stays closed.
        let ordinary = vec![Filter::new().kind(Kind::from(1u16))];
        assert!(
            processor
                .verify_filters(&ordinary, empty_state(), &context)
                .is_err(),
            "the exemption must not open the relay to ordinary traffic"
        );

        let note = create_test_event(&stranger, 1, vec![]).await;
        assert!(
            processor
                .handle_event(note, empty_state(), &context)
                .await
                .is_err(),
            "a non-whitelisted pubkey must still be refused for ordinary events"
        );

        // A filter that merely includes 24133 alongside other kinds is not a
        // handshake and must not inherit the exemption.
        let mixed = vec![Filter::new().kinds(vec![Kind::from(24133u16), Kind::from(1u16)])];
        assert!(
            processor
                .verify_filters(&mixed, empty_state(), &context)
                .is_err(),
            "mixing 24133 with other kinds must not smuggle access"
        );
    }

    #[tokio::test]
    async fn test_groups_relay_logic_non_group_event_visibility() {
        let (_tmp_dir, database, admin_keys) = setup_test().await;
        let groups = Arc::new(
            Groups::load_groups(
                database.clone(),
                admin_keys.public_key(),
                "wss://test.relay.com".to_string(),
                false,
            )
            .await
            .unwrap(),
        );

        let processor = GroupsRelayProcessor::new(
            groups,
            admin_keys.public_key(),
            Whitelist::new(vec![], None, crate::blacklist::Blacklist::new(None)),
        );
        let (_admin_keys, member_keys, _non_member_keys) = create_test_keys().await;

        // Create a non-group event (no 'h' tag)
        let event = create_test_event(&member_keys, 1, vec![]).await;

        let member_pubkey = member_keys.public_key();
        let context = EventContext {
            authed_pubkey: Some(member_pubkey),
            subdomain: Arc::new(Scope::Default),
            relay_pubkey: admin_keys.public_key(),
        };

        // Non-group events should be visible to everyone
        assert!(processor
            .can_see_event(&event, empty_state(), &context)
            .unwrap());
    }

    #[tokio::test]
    async fn test_groups_relay_logic_unmanaged_group_event() {
        let (_tmp_dir, database, admin_keys) = setup_test().await;
        let groups = Arc::new(
            Groups::load_groups(
                database.clone(),
                admin_keys.public_key(),
                "wss://test.relay.com".to_string(),
                false,
            )
            .await
            .unwrap(),
        );

        let processor = GroupsRelayProcessor::new(
            groups,
            admin_keys.public_key(),
            Whitelist::new(vec![], None, crate::blacklist::Blacklist::new(None)),
        );
        let (_admin_keys, member_keys, _non_member_keys) = create_test_keys().await;

        // Create an unmanaged group event (has 'h' tag but group doesn't exist)
        let event = create_test_event(
            &member_keys,
            11, // Group content event
            vec![Tag::custom(TagKind::h(), ["unmanaged_group"])],
        )
        .await;

        let member_pubkey = member_keys.public_key();
        let context = EventContext {
            authed_pubkey: Some(member_pubkey),
            subdomain: Arc::new(Scope::Default),
            relay_pubkey: admin_keys.public_key(),
        };

        // Unmanaged group events should be visible (everyone is considered a member)
        assert!(processor
            .can_see_event(&event, empty_state(), &context)
            .unwrap());

        // Test handle_event for unmanaged group
        let commands = processor
            .handle_event(event.clone(), empty_state(), &context)
            .await
            .unwrap();
        assert_eq!(commands.len(), 1);
        match &commands[0] {
            StoreCommand::SaveSignedEvent(saved_event, scope, _) => {
                assert_eq!(saved_event.id, event.id);
                assert_eq!(*scope, Scope::Default);
            }
            _ => panic!("Expected SaveSignedEvent command"),
        }
    }
}
