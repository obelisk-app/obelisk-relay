pub use crate::group::{
    Group, GroupError, GroupMember, GroupMetadata, GroupRole, Invite, ADDRESSABLE_EVENT_KINDS,
    KIND_GROUP_ADD_USER_9000, KIND_GROUP_ADMINS_39001, KIND_GROUP_CREATE_9007,
    KIND_GROUP_CREATE_INVITE_9009, KIND_GROUP_DELETE_9008, KIND_GROUP_DELETE_EVENT_9005,
    KIND_GROUP_EDIT_METADATA_9002, KIND_GROUP_MEMBERS_39002, KIND_GROUP_METADATA_39000,
    KIND_GROUP_REMOVE_USER_9001, KIND_GROUP_SET_ROLES_9006, KIND_GROUP_USER_JOIN_REQUEST_9021,
    KIND_GROUP_USER_LEAVE_REQUEST_9022, KIND_SIMPLE_LIST_10009, NON_GROUP_ALLOWED_KINDS,
};
use crate::metrics;
use crate::StoreCommand;
use anyhow::Result;
use dashmap::{
    mapref::one::{Ref, RefMut},
    DashMap,
};
use nostr_lmdb::Scope;
use nostr_sdk::prelude::*;
use relay_builder::{Error, RelayDatabase};
use std::collections::{BTreeSet, HashMap};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

// Type aliases to make complex types more manageable
type ScopedGroupKey = (Scope, String);
type ScopedGroupRef<'a> = Ref<'a, ScopedGroupKey, Group>;
type ScopedGroupRefMut<'a> = RefMut<'a, ScopedGroupKey, Group>;

/// Event count for a single kind, as stored, with the bytes those sampled
/// events occupy.
#[derive(Debug, Clone)]
pub struct StorageKindCount {
    pub kind: u16,
    pub count: usize,
    /// Summed `estimated_event_bytes` over the sampled events of this kind.
    /// Divide by `count` for a per-event average that can be scaled against an
    /// exact count; see the Storage screen.
    pub sampled_bytes: u64,
}

/// Fixed per-event storage floor: id (32) + pubkey (32) + signature (64) +
/// created_at (8) + kind (2), plus record framing.
const EVENT_OVERHEAD_BYTES: u64 = 160;

/// Roughly what one event occupies: content, tag payload, and the fixed floor.
///
/// Content alone is not enough. A kind-9000 membership event has empty content
/// and is entirely tags, and a reaction is one byte of content but well over a
/// hundred on disk — attributing by `content.len()` would report the two
/// heaviest structural categories as almost nothing.
///
/// Index entries are deliberately NOT counted, so these figures sum to less
/// than the file on disk. This answers "which kinds are heavy", it is not an
/// accounting of the database.
fn estimated_event_bytes(event: &Event) -> u64 {
    let tags: u64 = event
        .tags
        .iter()
        .map(|tag| {
            tag.as_slice()
                .iter()
                .map(|s| s.len() as u64 + 1)
                .sum::<u64>()
        })
        .sum();
    EVENT_OVERHEAD_BYTES + event.content.len() as u64 + tags
}

/// A gift-wrap recipient and how many wraps in the sample are addressed to them.
#[derive(Debug, Clone)]
pub struct RecipientCount {
    pub pubkey: String,
    pub count: usize,
}

/// Which end of an event a byte total was charged to.
///
/// Kind 1059 is signed by a throwaway key per wrap, so its author says nothing
/// about who is responsible for the traffic — the `p` tag recipient does. Every
/// other kind is charged to its author. The two are not interchangeable and the
/// distinction has to reach the screen, or an operator reading a single "pubkey"
/// column would conclude the recipient of a spam flood was sending it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attribution {
    /// `event.pubkey` — the key that signed it.
    Author,
    /// The `p` tag: who the event is addressed to.
    Recipient,
}

impl Attribution {
    /// Stable identifier for the JSON API and the UI.
    pub fn as_str(self) -> &'static str {
        match self {
            Attribution::Author => "author",
            Attribution::Recipient => "recipient",
        }
    }

    /// How a given kind is attributed.
    pub fn for_kind(kind: u16) -> Self {
        if kind == GIFT_WRAP_KIND {
            Attribution::Recipient
        } else {
            Attribution::Author
        }
    }
}

/// NIP-59 gift wrap. Authored by a one-time key, so attributed by recipient.
pub const GIFT_WRAP_KIND: u16 = 1059;

/// How many events one pubkey accounts for in the sample, and their weight.
#[derive(Debug, Clone)]
pub struct StorageAuthorCount {
    pub pubkey: String,
    pub count: usize,
    pub sampled_bytes: u64,
    pub attributed_by: Attribution,
}

/// The pubkeys behind one kind, heaviest first.
#[derive(Debug, Clone)]
pub struct StorageKindAuthors {
    pub kind: u16,
    /// How every row in this list was attributed — uniform per kind.
    pub attributed_by: Attribution,
    pub authors: Vec<StorageAuthorCount>,
}

/// Rows kept per kind for the drilldown. Past this the tail is noise an
/// operator will not read, and the cached snapshot stops being cheap to hold.
const MAX_AUTHORS_PER_KIND: usize = 25;

/// Rows kept for the relay-wide "events by pubkey" table.
const MAX_TOP_AUTHORS: usize = 50;

/// Tally of one pubkey's events within a sampling pass.
#[derive(Default, Clone, Copy)]
struct Tally {
    count: usize,
    bytes: u64,
}

impl Tally {
    fn add(&mut self, bytes: u64) {
        self.count += 1;
        self.bytes += bytes;
    }
}

/// Collapse a pubkey→tally map into the heaviest `limit` rows.
///
/// Ordered by bytes, not count: the screen exists to explain disk usage, and
/// one 200 KB event matters more than a thousand reactions.
fn top_authors_by_bytes(
    tallies: std::collections::HashMap<String, Tally>,
    attributed_by: Attribution,
    limit: usize,
) -> Vec<StorageAuthorCount> {
    let mut rows: Vec<StorageAuthorCount> = tallies
        .into_iter()
        .map(|(pubkey, tally)| StorageAuthorCount {
            pubkey,
            count: tally.count,
            sampled_bytes: tally.bytes,
            attributed_by,
        })
        .collect();
    rows.sort_by(|a, b| {
        b.sampled_bytes
            .cmp(&a.sampled_bytes)
            .then_with(|| b.count.cmp(&a.count))
    });
    rows.truncate(limit);
    rows
}

/// What the relay currently has on disk, and what pruning would remove.
///
/// The kind breakdown is a newest-first sample; only `prune_preview` is an
/// exact count. See `admin_storage_stats` for why.
#[derive(Debug, Clone)]
pub struct StorageStats {
    /// Busiest gift-wrap recipients within the sample, descending.
    pub top_recipients: Vec<RecipientCount>,
    /// Events actually examined. Equal to `sample_size` when the relay holds
    /// at least that many, otherwise the whole database was sampled.
    pub sampled_events: usize,
    /// The cap that was requested.
    pub sample_size: usize,
    pub kinds: Vec<StorageKindCount>,
    pub newest_event_unix: u64,
    /// Oldest timestamp in the sample — with a full sample this is the oldest
    /// event on the relay; otherwise it bounds how far back the sample reaches.
    pub oldest_sampled_unix: u64,
    pub scope_count: usize,
    /// True when any scope's page came back full, so the sample stopped short
    /// of the whole database. Exact, unlike comparing counts: `limit` is
    /// applied per scope.
    pub sample_truncated: bool,
    /// Exact count of events a prune run would delete right now under the
    /// configured window. Exact because it is the number that decides whether
    /// arming a destructive setting is safe.
    pub prune_preview: usize,
    /// Heaviest pubkeys across every kind — who this relay is storing data for.
    /// Mixed attribution: see [`StorageAuthorCount::attributed_by`].
    pub top_authors: Vec<StorageAuthorCount>,
    /// Per-kind pubkey breakdown, for drilling into a single row of the kinds
    /// table. Same pass, so asking costs nothing extra.
    pub kind_authors: Vec<StorageKindAuthors>,
}

/// Kinds worth counting for the storage screen: NIP-29 group traffic and state,
/// plus the general kinds a groups relay accumulates. Counting is one indexed
/// range per kind, so this list is cheap to extend but not free -- keep it to
/// kinds an operator would actually act on.
#[derive(Debug)]
pub struct Groups {
    db: Arc<RelayDatabase>,
    groups: DashMap<ScopedGroupKey, Group>, // (scope, group_id) -> Group
    pub relay_pubkey: PublicKey,
    pub relay_url: String,
    /// When `true`, every kind 9007 / 9002 has its `private` flag forced to
    /// `false` after `apply_tags`. Set on the public Obelisk relay where
    /// private groups would create read-access scoping the public relay
    /// isn't designed to enforce. Plumbed all the way to the per-call sites
    /// in handle_create_group_event / handle_edit_metadata so existing
    /// loaded groups also get re-coerced on first edit.
    pub force_public_groups: bool,
}

impl Groups {
    pub async fn load_groups(
        database: Arc<RelayDatabase>,
        relay_pubkey: PublicKey,
        relay_url: String,
        force_public_groups: bool,
    ) -> Result<Self, Error> {
        // Get all scopes available in the database
        let scopes = match database.list_scopes().await {
            Ok(s) => s,
            Err(e) => return Err(Error::internal(format!("Failed to list scopes: {e}"))),
        };

        info!("Found {} scopes to load groups from", scopes.len());
        let all_groups = DashMap::new();
        let mut load_failures = Vec::new();

        // Load groups from each scope
        for scope in &scopes {
            match Self::load_groups_for_scope(database.clone(), scope, relay_pubkey).await {
                Ok(scope_groups) => {
                    info!(
                        "Loaded {} groups from scope {:?}",
                        scope_groups.len(),
                        scope
                    );
                    for (group_id, group) in scope_groups {
                        all_groups.insert((scope.clone(), group_id), group);
                    }
                }
                Err(e) => {
                    error!("Failed to load groups for scope {:?}: {}", scope, e);
                    load_failures.push((scope.clone(), e.to_string()));
                }
            }
        }

        // Log summary of load failures if any
        if !load_failures.is_empty() {
            warn!(
                "Failed to load groups for {} scopes: {:?}",
                load_failures.len(),
                load_failures
            );
        }

        // Apply the force-public-groups policy to every loaded group so an
        // older private group is coerced public on relay restart, even if
        // no admin ever publishes a fresh kind 9002 to retire its
        // `["private"]` tag.
        if force_public_groups {
            for mut entry in all_groups.iter_mut() {
                entry.metadata.private = false;
                entry.metadata.hidden = false;
            }
        }

        Ok(Self {
            db: database,
            groups: all_groups,
            relay_pubkey,
            relay_url,
            force_public_groups,
        })
    }

    /// Helper function to load groups for a single scope
    async fn load_groups_for_scope(
        database: Arc<RelayDatabase>,
        scope: &Scope,
        _relay_pubkey: PublicKey,
    ) -> Result<HashMap<String, Group>, Error> {
        info!("Loading groups from scope: {:?}", scope);
        let mut groups = HashMap::new();

        // Step 1: Load current state from replaceable events
        let metadata_filter = vec![Filter::new()
            .kinds(vec![
                KIND_GROUP_METADATA_39000, // 39000
                KIND_GROUP_ADMINS_39001,   // 39001
                KIND_GROUP_MEMBERS_39002,  // 39002
            ])
            .since(Timestamp::from(0))];

        let metadata_events = match database.query(metadata_filter, scope).await {
            Ok(events) => events,
            Err(e) => {
                return Err(Error::notice(format!(
                    "Error querying metadata events for scope {scope:?}: {e}"
                )))
            }
        };

        info!(
            "Found {} metadata events in scope {:?}",
            metadata_events.len(),
            scope
        );

        // Process events in order to build current state
        for event in metadata_events.clone() {
            let group_id = match Group::extract_group_id(&event) {
                Some(id) => id,
                None => {
                    warn!("Group ID not found in event: {:?}", event);
                    continue; // Skip this event instead of failing the entire load
                }
            };

            if event.kind == KIND_GROUP_METADATA_39000 {
                debug!("[{}] Processing metadata in scope {:?}", group_id, scope);
                groups
                    .entry(group_id.to_string())
                    .or_insert_with(|| {
                        let mut g = Group::from(&event);
                        g.scope = scope.clone();
                        g
                    })
                    .load_metadata_from_event(&event)?;
            } else if event.kind == KIND_GROUP_ADMINS_39001
                || event.kind == KIND_GROUP_MEMBERS_39002
            {
                debug!("[{}] Processing members in scope {:?}", group_id, scope);
                groups
                    .entry(group_id.to_string())
                    .or_insert_with(|| {
                        let mut g = Group::from(&event);
                        g.scope = scope.clone();
                        g
                    })
                    .load_members_from_event(&event)?;
            }
        }

        // Step 2: Load historical data for each group
        info!("Processing {} groups in scope {:?}", groups.len(), scope);
        let mut historical_load_errors = Vec::new();

        for (group_id, group) in groups.iter_mut() {
            debug!(
                "[{}] Loading historical data in scope {:?}",
                group_id, scope
            );

            let historical_filter = vec![Filter::new()
                .kinds(vec![
                    KIND_GROUP_CREATE_9007,            // 9007
                    KIND_GROUP_USER_JOIN_REQUEST_9021, // 9021
                    KIND_GROUP_CREATE_INVITE_9009,     // 9009
                ])
                .custom_tag(
                    SingleLetterTag::lowercase(Alphabet::H),
                    group_id.to_string(),
                )
                .since(Timestamp::from(0))];

            match database.query(historical_filter, scope).await {
                Ok(historical_events) => {
                    debug!(
                        "[{}] Found {} historical events in scope {:?}",
                        group_id,
                        historical_events.len(),
                        scope
                    );

                    for event in historical_events {
                        if event.kind == KIND_GROUP_CREATE_9007 {
                            debug!("[{}] Found creation event in scope {:?}", group_id, scope);
                            group.created_at = event.created_at;
                        } else if event.kind == KIND_GROUP_USER_JOIN_REQUEST_9021 {
                            if let Err(e) = group.load_join_request_from_event(&event) {
                                warn!(
                                    "Error loading join request for group {} in scope {:?}: {}",
                                    group_id, scope, e
                                );
                            }
                        } else if event.kind == KIND_GROUP_CREATE_INVITE_9009 {
                            if let Err(e) = group.load_invite_from_event(&event) {
                                warn!(
                                    "Error loading invite for group {} in scope {:?}: {}",
                                    group_id, scope, e
                                );
                            }
                        }
                    }

                    // Update timestamps
                    group.updated_at = metadata_events
                        .iter()
                        .map(|e| e.created_at)
                        .max()
                        .unwrap_or(group.updated_at);
                }
                Err(e) => {
                    warn!(
                        "Error querying historical events for group {} in scope {:?}: {}",
                        group_id, scope, e
                    );
                    historical_load_errors.push((group_id.clone(), e.to_string()));
                    // Continue with the next group
                }
            }
        }

        // Log summary of historical data loading errors if any
        if !historical_load_errors.is_empty() {
            warn!(
                "Failed to load historical data for {} groups in scope {:?}: {:?}",
                historical_load_errors.len(),
                scope,
                historical_load_errors
            );
        }

        Ok(groups)
    }

    // Basic accessor methods
    pub fn get_group(&self, scope: &Scope, group_id: &str) -> Option<ScopedGroupRef<'_>> {
        // Create the key with minimal cloning
        let key = (scope.clone(), group_id.to_string());
        self.groups.get(&key)
    }

    // Nothing - removing backward compatibility method

    pub fn get_group_mut(&self, scope: &Scope, group_id: &str) -> Option<ScopedGroupRefMut<'_>> {
        let key = (scope.clone(), group_id.to_string());
        self.groups.get_mut(&key)
    }

    // Nothing - removing backward compatibility method

    // List groups in a specific scope
    pub fn list_groups_in_scope(&self, scope: &Scope) -> Vec<String> {
        self.groups
            .iter()
            .filter_map(|entry| {
                let (key_scope, group_id) = entry.key();
                if key_scope == scope {
                    Some(group_id.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    // Iterator over all groups (returns clones to avoid holding references)
    pub fn list_all_groups(&self) -> Vec<(Scope, String, Group)> {
        self.groups
            .iter()
            .map(|entry| {
                let (scope, group_id) = entry.key();
                let group = entry.value().clone();
                (scope.clone(), group_id.clone(), group)
            })
            .collect()
    }

    // Get all scopes currently containing groups
    pub fn get_all_scopes(&self) -> std::collections::HashSet<Scope> {
        let mut scopes = std::collections::HashSet::new();
        for entry in self.groups.iter() {
            scopes.insert(entry.key().0.clone());
        }
        scopes
    }

    // More efficient implementation of find_group_in_any_scope
    pub fn find_group_in_any_scope(&self, group_id: &str) -> Option<(Scope, ScopedGroupRef<'_>)> {
        // First find the matching scope (holding minimal locks)
        let mut found_scope = None;

        for entry in self.groups.iter() {
            let (scope, id) = entry.key();
            if id == group_id {
                found_scope = Some(scope.clone());
                break;
            }
        }

        // Second pass to get the actual group reference
        if let Some(scope) = found_scope {
            let key = (scope.clone(), group_id.to_string());
            if let Some(group) = self.groups.get(&key) {
                return Some((scope, group));
            }
        }

        None
    }

    pub fn find_group_from_event<'a>(
        &'a self,
        event: &Event,
        scope: &Scope,
    ) -> Option<Ref<'a, (Scope, String), Group>> {
        let group_id = Group::extract_group_id(event)?;
        self.get_group(scope, group_id)
    }

    // Nothing - removing backward compatibility method

    pub fn find_group_from_event_mut<'a>(
        &'a self,
        event: &Event,
        scope: &Scope,
    ) -> Result<Option<ScopedGroupRefMut<'a>>, Error> {
        let Some(group_id) = Group::extract_group_id(event) else {
            return Ok(None);
        };

        let key = (scope.clone(), group_id.to_string());

        // Check existence first - this acquires a read lock briefly
        // but avoids creating a RefMut that won't be needed
        if !self.groups.contains_key(&key) {
            return Ok(None);
        }

        // Now acquire the write lock for the group
        let mut group_ref_opt = self.groups.get_mut(&key);

        if let Some(ref mut group_ref) = group_ref_opt {
            if event.pubkey != self.relay_pubkey && event.kind != KIND_GROUP_USER_LEAVE_REQUEST_9022
            {
                let verification_result = group_ref.verify_member_access(&event.pubkey, event.kind);
                verification_result?
            }
        }

        Ok(group_ref_opt)
    }

    // Nothing - removing backward compatibility method

    pub fn find_group_from_event_h_tag<'a>(
        &'a self,
        event: &Event,
        scope: &Scope,
    ) -> Option<Ref<'a, (Scope, String), Group>> {
        let group_id = Group::extract_group_h_tag(event)?;
        self.get_group(scope, group_id)
    }

    // Nothing - removing backward compatibility method

    /// Handles group creation events (KIND_GROUP_CREATE_9007).
    /// Creates a group and generates associated metadata and membership events.
    pub async fn handle_group_create(
        &self,
        event: Box<Event>,
        scope: &Scope,
    ) -> Result<Vec<StoreCommand>, Error> {
        let event_id = event.id;
        let Some(group_id) = Group::extract_group_id(&event) else {
            return Err(Error::event_error("Group ID not found in event", event_id));
        };

        // Safety: Query key existence without holding lock
        let key = (scope.clone(), group_id.to_string());

        if self.groups.contains_key(&key) {
            return Err(Error::event_error("Group already exists", event_id));
        }

        // If a group with this id existed (kind 9008), we don't let it be created again
        let deleted_events = match self
            .db
            .query(
                vec![Filter::new()
                    .kinds(vec![KIND_GROUP_DELETE_9008])
                    .custom_tag(
                        SingleLetterTag::lowercase(Alphabet::H),
                        group_id.to_string(),
                    )],
                scope,
            )
            .await
        {
            Ok(events) => events,
            Err(e) => {
                return Err(Error::event_error(
                    format!("Error querying database: {e}"),
                    event_id,
                ))
            }
        };

        if !deleted_events.is_empty() {
            return Err(Error::event_error(
                "Group existed before and was deleted",
                event_id,
            ));
        }

        // Find all previous participants in unmanaged group
        let previous_events = match self
            .db
            .query(
                vec![Filter::new().custom_tag(
                    SingleLetterTag::lowercase(Alphabet::H),
                    group_id.to_string(),
                )],
                scope,
            )
            .await
        {
            Ok(events) => events,
            Err(e) => {
                return Err(Error::event_error(
                    format!("Error querying database: {e}"),
                    event_id,
                ))
            }
        };

        let mut group = Group::new(&event, scope.clone())?;

        // Force-public policy: applied here (after Group::new ran apply_tags
        // on the kind 9007) so a creation event carrying ["private"] still
        // ends up public on relays configured as public-only.
        if self.force_public_groups {
            group.metadata.private = false;
            group.metadata.hidden = false;
        }

        // Only allow migrating unmanaged groups to managed ones if creator is relay admin
        if !previous_events.is_empty() && event.pubkey != self.relay_pubkey {
            return Err(Error::event_error(
                "Only relay admin can create a managed group from an unmanaged one",
                event_id,
            ));
        }

        // Add all previous participants as members
        let mut previous_participants = std::collections::HashSet::new();
        for prev_event in previous_events {
            // Skip any group management events
            if Group::is_group_management_kind(prev_event.kind) {
                continue;
            }
            previous_participants.insert(prev_event.pubkey);
        }

        for pubkey in previous_participants {
            if pubkey != event.pubkey {
                // Skip creator as they're already an admin
                group.add_pubkey(pubkey)?;
            }
        }

        // Now insert the new group with scope
        self.groups.insert(key, group.clone());

        metrics::groups_created().increment(1);

        // Make sure we're using the correct scope for all StoreCommands
        let mut commands = vec![StoreCommand::SaveSignedEvent(event, scope.clone(), None)];
        commands.extend(
            group
                .generate_all_state_events(&self.relay_pubkey, &self.relay_url)?
                .into_iter()
                .map(|e| StoreCommand::SaveUnsignedEvent(e, scope.clone(), None)),
        );

        Ok(commands)
    }

    pub fn handle_set_roles(
        &self,
        event: Box<Event>,
        scope: &Scope,
    ) -> Result<Vec<StoreCommand>, Error> {
        let event_id = event.id;
        let mut group = self
            .find_group_from_event_mut(&event, scope)?
            .ok_or_else(|| Error::event_error("[SetRoles] Group not found", event_id))?;

        // Group now uses the correct scope internally
        group.set_roles(event, &self.relay_pubkey)
    }

    // Nothing - removing backward compatibility method

    pub fn handle_put_user(
        &self,
        event: Box<Event>,
        scope: &Scope,
    ) -> Result<Vec<StoreCommand>, Error> {
        let event_id = event.id;
        let mut group = self
            .find_group_from_event_mut(&event, scope)?
            .ok_or_else(|| Error::event_error("[PutUser] Group not found", event_id))?;

        // Group now uses the correct scope internally
        group.add_members_from_event(event, &self.relay_pubkey)
    }

    // Nothing - removing backward compatibility method

    pub fn handle_remove_user(
        &self,
        event: Box<Event>,
        scope: &Scope,
    ) -> Result<Vec<StoreCommand>, Error> {
        let event_id = event.id;
        let mut group = self
            .find_group_from_event_mut(&event, scope)?
            .ok_or_else(|| Error::event_error("[RemoveUser] Group not found", event_id))?;

        group.remove_members(event, &self.relay_pubkey)
    }

    // Nothing - removing backward compatibility method

    pub fn handle_group_content(
        &self,
        event: Box<Event>,
        scope: &Scope,
    ) -> Result<Vec<StoreCommand>, Error> {
        let event_id = event.id;
        let mut group = self
            .find_group_from_event_mut(&event, scope)?
            .ok_or_else(|| Error::event_error("[GroupManagement] Group not found", event_id))?;

        group.handle_group_content(event, &self.relay_pubkey)
    }

    // Nothing - removing backward compatibility method

    pub fn handle_edit_metadata(
        &self,
        event: Box<Event>,
        scope: &Scope,
    ) -> Result<Vec<StoreCommand>, Error> {
        let event_id = event.id;
        let mut group = self
            .find_group_from_event_mut(&event, scope)?
            .ok_or_else(|| Error::event_error("[EditMetadata] Group not found", event_id))?;

        group.set_metadata(&event, &self.relay_pubkey)?;

        // Force-public policy: re-apply after every edit so a 9002 carrying
        // ["private"] doesn't quietly flip a public-only relay's group back
        // to private.
        if self.force_public_groups {
            group.metadata.private = false;
            group.metadata.hidden = false;
        }

        let scope_clone = scope.clone();
        let mut commands = vec![StoreCommand::SaveSignedEvent(
            event,
            scope_clone.clone(),
            None,
        )];
        commands.extend(
            group
                .generate_metadata_events(&self.relay_pubkey, &self.relay_url)
                .into_iter()
                .map(|e| StoreCommand::SaveUnsignedEvent(e, scope_clone.clone(), None)),
        );

        Ok(commands)
    }

    // Nothing - removing backward compatibility method

    pub fn handle_create_invite(
        &self,
        event: Box<Event>,
        scope: &Scope,
    ) -> Result<Vec<StoreCommand>, Error> {
        let event_id = event.id;
        {
            let mut group = self
                .find_group_from_event_mut(&event, scope)?
                .ok_or_else(|| Error::event_error("[CreateInvite] Group not found", event_id))?;
            group.create_invite(&event, &self.relay_pubkey)?;
        }

        // Regardless of whether the invite was newly created or already existed (created=false),
        // we save the event that attempted the creation, as per NIP-29.
        Ok(vec![StoreCommand::SaveSignedEvent(
            event,
            scope.clone(),
            None,
        )])
    }

    // Nothing - removing backward compatibility method

    pub fn handle_join_request(
        &self,
        event: Box<Event>,
        scope: &Scope,
    ) -> Result<Vec<StoreCommand>, Error> {
        let event_id = event.id;
        let result;
        {
            let mut group = self
                .find_group_from_event_mut(&event, scope)?
                .ok_or_else(|| Error::event_error("[JoinRequest] Group not found", event_id))?;

            result = group.join_request(event, &self.relay_pubkey);
        }

        result
    }

    // Nothing - removing backward compatibility method

    pub fn handle_leave_request(
        &self,
        event: Box<Event>,
        scope: &Scope,
    ) -> Result<Vec<StoreCommand>, Error> {
        let event_id = event.id;
        let mut group = self
            .find_group_from_event_mut(&event, scope)?
            .ok_or_else(|| Error::event_error("[LeaveRequest] Group not found", event_id))?;

        group.leave_request(event, &self.relay_pubkey)
    }

    // Nothing - removing backward compatibility method

    pub fn handle_delete_event(
        &self,
        event: Box<Event>,
        scope: &Scope,
    ) -> Result<Vec<StoreCommand>, Error> {
        let event_id = event.id;
        let mut group = self
            .find_group_from_event_mut(&event, scope)?
            .ok_or_else(|| {
                Error::event_error("Group not found for this group content", event_id)
            })?;

        group.delete_event_request(event, &self.relay_pubkey)
    }

    // Nothing - removing backward compatibility method

    pub fn handle_delete_group(
        &self,
        event: Box<Event>,
        scope: &Scope,
    ) -> Result<Vec<StoreCommand>, Error> {
        let event_id = event.id;
        let group = self
            .find_group_from_event(&event, scope)
            .ok_or_else(|| Error::event_error("[DeleteGroup] Group not found", event_id))?;

        // Extract the group ID
        let group_id = group.key().1.clone();
        let commands = group.delete_group_request(event, &self.relay_pubkey)?;
        drop(group);

        // Remove using the composite key: (scope, group_id)
        let key = (scope.clone(), group_id);
        self.groups.remove(&key);

        Ok(commands)
    }

    /// Admin-only: delete a group and all its events from the database.
    /// Searches all scopes for groups matching the given group ID.
    pub async fn admin_delete_group(&self, group_id: &str) -> Result<(), Error> {
        let keys: Vec<(Scope, String)> = self
            .groups
            .iter()
            .filter(|e| e.key().1 == group_id)
            .map(|e| e.key().clone())
            .collect();

        if keys.is_empty() {
            return Err(Error::notice("Group not found"));
        }

        for (scope, _) in &keys {
            let h_filter = Filter::new().custom_tag(
                SingleLetterTag::lowercase(Alphabet::H),
                group_id.to_string(),
            );
            let d_filter = Filter::new().custom_tag(
                SingleLetterTag::lowercase(Alphabet::D),
                group_id.to_string(),
            );

            self.db
                .delete(h_filter, scope)
                .await
                .map_err(|e| Error::internal(e.to_string()))?;
            self.db
                .delete(d_filter, scope)
                .await
                .map_err(|e| Error::internal(e.to_string()))?;

            self.groups.remove(&(scope.clone(), group_id.to_string()));
        }

        info!(
            "Admin deleted group '{}' ({} scope(s))",
            group_id,
            keys.len()
        );
        Ok(())
    }

    /// Admin-only: get recent events in a group, optionally filtered by author.
    /// Every stored moderation report, grouped into one case per target.
    ///
    /// Enriches each case with what was actually reported: an admin cannot tell
    /// a real problem from a false positive without seeing the message, and
    /// making them go and find it by id is how queues stop getting worked.
    /// Reported events are looked up across scopes because a report carries no
    /// group and the target may live in any of them.
    pub async fn admin_get_reports(
        &self,
        state: &crate::reports::ReportsState,
        limit: usize,
    ) -> Result<Vec<crate::reports::ReportCase>, Error> {
        use crate::reports::{group_into_cases, Report, ReportTarget, KIND_REPORT_1984};

        let filter = Filter::new().kind(KIND_REPORT_1984).limit(limit);

        // Reports have no group, so they land in whichever scope the reporter's
        // connection was on. Sweep every scope this relay knows about, plus the
        // default one, rather than assuming.
        let mut scopes: Vec<Scope> = self.groups.iter().map(|e| e.key().0.clone()).collect();
        scopes.push(Scope::Default);
        scopes.sort_by_key(|s| format!("{s:?}"));
        scopes.dedup_by_key(|s| format!("{s:?}"));

        let mut parsed: Vec<Report> = Vec::new();
        for scope in &scopes {
            let raw = self
                .db
                .query(vec![filter.clone()], scope)
                .await
                .map_err(|e| Error::internal(e.to_string()))?;
            for event in raw {
                parsed.extend(Report::parse(&event));
            }
        }

        let mut cases = group_into_cases(parsed, state);

        // Fill in the reported event's content and the group it belongs to. The
        // group is what decides whether "remove from group" is even offered.
        for case in &mut cases {
            let ReportTarget::Event { id } = &case.target else {
                continue;
            };
            let Ok(event_id) = EventId::from_hex(id) else {
                continue;
            };

            for scope in &scopes {
                let found = self
                    .db
                    .query(vec![Filter::new().id(event_id).limit(1)], scope)
                    .await
                    .map_err(|e| Error::internal(e.to_string()))?;
                if let Some(event) = found.into_iter().next() {
                    // Truncated: the queue is a list, not a reader. Enough to
                    // judge, not so much that one long message buries the rest.
                    case.reported_content = Some(if event.content.chars().count() > 500 {
                        let truncated: String = event.content.chars().take(500).collect();
                        format!("{truncated}…")
                    } else {
                        event.content.clone()
                    });
                    case.reported_pubkey = Some(event.pubkey.to_hex());
                    case.group_id = event
                        .tags
                        .find(TagKind::h())
                        .and_then(|t| t.content())
                        .map(str::to_string);
                    break;
                }
            }
        }

        Ok(cases)
    }

    pub async fn admin_get_group_events(
        &self,
        group_id: &str,
        limit: usize,
        author: Option<&str>,
    ) -> Result<Vec<serde_json::Value>, Error> {
        // Find the scope for this group
        let scope = self
            .groups
            .iter()
            .find(|e| e.key().1 == group_id)
            .map(|e| e.key().0.clone())
            .ok_or_else(|| Error::notice("Group not found"))?;

        let mut filter = Filter::new()
            .custom_tag(
                SingleLetterTag::lowercase(Alphabet::H),
                group_id.to_string(),
            )
            .limit(limit)
            .since(Timestamp::from(0));

        if let Some(pubkey_hex) = author {
            let pubkey = PublicKey::from_hex(pubkey_hex)
                .map_err(|_| Error::notice("Invalid author pubkey"))?;
            filter = filter.author(pubkey);
        }

        let raw = self
            .db
            .query(vec![filter], &scope)
            .await
            .map_err(|e| Error::internal(e.to_string()))?;

        // Collect into a Vec so we can sort
        let mut events: Vec<Event> = raw.into_iter().collect();
        events.sort_by(|a, b| b.created_at.cmp(&a.created_at));

        let result = events
            .into_iter()
            .map(|e| {
                let content = if e.content.len() > 200 {
                    format!("{}…", &e.content[..200])
                } else {
                    e.content.clone()
                };
                serde_json::json!({
                    "id": e.id.to_hex(),
                    "pubkey": e.pubkey.to_hex(),
                    "kind": e.kind.as_u16(),
                    "content": content,
                    "created_at": e.created_at.as_secs(),
                })
            })
            .collect();

        Ok(result)
    }

    /// Admin-only: delete a single event by ID across all scopes.
    /// Admin-only: delete many events in one call.
    ///
    /// Returns `(deleted_id, error)` pairs per input so the caller can report
    /// partial results honestly. Done server-side rather than as N requests
    /// from the browser: one auth check, one pass over the scopes, and it
    /// cannot half-apply because the operator closed the tab.
    pub async fn admin_delete_events(
        &self,
        event_id_hexes: &[String],
    ) -> Vec<(String, Option<String>)> {
        let mut results = Vec::with_capacity(event_id_hexes.len());
        for id in event_id_hexes {
            let outcome = self.admin_delete_event(id).await;
            results.push((id.clone(), outcome.err().map(|e| e.to_string())));
        }
        results
    }

    /// Who wrote a given event, across every scope.
    ///
    /// Needed because a report names an event but the actions an admin might
    /// take -- remove from group, blacklist -- act on a person. Returns None if
    /// the event is gone, which is a real case: someone may already have deleted
    /// it between the report and the review.
    pub async fn admin_find_event_author(
        &self,
        event_id: &EventId,
    ) -> Result<Option<String>, Error> {
        let mut scopes: Vec<Scope> = self.groups.iter().map(|e| e.key().0.clone()).collect();
        scopes.push(Scope::Default);
        scopes.sort_by_key(|s| format!("{s:?}"));
        scopes.dedup_by_key(|s| format!("{s:?}"));

        for scope in &scopes {
            let found = self
                .db
                .query(vec![Filter::new().id(*event_id).limit(1)], scope)
                .await
                .map_err(|e| Error::internal(e.to_string()))?;
            if let Some(event) = found.into_iter().next() {
                return Ok(Some(event.pubkey.to_hex()));
            }
        }
        Ok(None)
    }

    pub async fn admin_delete_event(&self, event_id_hex: &str) -> Result<(), Error> {
        let event_id =
            EventId::from_hex(event_id_hex).map_err(|_| Error::notice("Invalid event ID"))?;

        // Scopes are derived from the managed groups, plus the default one.
        //
        // Without `Scope::Default` this deletes nothing on a relay with no
        // managed groups -- the loop below simply has nothing to iterate -- and
        // still returns Ok, so the caller is told the event was deleted when it
        // is still there. Events in unmanaged groups, and every non-group kind,
        // live in the default scope.
        let mut scopes: Vec<Scope> = self
            .groups
            .iter()
            .map(|e| e.key().0.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        if !scopes.contains(&Scope::Default) {
            scopes.push(Scope::Default);
        }

        for scope in &scopes {
            let filter = Filter::new().id(event_id);
            self.db
                .delete(filter, scope)
                .await
                .map_err(|e| Error::internal(e.to_string()))?;
        }

        info!("Admin deleted event '{}'", event_id_hex);
        Ok(())
    }

    /// Admin-only: storage statistics — what is actually stored, and what a
    /// prune run would delete.
    ///
    /// Deliberately built from indexed `count()` calls only. The public relay's
    /// LMDB is multiple GB, so anything that materialises every event (or even
    /// every id+timestamp) would spike memory on a box that also runs the live
    /// relays. Every number below comes from a counted index range.
    /// Every pubkey that has signed an addressable group-state event.
    ///
    /// Group state is written by the relay, but a relay that rotates its
    /// identity does not re-sign what it wrote under the old key — this one
    /// rotated on 2026-08-11 and four distinct pubkeys hold live group state as
    /// a result. A client discovering groups therefore cannot guess the author
    /// set, and without an author the query cannot use an index at all (see
    /// `crate::group_state_filter`). This is the scan that produces the set,
    /// run once at startup so every later discovery query is indexed.
    pub async fn group_state_authors(&self) -> Result<BTreeSet<PublicKey>, Error> {
        let scopes = self
            .db
            .list_scopes()
            .await
            .map_err(|e| Error::internal(e.to_string()))?;

        let mut authors = BTreeSet::new();
        for scope in &scopes {
            let events = self
                .db
                .query(
                    vec![Filter::new().kinds(ADDRESSABLE_EVENT_KINDS.iter().copied())],
                    scope,
                )
                .await
                .map_err(|e| Error::internal(e.to_string()))?;
            for event in events {
                authors.insert(event.pubkey);
            }
        }

        Ok(authors)
    }

    /// Exact count for a single kind, and how many of those are older than a
    /// window.
    ///
    /// One indexed range per figure. Measured at roughly 12s per kind on the
    /// 4.2 GB production database, which is why the storage overview samples
    /// instead and this is only ever run for one kind on request.
    pub async fn admin_exact_kind_count(
        &self,
        kind: u16,
        older_than_days: Option<u32>,
    ) -> Result<(u64, Option<u64>), Error> {
        let scopes = self
            .db
            .list_scopes()
            .await
            .map_err(|e| Error::internal(e.to_string()))?;

        let started = std::time::Instant::now();
        let mut total = 0u64;
        let mut older = 0u64;

        for scope in &scopes {
            total += self
                .db
                .count(vec![Filter::new().kind(Kind::from(kind))], scope)
                .await
                .map_err(|e| Error::internal(e.to_string()))? as u64;

            if let Some(days) = older_than_days {
                if days > 0 {
                    let cutoff = Timestamp::now() - (days as u64) * 86_400;
                    older += self
                        .db
                        .count(
                            vec![Filter::new().kind(Kind::from(kind)).until(cutoff)],
                            scope,
                        )
                        .await
                        .map_err(|e| Error::internal(e.to_string()))?
                        as u64;
                }
            }
        }

        info!(
            "Exact count for kind {}: {} events in {:?}",
            kind,
            total,
            started.elapsed()
        );

        Ok((total, older_than_days.map(|_| older)))
    }

    pub async fn admin_storage_stats(
        &self,
        sample_size: usize,
        prune_kinds: &[u16],
        retention_days: u32,
    ) -> Result<StorageStats, Error> {
        let scopes = self
            .db
            .list_scopes()
            .await
            .map_err(|e| Error::internal(e.to_string()))?;

        let started = std::time::Instant::now();

        // The kind breakdown is SAMPLED, not exhaustive.
        //
        // Exact per-kind totals were measured first and are not viable here:
        // `count()` on the 4.2 GB production database costs >12s per kind, so
        // a 47-kind sweep ran past ten minutes, and `count(Filter::new())` has
        // no index to walk at all and degrades into a full scan. Neither is
        // acceptable work to repeat on a box that is also serving live relays.
        //
        // A bounded newest-first sample answers the question an operator is
        // actually asking -- what is filling this relay now -- in one indexed
        // range read, and the response labels it as a sample so the numbers
        // are never mistaken for totals.
        let mut tally: HashMap<u16, usize> = HashMap::new();
        // Recipients of gift wraps, tallied from the same pass. Their authors are
        // one-time keys, so the `p` tag is the only way to attribute that traffic
        // to anyone -- and on this relay it is the bulk of stored data.
        let mut recipients: HashMap<String, usize> = HashMap::new();
        // Who the stored data belongs to, relay-wide and per kind. The kinds
        // table says *what* is filling the disk; without these an operator can
        // see the symptom but cannot aim the blacklist at anyone.
        let mut authors: HashMap<String, Tally> = HashMap::new();
        let mut authors_by_kind: HashMap<u16, HashMap<String, Tally>> = HashMap::new();
        let mut sampled = 0usize;
        let mut newest_event_unix = 0u64;
        let mut oldest_sampled_unix = 0u64;
        let mut bytes: HashMap<u16, u64> = HashMap::new();
        // Set when any scope returns a full page, which is the only exact way
        // to know the sample was truncated: `limit` applies per scope, so
        // comparing sampled_events against sample_size is wrong once there is
        // more than one scope.
        let mut sample_truncated = false;

        for scope in &scopes {
            let events = self
                .db
                .query(vec![Filter::new().limit(sample_size)], scope)
                .await
                .map_err(|e| Error::internal(e.to_string()))?;
            if events.len() >= sample_size {
                sample_truncated = true;
            }
            for event in events {
                let ts = event.created_at.as_secs();
                newest_event_unix = newest_event_unix.max(ts);
                oldest_sampled_unix = if oldest_sampled_unix == 0 {
                    ts
                } else {
                    oldest_sampled_unix.min(ts)
                };
                let kind = event.kind.as_u16();
                let event_bytes = estimated_event_bytes(&event);
                *tally.entry(kind).or_insert(0) += 1;
                *bytes.entry(kind).or_insert(0) += event_bytes;

                // Charge the event to whoever it is actually attributable to:
                // the `p` tag for gift wraps, the signer for everything else.
                let responsible = match Attribution::for_kind(kind) {
                    Attribution::Recipient => event
                        .tags
                        .iter()
                        .find(|tag| tag.kind() == TagKind::p())
                        .and_then(|tag| tag.content())
                        .map(str::to_string),
                    Attribution::Author => Some(event.pubkey.to_hex()),
                };
                if let Some(pubkey) = responsible {
                    if kind == GIFT_WRAP_KIND {
                        *recipients.entry(pubkey.clone()).or_insert(0) += 1;
                    }
                    authors.entry(pubkey.clone()).or_default().add(event_bytes);
                    authors_by_kind
                        .entry(kind)
                        .or_default()
                        .entry(pubkey)
                        .or_default()
                        .add(event_bytes);
                }
                sampled += 1;
            }
        }

        let mut kinds: Vec<StorageKindCount> = tally
            .into_iter()
            .map(|(kind, count)| StorageKindCount {
                kind,
                count,
                sampled_bytes: bytes.get(&kind).copied().unwrap_or(0),
            })
            .collect();
        kinds.sort_by(|a, b| b.count.cmp(&a.count));
        info!(
            "Storage stats: sampled {} events across {} kinds in {:?}",
            sampled,
            kinds.len(),
            started.elapsed()
        );

        // What a prune run would actually delete right now, under the currently
        // configured window and kinds. This is the number that matters before
        // arming a destructive setting -- far more use than an age range.
        let mut prune_preview = 0usize;
        if retention_days > 0 && !prune_kinds.is_empty() {
            let cutoff = Timestamp::now() - (retention_days as u64) * 86_400;
            for kind in prune_kinds {
                for scope in &scopes {
                    prune_preview += self
                        .db
                        .count(
                            vec![Filter::new().kind(Kind::from(*kind)).until(cutoff)],
                            scope,
                        )
                        .await
                        .map_err(|e| Error::internal(e.to_string()))?;
                }
            }
        }

        info!(
            "Storage stats: complete in {:?} ({} sampled across {} scopes, prune preview {})",
            started.elapsed(),
            sampled,
            scopes.len(),
            prune_preview
        );

        // Only the head of the distribution is useful; a full recipient list on a
        // busy relay is thousands of rows the operator will not read.
        let mut top_recipients: Vec<RecipientCount> = recipients
            .into_iter()
            .map(|(pubkey, count)| RecipientCount { pubkey, count })
            .collect();
        top_recipients.sort_by(|a, b| b.count.cmp(&a.count));
        top_recipients.truncate(10);

        // Relay-wide attribution. Mixed by construction: gift-wrap rows name a
        // recipient, everything else names a signer.
        let mut top_authors: Vec<StorageAuthorCount> = authors
            .into_iter()
            .map(|(pubkey, tally)| StorageAuthorCount {
                pubkey,
                count: tally.count,
                sampled_bytes: tally.bytes,
                // A pubkey can appear under both rules across different kinds.
                // Label the row by whichever produced most of its bytes, and
                // let the per-kind drilldown disambiguate.
                attributed_by: Attribution::Author,
            })
            .collect();
        // Recompute the label from the per-kind data rather than guessing:
        // a pubkey whose bytes are mostly gift wraps is a recipient.
        for row in &mut top_authors {
            let wrap_bytes = authors_by_kind
                .get(&GIFT_WRAP_KIND)
                .and_then(|m| m.get(&row.pubkey))
                .map(|t| t.bytes)
                .unwrap_or(0);
            if wrap_bytes * 2 > row.sampled_bytes {
                row.attributed_by = Attribution::Recipient;
            }
        }
        top_authors.sort_by(|a, b| {
            b.sampled_bytes
                .cmp(&a.sampled_bytes)
                .then_with(|| b.count.cmp(&a.count))
        });
        top_authors.truncate(MAX_TOP_AUTHORS);

        let mut kind_authors: Vec<StorageKindAuthors> = authors_by_kind
            .into_iter()
            .map(|(kind, tallies)| {
                let attributed_by = Attribution::for_kind(kind);
                StorageKindAuthors {
                    kind,
                    attributed_by,
                    authors: top_authors_by_bytes(tallies, attributed_by, MAX_AUTHORS_PER_KIND),
                }
            })
            .collect();
        kind_authors.sort_by_key(|k| k.kind);

        Ok(StorageStats {
            top_recipients,
            sampled_events: sampled,
            sample_size,
            kinds,
            newest_event_unix,
            oldest_sampled_unix,
            scope_count: scopes.len(),
            sample_truncated,
            prune_preview,
            top_authors,
            kind_authors,
        })
    }

    /// Admin-only: list members of a group with their roles.
    pub fn admin_get_group_members(&self, group_id: &str) -> Result<Vec<serde_json::Value>, Error> {
        let entry = self
            .groups
            .iter()
            .find(|e| e.key().1 == group_id)
            .ok_or_else(|| Error::notice("Group not found"))?;

        let result = entry
            .value()
            .members
            .values()
            .map(|m| {
                let roles: Vec<String> = m.roles.iter().map(|r| r.to_string()).collect();
                serde_json::json!({
                    "pubkey": m.pubkey.to_hex(),
                    "roles": roles,
                })
            })
            .collect();

        Ok(result)
    }

    /// Admin-only: delete events *addressed to* a pubkey via their `p` tag.
    ///
    /// Exists because NIP-59 gift wraps (kind 1059) are signed by a fresh throwaway
    /// key per wrap, so author-based moderation cannot touch them at all. The `p`
    /// tag is the only stable handle a relay operator has on that traffic.
    ///
    /// `kinds` is required rather than optional: "everything addressed to this
    /// person" would sweep up group management events that merely mention them
    /// (9000 add-user carries a `p` tag), and those are exactly what must survive.
    /// Protected kinds are filtered out regardless.
    ///
    /// Deleting gift wraps destroys the messages themselves — the relay holds no
    /// other copy and clients fetch DM history from it.
    pub async fn admin_delete_events_by_recipient(
        &self,
        pubkey_hex: &str,
        kinds: &[u16],
    ) -> Result<u64, Error> {
        let pubkey =
            PublicKey::from_hex(pubkey_hex).map_err(|_| Error::notice("Invalid pubkey"))?;

        let target_kinds: Vec<Kind> = kinds
            .iter()
            .filter(|k| !crate::pruner::NEVER_PRUNE_KINDS.contains(k))
            .map(|k| Kind::from(*k))
            .collect();

        if target_kinds.is_empty() {
            return Ok(0);
        }

        let scopes = self
            .db
            .list_scopes()
            .await
            .map_err(|e| Error::internal(e.to_string()))?;

        let mut deleted_total = 0u64;
        for scope in &scopes {
            let filter = Filter::new()
                .kinds(target_kinds.iter().copied())
                .custom_tag(SingleLetterTag::lowercase(Alphabet::P), pubkey.to_hex());

            let count = self
                .db
                .count(vec![filter.clone()], scope)
                .await
                .map_err(|e| Error::internal(e.to_string()))? as u64;

            if count == 0 {
                continue;
            }

            self.db
                .delete(filter, scope)
                .await
                .map_err(|e| Error::internal(e.to_string()))?;
            deleted_total = deleted_total.saturating_add(count);
        }

        info!(
            "Admin deleted {} events addressed to '{}' (kinds {:?})",
            deleted_total, pubkey_hex, kinds
        );
        Ok(deleted_total)
    }

    /// Which end of an event a targeted prune matches on.
    ///
    /// Gift wraps are signed by a throwaway key per message, so "delete this
    /// person's gift wraps" can only mean the `p` tag. Every other kind means
    /// the author. Getting this wrong deletes a bystander's data.
    ///
    /// Count and delete run off the *same* filter, so the number an operator
    /// confirms is the number that is removed.
    pub async fn admin_prune_events(
        &self,
        pubkey_hex: &str,
        by: Attribution,
        kinds: &[u16],
        since: Option<u64>,
        until: Option<u64>,
        dry_run: bool,
    ) -> Result<u64, Error> {
        let pubkey =
            PublicKey::from_hex(pubkey_hex).map_err(|_| Error::notice("Invalid pubkey"))?;

        // Kinds are required, not optional. "Everything addressed to this
        // person" would sweep group management events that merely mention them,
        // and "everything they authored" within a date range is still broad
        // enough that it should be stated rather than defaulted.
        let target_kinds: Vec<Kind> = kinds
            .iter()
            .filter(|k| !crate::pruner::NEVER_PRUNE_KINDS.contains(k))
            .map(|k| Kind::from(*k))
            .collect();

        if target_kinds.is_empty() {
            return Ok(0);
        }

        let scopes = self
            .db
            .list_scopes()
            .await
            .map_err(|e| Error::internal(e.to_string()))?;

        let mut total = 0u64;
        for scope in &scopes {
            let mut filter = Filter::new().kinds(target_kinds.iter().copied());
            filter = match by {
                Attribution::Author => filter.author(pubkey),
                Attribution::Recipient => {
                    filter.custom_tag(SingleLetterTag::lowercase(Alphabet::P), pubkey.to_hex())
                }
            };
            if let Some(since) = since {
                filter = filter.since(Timestamp::from_secs(since));
            }
            if let Some(until) = until {
                filter = filter.until(Timestamp::from_secs(until));
            }

            let count = self
                .db
                .count(vec![filter.clone()], scope)
                .await
                .map_err(|e| Error::internal(e.to_string()))? as u64;

            if count == 0 {
                continue;
            }
            total = total.saturating_add(count);

            if !dry_run {
                self.db
                    .delete(filter, scope)
                    .await
                    .map_err(|e| Error::internal(e.to_string()))?;
            }
        }

        if dry_run {
            debug!(
                "Prune preview for '{}' ({:?}, kinds {:?}): {} event(s) would be deleted",
                pubkey_hex, by, kinds, total
            );
        } else {
            info!(
                "Admin pruned {} events for '{}' ({:?}, kinds {:?}, since {:?}, until {:?})",
                total, pubkey_hex, by, kinds, since, until
            );
        }
        Ok(total)
    }

    /// Admin-only: delete events authored by a pubkey across all scopes.
    ///
    /// Never deletes [`crate::pruner::NEVER_PRUNE_KINDS`]. This used to wipe with a
    /// bare `Filter::author(pubkey)`, which meant wiping whoever created a group also
    /// deleted its 9007/9000/9002 events and left the group orphaned — the exact
    /// outcome the pruner has always refused to cause. Both paths now consult the
    /// same constant so they cannot drift apart.
    ///
    /// Pass `kinds` to restrict further; `None` means "everything this author wrote,
    /// minus the protected kinds". Returns the number of events deleted.
    pub async fn admin_delete_user_events(
        &self,
        pubkey_hex: &str,
        kinds: Option<&[u16]>,
    ) -> Result<u64, Error> {
        let pubkey =
            PublicKey::from_hex(pubkey_hex).map_err(|_| Error::notice("Invalid pubkey"))?;

        let scopes: Vec<Scope> = self
            .groups
            .iter()
            .map(|e| e.key().0.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        let mut deleted_total = 0u64;

        for scope in &scopes {
            // A nostr filter can only express kind *inclusion*, so "everything except
            // the protected kinds" cannot be stated directly. Discover which kinds
            // this author actually used, subtract the protected set, and delete that
            // explicit list — which keeps the delete itself index-bounded.
            let target_kinds: Vec<Kind> = match kinds {
                Some(requested) => requested
                    .iter()
                    .filter(|k| !crate::pruner::NEVER_PRUNE_KINDS.contains(k))
                    .map(|k| Kind::from(*k))
                    .collect(),
                None => self
                    .author_kinds_in_scope(pubkey, scope)
                    .await?
                    .into_iter()
                    .filter(|k| !crate::pruner::NEVER_PRUNE_KINDS.contains(&k.as_u16()))
                    .collect(),
            };

            if target_kinds.is_empty() {
                continue;
            }

            let filter = Filter::new()
                .author(pubkey)
                .kinds(target_kinds.iter().copied());

            let count = self
                .db
                .count(vec![filter.clone()], scope)
                .await
                .map_err(|e| Error::internal(e.to_string()))? as u64;

            if count == 0 {
                continue;
            }

            self.db
                .delete(filter, scope)
                .await
                .map_err(|e| Error::internal(e.to_string()))?;
            deleted_total = deleted_total.saturating_add(count);
        }

        info!(
            "Admin deleted {} events authored by '{}' (protected kinds retained)",
            deleted_total, pubkey_hex
        );
        Ok(deleted_total)
    }

    /// Distinct kinds an author has stored in a scope.
    ///
    /// Pages newest-first rather than taking a single window: missing a kind here
    /// would silently leave those events behind on a wipe.
    async fn author_kinds_in_scope(
        &self,
        pubkey: PublicKey,
        scope: &Scope,
    ) -> Result<Vec<Kind>, Error> {
        const PAGE: usize = 500;
        // Bounds the work for a pathological author; 200 pages = 100k events.
        const MAX_PAGES: usize = 200;

        let mut kinds: std::collections::HashSet<Kind> = std::collections::HashSet::new();
        let mut until: Option<Timestamp> = None;

        for page in 0..MAX_PAGES {
            let mut filter = Filter::new().author(pubkey).limit(PAGE);
            if let Some(ts) = until {
                filter = filter.until(ts);
            }

            let events = self
                .db
                .query(vec![filter], scope)
                .await
                .map_err(|e| Error::internal(e.to_string()))?;

            let mut oldest: Option<Timestamp> = None;
            let mut seen = 0usize;
            for event in events {
                kinds.insert(event.kind);
                oldest = Some(match oldest {
                    Some(o) if o <= event.created_at => o,
                    _ => event.created_at,
                });
                seen += 1;
            }

            if seen < PAGE {
                break;
            }
            // Step strictly past the oldest seen, otherwise a page full of
            // identical timestamps would loop forever.
            match oldest {
                Some(ts) if ts.as_secs() > 0 => until = Some(Timestamp::from(ts.as_secs() - 1)),
                _ => break,
            }
            if page == MAX_PAGES - 1 {
                warn!(
                    "Author kind discovery hit the page cap for {}; some kinds may be missed",
                    pubkey.to_hex()
                );
            }
        }

        Ok(kinds.into_iter().collect())
    }

    /// Admin-only: remove a member from a group.
    pub async fn admin_remove_group_member(
        &self,
        group_id: &str,
        pubkey_hex: &str,
    ) -> Result<(), Error> {
        let pubkey =
            PublicKey::from_hex(pubkey_hex).map_err(|_| Error::notice("Invalid pubkey"))?;

        // Find key
        let key = self
            .groups
            .iter()
            .find(|e| e.key().1 == group_id)
            .map(|e| e.key().clone())
            .ok_or_else(|| Error::notice("Group not found"))?;

        {
            let mut group = self
                .groups
                .get_mut(&key)
                .ok_or_else(|| Error::notice("Group not found"))?;

            if !group.members.contains_key(&pubkey) {
                return Err(Error::notice("Member not found"));
            }
            group.members.remove(&pubkey);
        }

        let scope = &key.0;

        // Remove add-user events for this member from DB
        let filter = Filter::new()
            .kind(KIND_GROUP_ADD_USER_9000)
            .custom_tag(
                SingleLetterTag::lowercase(Alphabet::H),
                group_id.to_string(),
            )
            .custom_tag(
                SingleLetterTag::lowercase(Alphabet::P),
                pubkey_hex.to_string(),
            );
        self.db
            .delete(filter, scope)
            .await
            .map_err(|e| Error::internal(e.to_string()))?;

        info!(
            "Admin removed member '{}' from group '{}'",
            pubkey_hex, group_id
        );
        Ok(())
    }

    /// Returns counts of groups by their privacy settings for all scopes
    pub fn count_groups_by_privacy(&self) -> [(bool, bool, usize); 4] {
        let mut counts = [
            (false, false, 0),
            (false, true, 0),
            (true, false, 0),
            (true, true, 0),
        ];

        for group in self.iter() {
            let group = group.value();
            let idx = match (group.metadata.private, group.metadata.closed) {
                (false, false) => 0,
                (false, true) => 1,
                (true, false) => 2,
                (true, true) => 3,
            };
            counts[idx].2 += 1;
        }

        counts
    }

    /// Returns counts of groups by their privacy settings for a specific scope
    pub fn count_groups_by_privacy_in_scope(&self, scope: &Scope) -> [(bool, bool, usize); 4] {
        let mut counts = [
            (false, false, 0),
            (false, true, 0),
            (true, false, 0),
            (true, true, 0),
        ];

        for entry in self.iter() {
            let (key_scope, _) = entry.key();
            let group = entry.value();

            // Only count groups in the specified scope
            if key_scope != scope {
                continue;
            }

            let idx = match (group.metadata.private, group.metadata.closed) {
                (false, false) => 0,
                (false, true) => 1,
                (true, false) => 2,
                (true, true) => 3,
            };
            counts[idx].2 += 1;
        }

        counts
    }

    /// Verifies if a user has access to a group
    /// Returns Ok(()) if access is allowed, or an appropriate error if not
    pub fn verify_group_access(
        &self,
        group: &Group,
        pubkey: Option<PublicKey>,
    ) -> Result<(), GroupError> {
        if !group.metadata.private {
            return Ok(());
        }

        let pubkey = pubkey.ok_or_else(|| {
            GroupError::PermissionDenied("Authentication required for private group".to_string())
        })?;

        if pubkey == self.relay_pubkey || group.is_member(&pubkey) {
            Ok(())
        } else {
            Err(GroupError::PermissionDenied(
                "Not a member of this private group".to_string(),
            ))
        }
    }

    /// Verifies if a user has access to a group by ID and scope
    /// Returns Ok(()) if access is allowed, or an appropriate error if not
    pub fn verify_group_access_by_id(
        &self,
        scope: &Scope,
        group_id: &str,
        pubkey: Option<PublicKey>,
    ) -> Result<(), GroupError> {
        let group = self.get_group(scope, group_id).ok_or_else(|| {
            GroupError::NotFound(format!("Group {group_id} not found in scope {scope:?}"))
        })?;
        // Get the value from the Ref, not the reference to Ref
        let group_value = group.value();
        self.verify_group_access(group_value, pubkey)
    }

    // Nothing - removing backward compatibility method
}

impl Deref for Groups {
    type Target = DashMap<(Scope, String), Group>;

    fn deref(&self) -> &Self::Target {
        &self.groups
    }
}

impl DerefMut for Groups {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.groups
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay_builder::RelayDatabase;
    use std::time::Instant;
    use tempfile::TempDir;

    const TEST_GROUP_ID: &str = "test_group_123";

    async fn create_test_keys() -> (Keys, Keys, Keys) {
        (Keys::generate(), Keys::generate(), Keys::generate())
    }

    async fn create_test_event(keys: &Keys, kind: Kind, tags: Vec<Tag>) -> Box<Event> {
        let unsigned_event = EventBuilder::new(kind, "")
            .tags(tags)
            .build_with_ctx(&Instant::now(), keys.public_key());
        let event = keys.sign_event(unsigned_event).await.unwrap();
        Box::new(event)
    }

    async fn create_test_groups_with_db(admin_keys: &Keys) -> Groups {
        let temp_dir = TempDir::new().unwrap();
        let db = RelayDatabase::new(
            temp_dir
                .path()
                .join("test.db")
                .to_string_lossy()
                .to_string(),
        )
        .await
        .unwrap();

        std::mem::forget(temp_dir);

        Groups {
            db: Arc::new(db),
            groups: DashMap::new(),
            relay_pubkey: admin_keys.public_key(),
            relay_url: "wss://test.relay.url".to_string(),
            force_public_groups: false,
        }
    }

    async fn setup_test_groups() -> (Groups, Keys, Keys, Keys, String, Scope) {
        let (relay_keys, user_keys, member_keys) = create_test_keys().await;
        let tags = vec![Tag::custom(TagKind::h(), [TEST_GROUP_ID])];
        // User creates the group (not the relay)
        let event = create_test_event(&user_keys, KIND_GROUP_CREATE_9007, tags).await;

        // Relay pubkey is relay_keys
        let groups = create_test_groups_with_db(&relay_keys).await;
        let scope = Scope::Default;
        groups.handle_group_create(event, &scope).await.unwrap();

        // Return: groups, user_keys (acts as admin), member_keys, relay_keys (non_member), group_id, scope
        (
            groups,
            user_keys,   // This user created the group and is admin
            member_keys, // Regular member key for tests
            relay_keys,  // The relay (should never be in group)
            TEST_GROUP_ID.to_string(),
            scope,
        )
    }

    #[tokio::test]
    async fn test_handle_group_create_sets_admin() {
        let (groups, admin_keys, _, _, group_id, scope) = setup_test_groups().await;

        // Verify group exists and admin is set
        let group = groups.get_group(&scope, &group_id).unwrap();
        assert!(group.value().is_admin(&admin_keys.public_key()));
    }

    #[tokio::test]
    async fn test_handle_group_create_relay_never_member() {
        let (admin_keys, user_keys, _) = create_test_keys().await;
        let groups = create_test_groups_with_db(&admin_keys).await;
        let scope = Scope::Default;
        let relay_pubkey = admin_keys.public_key(); // In tests, admin_keys acts as relay

        // User creates a group
        let group_id = "test_relay_not_member";
        let create_event = create_test_event(
            &user_keys,
            KIND_GROUP_CREATE_9007,
            vec![Tag::custom(TagKind::h(), [group_id])],
        )
        .await;

        let commands = groups
            .handle_group_create(create_event, &scope)
            .await
            .unwrap();

        // Verify the generated events
        let mut found_user_9000 = false;
        let mut found_relay_9000 = false;
        let mut user_in_39001 = false;
        let mut relay_in_39001 = false;
        let mut user_in_39002 = false;
        let mut relay_in_39002 = false;

        for cmd in &commands {
            match cmd {
                StoreCommand::SaveUnsignedEvent(event, _, _) => {
                    // Check kind:9000 events
                    if event.kind == KIND_GROUP_ADD_USER_9000 {
                        // Find who is being added
                        if let Some(p_tag) = event.tags.iter().find(|t| t.kind() == TagKind::p()) {
                            if let Some(pubkey_str) = p_tag.content() {
                                if pubkey_str == user_keys.public_key().to_string() {
                                    found_user_9000 = true;
                                }
                                if pubkey_str == relay_pubkey.to_string() {
                                    found_relay_9000 = true;
                                }
                            }
                        }
                    }

                    // Check kind:39001 (admins)
                    if event.kind == KIND_GROUP_ADMINS_39001 {
                        for tag in event.tags.iter() {
                            if tag.kind() == TagKind::p() {
                                if let Some(pubkey_str) = tag.content() {
                                    if pubkey_str == user_keys.public_key().to_string() {
                                        user_in_39001 = true;
                                    }
                                    if pubkey_str == relay_pubkey.to_string() {
                                        relay_in_39001 = true;
                                    }
                                }
                            }
                        }
                    }

                    // Check kind:39002 (members)
                    if event.kind == KIND_GROUP_MEMBERS_39002 {
                        for tag in event.tags.iter() {
                            if tag.kind() == TagKind::p() {
                                if let Some(pubkey_str) = tag.content() {
                                    if pubkey_str == user_keys.public_key().to_string() {
                                        user_in_39002 = true;
                                    }
                                    if pubkey_str == relay_pubkey.to_string() {
                                        relay_in_39002 = true;
                                    }
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }

        // Assertions per NIP-29:
        // 1. User should have a kind:9000 establishing their membership
        assert!(
            found_user_9000,
            "User should have a kind:9000 event in moderation history"
        );

        // 2. Relay should NEVER have a kind:9000
        assert!(
            !found_relay_9000,
            "Relay should NEVER be added as a member via kind:9000"
        );

        // 3. User should be in kind:39001 (admins list)
        assert!(
            user_in_39001,
            "User should be in the admins list (kind:39001)"
        );

        // 4. Relay should NEVER be in kind:39001
        assert!(
            !relay_in_39001,
            "Relay should NEVER be in the admins list (kind:39001)"
        );

        // 5. User should be in kind:39002 (members list)
        assert!(
            user_in_39002,
            "User should be in the members list (kind:39002)"
        );

        // 6. Relay should NEVER be in kind:39002
        assert!(
            !relay_in_39002,
            "Relay should NEVER be in the members list (kind:39002)"
        );

        // Verify in-memory state as well
        let group = groups.get_group(&scope, group_id).unwrap();
        assert!(
            group.value().is_admin(&user_keys.public_key()),
            "User should be admin in memory"
        );
        assert!(
            !group.value().is_member(&relay_pubkey),
            "Relay should NOT be a member in memory"
        );
    }

    #[tokio::test]
    async fn test_handle_group_create_rejects_duplicate_group() {
        let (groups, admin_keys, _, _, group_id, scope) = setup_test_groups().await;

        // Test creating a duplicate group
        let tags = vec![Tag::custom(TagKind::h(), [&group_id])];
        let event = create_test_event(&admin_keys, KIND_GROUP_CREATE_9007, tags).await;
        assert!(groups.handle_group_create(event, &scope).await.is_err());
    }

    #[tokio::test]
    async fn test_handle_group_create_generates_state_events() {
        let (admin_keys, _, _) = create_test_keys().await;
        let groups = create_test_groups_with_db(&admin_keys).await;
        let scope = Scope::Default;

        // Create group
        let create_event = create_test_event(
            &admin_keys,
            KIND_GROUP_CREATE_9007,
            vec![Tag::custom(TagKind::h(), ["statetest123"])],
        )
        .await;

        let result = groups.handle_group_create(create_event, &scope).await;
        assert!(result.is_ok(), "handle_group_create should succeed");

        let commands = result.unwrap();
        println!("Generated {} store commands", commands.len());

        // Should generate multiple commands
        assert!(
            commands.len() >= 4,
            "Should generate at least 4 commands (1 signed + 3 unsigned state events)"
        );

        // Check for specific event kinds
        let mut has_39000 = false;
        let mut has_39001 = false;
        let mut has_39002 = false;

        for cmd in &commands {
            if let StoreCommand::SaveUnsignedEvent(evt, _, _) = cmd {
                println!("  SaveUnsignedEvent: kind={}", evt.kind);
                match evt.kind.as_u16() {
                    39000 => has_39000 = true,
                    39001 => has_39001 = true,
                    39002 => has_39002 = true,
                    _ => {}
                }
            }
        }

        assert!(has_39000, "Should generate 39000 (metadata) event");
        assert!(has_39001, "Should generate 39001 (admins) event");
        assert!(has_39002, "Should generate 39002 (members) event");
    }

    #[tokio::test]
    async fn test_handle_set_roles_admin_can_promote_member() {
        let (relay_keys, user_keys, member_keys) = create_test_keys().await;
        let groups = create_test_groups_with_db(&relay_keys).await;
        let scope = Scope::Default;

        // User creates group
        let create_event = create_test_event(
            &user_keys,
            KIND_GROUP_CREATE_9007,
            vec![Tag::custom(TagKind::h(), [TEST_GROUP_ID])],
        )
        .await;
        groups
            .handle_group_create(create_event, &scope)
            .await
            .unwrap();

        // User adds member
        let add_event = create_test_event(
            &user_keys,
            KIND_GROUP_ADD_USER_9000,
            vec![
                Tag::custom(TagKind::h(), [TEST_GROUP_ID]),
                Tag::public_key(member_keys.public_key()),
            ],
        )
        .await;
        groups.handle_put_user(add_event, &scope).unwrap();

        // Promote member to admin
        let set_roles_event = create_test_event(
            &user_keys,
            KIND_GROUP_SET_ROLES_9006,
            vec![
                Tag::custom(TagKind::h(), [TEST_GROUP_ID]),
                Tag::custom(
                    TagKind::p(),
                    [member_keys.public_key().to_string(), "admin".to_string()],
                ),
            ],
        )
        .await;

        groups.handle_set_roles(set_roles_event, &scope).unwrap();

        let group = groups.get_group(&scope, TEST_GROUP_ID).unwrap();
        assert!(group
            .value()
            .members
            .get(&member_keys.public_key())
            .unwrap()
            .is(GroupRole::Admin));
    }

    #[tokio::test]
    async fn test_handle_set_roles_non_admin_cannot_set_roles() {
        let (relay_keys, user_keys, member_keys) = create_test_keys().await;
        let groups = create_test_groups_with_db(&relay_keys).await;
        let scope = Scope::Default;
        let (_, _, non_member_keys) = create_test_keys().await;

        // User creates group
        let create_event = create_test_event(
            &user_keys,
            KIND_GROUP_CREATE_9007,
            vec![Tag::custom(TagKind::h(), [TEST_GROUP_ID])],
        )
        .await;
        groups
            .handle_group_create(create_event, &scope)
            .await
            .unwrap();

        // User adds member
        let add_event = create_test_event(
            &user_keys,
            KIND_GROUP_ADD_USER_9000,
            vec![
                Tag::custom(TagKind::h(), [TEST_GROUP_ID]),
                Tag::public_key(member_keys.public_key()),
            ],
        )
        .await;
        groups.handle_put_user(add_event, &scope).unwrap();

        // Attempt to set roles as non-admin
        let set_roles_event = create_test_event(
            &non_member_keys,
            KIND_GROUP_SET_ROLES_9006,
            vec![
                Tag::custom(TagKind::h(), [TEST_GROUP_ID]),
                Tag::custom(
                    TagKind::p(),
                    [member_keys.public_key().to_string(), "admin".to_string()],
                ),
            ],
        )
        .await;

        assert!(groups.handle_set_roles(set_roles_event, &scope).is_err());
    }

    #[tokio::test]
    async fn test_handle_put_user_admin_can_add_member() {
        let (relay_keys, user_keys, member_keys) = create_test_keys().await;
        let groups = create_test_groups_with_db(&relay_keys).await;
        let scope = Scope::Default;

        // User creates group
        let create_event = create_test_event(
            &user_keys,
            KIND_GROUP_CREATE_9007,
            vec![Tag::custom(TagKind::h(), [TEST_GROUP_ID])],
        )
        .await;
        groups
            .handle_group_create(create_event, &scope)
            .await
            .unwrap();

        // User adds member
        let add_event = create_test_event(
            &user_keys,
            KIND_GROUP_ADD_USER_9000,
            vec![
                Tag::custom(TagKind::h(), [TEST_GROUP_ID]),
                Tag::public_key(member_keys.public_key()),
            ],
        )
        .await;

        let result = groups.handle_put_user(add_event, &scope).unwrap();
        assert!(!result.is_empty());

        let group = groups.get_group(&scope, TEST_GROUP_ID).unwrap();
        assert!(group
            .value()
            .members
            .contains_key(&member_keys.public_key()));
    }

    #[tokio::test]
    async fn test_handle_put_user_non_admin_cannot_add_member() {
        let (relay_keys, user_keys, member_keys) = create_test_keys().await;
        let groups = create_test_groups_with_db(&relay_keys).await;
        let scope = Scope::Default;
        let (_, _, non_member_keys) = create_test_keys().await;

        // User creates group
        let create_event = create_test_event(
            &user_keys,
            KIND_GROUP_CREATE_9007,
            vec![Tag::custom(TagKind::h(), [TEST_GROUP_ID])],
        )
        .await;
        groups
            .handle_group_create(create_event, &scope)
            .await
            .unwrap();

        // Attempt to add member as non-admin
        let add_event = create_test_event(
            &non_member_keys,
            KIND_GROUP_ADD_USER_9000,
            vec![
                Tag::custom(TagKind::h(), [TEST_GROUP_ID]),
                Tag::public_key(member_keys.public_key()),
            ],
        )
        .await;

        assert!(groups.handle_put_user(add_event, &scope).is_err());
    }

    #[tokio::test]
    async fn test_handle_remove_user_admin_can_remove_member() {
        let (relay_keys, user_keys, member_keys) = create_test_keys().await;
        let groups = create_test_groups_with_db(&relay_keys).await;
        let scope = Scope::Default;

        // User creates group
        let create_event = create_test_event(
            &user_keys,
            KIND_GROUP_CREATE_9007,
            vec![Tag::custom(TagKind::h(), [TEST_GROUP_ID])],
        )
        .await;
        groups
            .handle_group_create(create_event, &scope)
            .await
            .unwrap();

        // User adds member
        let add_event = create_test_event(
            &user_keys,
            KIND_GROUP_ADD_USER_9000,
            vec![
                Tag::custom(TagKind::h(), [TEST_GROUP_ID]),
                Tag::public_key(member_keys.public_key()),
            ],
        )
        .await;
        groups.handle_put_user(add_event, &scope).unwrap();

        // Remove member
        let remove_event = create_test_event(
            &user_keys,
            KIND_GROUP_REMOVE_USER_9001,
            vec![
                Tag::custom(TagKind::h(), [TEST_GROUP_ID]),
                Tag::custom(TagKind::p(), [member_keys.public_key().to_string()]),
            ],
        )
        .await;

        let result = groups.handle_remove_user(remove_event, &scope);
        assert!(!result.unwrap().is_empty());

        let group = groups.get_group(&scope, TEST_GROUP_ID).unwrap();
        assert!(!group
            .value()
            .members
            .contains_key(&member_keys.public_key()));
    }

    #[tokio::test]
    async fn test_handle_remove_user_non_admin_cannot_remove_member() {
        let (relay_keys, user_keys, member_keys) = create_test_keys().await;
        let groups = create_test_groups_with_db(&relay_keys).await;
        let scope = Scope::Default;
        let (_, _, non_member_keys) = create_test_keys().await;

        // User creates group
        let create_event = create_test_event(
            &user_keys,
            KIND_GROUP_CREATE_9007,
            vec![Tag::custom(TagKind::h(), [TEST_GROUP_ID])],
        )
        .await;
        groups
            .handle_group_create(create_event, &scope)
            .await
            .unwrap();

        // User adds member
        let add_event = create_test_event(
            &user_keys,
            KIND_GROUP_ADD_USER_9000,
            vec![
                Tag::custom(TagKind::h(), [TEST_GROUP_ID]),
                Tag::public_key(member_keys.public_key()),
            ],
        )
        .await;
        groups.handle_put_user(add_event, &scope).unwrap();

        // Attempt to remove member as non-admin
        let remove_event = create_test_event(
            &non_member_keys,
            KIND_GROUP_REMOVE_USER_9001,
            vec![
                Tag::custom(TagKind::h(), [TEST_GROUP_ID]),
                Tag::public_key(member_keys.public_key()),
            ],
        )
        .await;

        assert!(groups.handle_remove_user(remove_event, &scope).is_err());
    }

    #[tokio::test]
    async fn test_handle_edit_metadata_can_set_name() {
        let (groups, admin_keys, _, _, group_id, scope) = setup_test_groups().await;

        let tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::custom(TagKind::Name, ["New Group Name"]),
        ];
        let event = create_test_event(&admin_keys, KIND_GROUP_EDIT_METADATA_9002, tags).await;
        assert!(groups.handle_edit_metadata(event, &scope).is_ok());

        let group = groups.get_group(&scope, &group_id).unwrap();
        assert_eq!(group.value().metadata.name, "New Group Name");
    }

    #[tokio::test]
    async fn test_handle_edit_metadata_can_set_about() {
        let (groups, admin_keys, _, _, group_id, scope) = setup_test_groups().await;

        let tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::custom(TagKind::custom("about"), ["About text"]),
        ];
        let event = create_test_event(&admin_keys, KIND_GROUP_EDIT_METADATA_9002, tags).await;
        assert!(groups.handle_edit_metadata(event, &scope).is_ok());

        let group = groups.get_group(&scope, &group_id).unwrap();
        assert_eq!(group.value().metadata.about, Some("About text".to_string()));
    }

    #[tokio::test]
    async fn test_handle_edit_metadata_can_set_picture() {
        let (groups, admin_keys, _, _, group_id, scope) = setup_test_groups().await;

        let tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::custom(TagKind::custom("picture"), ["picture_url"]),
        ];
        let event = create_test_event(&admin_keys, KIND_GROUP_EDIT_METADATA_9002, tags).await;
        assert!(groups.handle_edit_metadata(event, &scope).is_ok());

        let group = groups.get_group(&scope, &group_id).unwrap();
        assert_eq!(
            group.value().metadata.picture,
            Some("picture_url".to_string())
        );
    }

    #[tokio::test]
    async fn test_handle_edit_metadata_can_set_visibility() {
        let (groups, admin_keys, _, _, group_id, scope) = setup_test_groups().await;

        let tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::custom(TagKind::custom("public"), &[] as &[String]),
        ];
        let event = create_test_event(&admin_keys, KIND_GROUP_EDIT_METADATA_9002, tags).await;
        assert!(groups.handle_edit_metadata(event, &scope).is_ok());

        let group = groups.get_group(&scope, &group_id).unwrap();
        assert!(!group.value().metadata.private);
    }

    #[tokio::test]
    async fn test_handle_edit_metadata_can_set_multiple_fields() {
        let (groups, admin_keys, _, _, group_id, scope) = setup_test_groups().await;

        let tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::custom(TagKind::Name, ["New Group Name"]),
            Tag::custom(TagKind::custom("about"), ["About text"]),
            Tag::custom(TagKind::custom("picture"), ["picture_url"]),
            Tag::custom(TagKind::custom("public"), &[] as &[String]),
        ];
        let event = create_test_event(&admin_keys, KIND_GROUP_EDIT_METADATA_9002, tags).await;
        assert!(groups.handle_edit_metadata(event, &scope).is_ok());

        let group = groups.get_group(&scope, &group_id).unwrap();
        assert_eq!(group.value().metadata.name, "New Group Name");
        assert_eq!(group.value().metadata.about, Some("About text".to_string()));
        assert_eq!(
            group.value().metadata.picture,
            Some("picture_url".to_string())
        );
        assert!(!group.value().metadata.private);
    }

    #[tokio::test]
    async fn test_handle_create_invite_creates_valid_invite() {
        let (groups, admin_keys, _, _, group_id, scope) = setup_test_groups().await;

        // Create invite
        let invite_code = "test_invite_1234567890ab";
        let tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::custom(TagKind::custom("code"), [invite_code]),
        ];
        let event =
            create_test_event(&admin_keys, KIND_GROUP_CREATE_INVITE_9009, tags.clone()).await;
        groups.handle_create_invite(event, &scope).unwrap();

        // Verify invite was created
        let group = groups.get_group(&scope, &group_id).unwrap();
        assert!(group.value().invites.contains_key(invite_code));
    }

    #[tokio::test]
    async fn test_handle_create_invite_can_be_used_to_join() {
        let (groups, admin_keys, member_keys, _, group_id, scope) = setup_test_groups().await;

        // Create invite
        let invite_code = "test_invite_1234567890ab";
        let tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::custom(TagKind::custom("code"), [invite_code]),
        ];
        let event =
            create_test_event(&admin_keys, KIND_GROUP_CREATE_INVITE_9009, tags.clone()).await;
        groups.handle_create_invite(event, &scope).unwrap();

        // Use invite
        let join_tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::custom(TagKind::custom("code"), [invite_code]),
        ];
        let join_event =
            create_test_event(&member_keys, KIND_GROUP_USER_JOIN_REQUEST_9021, join_tags).await;
        assert!(!groups
            .handle_join_request(join_event, &scope)
            .unwrap()
            .is_empty());

        // Verify member was added
        let group = groups.get_group(&scope, &group_id).unwrap();
        assert!(group.value().is_member(&member_keys.public_key()));
    }

    #[tokio::test]
    async fn test_handle_create_invite_marks_invite_as_used() {
        let (groups, admin_keys, member_keys, _, group_id, scope) = setup_test_groups().await;

        // Create invite
        let invite_code = "test_invite_1234567890ab";
        let tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::custom(TagKind::custom("code"), [invite_code]),
        ];
        let event =
            create_test_event(&admin_keys, KIND_GROUP_CREATE_INVITE_9009, tags.clone()).await;
        groups.handle_create_invite(event, &scope).unwrap();

        // Use invite
        let join_tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::custom(TagKind::custom("code"), [invite_code]),
        ];
        let join_event =
            create_test_event(&member_keys, KIND_GROUP_USER_JOIN_REQUEST_9021, join_tags).await;
        groups.handle_join_request(join_event, &scope).unwrap();
    }

    #[tokio::test]
    async fn test_handle_join_request_with_valid_invite() {
        let (groups, admin_keys, member_keys, _, group_id, scope) = setup_test_groups().await;

        // Create invite
        let invite_code = "test_invite_1234567890ab";
        let tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::custom(TagKind::custom("code"), [invite_code]),
        ];
        let event =
            create_test_event(&admin_keys, KIND_GROUP_CREATE_INVITE_9009, tags.clone()).await;
        groups.handle_create_invite(event, &scope).unwrap();

        // Use invite to join
        let join_tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::custom(TagKind::custom("code"), [invite_code]),
        ];
        let join_event =
            create_test_event(&member_keys, KIND_GROUP_USER_JOIN_REQUEST_9021, join_tags).await;
        assert!(!groups
            .handle_join_request(join_event, &scope)
            .unwrap()
            .is_empty());

        let group = groups.get_group(&scope, &group_id).unwrap();
        assert!(group.value().is_member(&member_keys.public_key()));
    }

    #[tokio::test]
    async fn test_handle_join_request_with_invalid_invite() {
        let (groups, _, member_keys, _, group_id, scope) = setup_test_groups().await;

        // Try to join with invalid invite code
        let join_tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::custom(TagKind::custom("code"), ["invalid_code"]),
        ];
        let join_event =
            create_test_event(&member_keys, KIND_GROUP_USER_JOIN_REQUEST_9021, join_tags).await;

        // According to NIP-29, the join request should be saved
        let result = groups.handle_join_request(join_event, &scope).unwrap();
        assert_eq!(result.len(), 1, "Join request should be saved");

        match &result[0] {
            StoreCommand::SaveSignedEvent(event, _, _) => {
                assert_eq!(event.kind, KIND_GROUP_USER_JOIN_REQUEST_9021);
            }
            _ => panic!("Expected SaveSignedEvent command"),
        }

        let group = groups.get_group(&scope, &group_id).unwrap();
        assert!(!group.value().is_member(&member_keys.public_key()));
    }

    #[tokio::test]
    async fn test_handle_join_request_without_invite_adds_to_requests() {
        let (groups, _, member_keys, _, group_id, scope) = setup_test_groups().await;

        // Join without invite code
        let join_tags = vec![Tag::custom(TagKind::h(), [&group_id])];
        let join_event =
            create_test_event(&member_keys, KIND_GROUP_USER_JOIN_REQUEST_9021, join_tags).await;

        // According to NIP-29, the join request should be saved
        let result = groups.handle_join_request(join_event, &scope).unwrap();
        assert_eq!(result.len(), 1, "Join request should be saved");

        match &result[0] {
            StoreCommand::SaveSignedEvent(event, _, _) => {
                assert_eq!(event.kind, KIND_GROUP_USER_JOIN_REQUEST_9021);
            }
            _ => panic!("Expected SaveSignedEvent command"),
        }

        let group = groups.get_group(&scope, &group_id).unwrap();
        // The join request should be added to the join_requests set
        assert!(group
            .value()
            .join_requests
            .contains(&member_keys.public_key()));
    }

    #[tokio::test]
    async fn test_handle_leave_request_member_can_leave() {
        let (groups, admin_keys, member_keys, _, group_id, scope) = setup_test_groups().await;

        // Add member first
        let add_tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::public_key(member_keys.public_key()),
        ];
        let add_event = create_test_event(&admin_keys, KIND_GROUP_ADD_USER_9000, add_tags).await;
        groups.handle_put_user(add_event, &scope).unwrap();

        // Test leave request
        let leave_tags = vec![Tag::custom(TagKind::h(), [&group_id])];
        let leave_event =
            create_test_event(&member_keys, KIND_GROUP_USER_LEAVE_REQUEST_9022, leave_tags).await;

        // Get the store commands
        let leave_event_id = leave_event.id;
        let commands = groups.handle_leave_request(leave_event, &scope).unwrap();

        // Verify the commands
        assert_eq!(
            commands.len(),
            2,
            "Should have 2 commands: save leave event and update members"
        );
        match &commands[0] {
            StoreCommand::SaveSignedEvent(event, _, _) => assert_eq!(event.id, leave_event_id),
            _ => panic!("First command should be SaveSignedEvent"),
        }
        match &commands[1] {
            StoreCommand::SaveUnsignedEvent(event, _, _) => {
                assert_eq!(event.kind, KIND_GROUP_MEMBERS_39002);
                // Verify the member is not in the members list
                assert!(!event
                    .tags
                    .filter(TagKind::p())
                    .filter_map(|t| t.content())
                    .any(|t| t == member_keys.public_key().to_string()));
            }
            _ => panic!("Second command should be SaveUnsignedEvent for members"),
        }

        // Also verify the state change
        let group = groups.get_group(&scope, &group_id).unwrap();
        assert!(!group.value().is_member(&member_keys.public_key()));
    }

    #[tokio::test]
    async fn test_handle_leave_request_non_member_cannot_leave() {
        let (groups, _, non_member_keys, _, group_id, scope) = setup_test_groups().await;

        // Test leave request from non-member
        let leave_tags = vec![Tag::custom(TagKind::h(), [&group_id])];
        let leave_event = create_test_event(
            &non_member_keys,
            KIND_GROUP_USER_LEAVE_REQUEST_9022,
            leave_tags,
        )
        .await;
        assert!(groups
            .handle_leave_request(leave_event, &scope)
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn test_handle_leave_request_admin_can_leave_if_not_last_admin() {
        let (groups, admin_keys, member_keys, _, group_id, scope) = setup_test_groups().await;

        // Add member as admin
        let add_tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::public_key(member_keys.public_key()),
        ];
        let add_event = create_test_event(&admin_keys, KIND_GROUP_ADD_USER_9000, add_tags).await;
        groups.handle_put_user(add_event, &scope).unwrap();

        // Make member an admin
        let set_roles_event = create_test_event(
            &admin_keys,
            KIND_GROUP_SET_ROLES_9006,
            vec![
                Tag::custom(TagKind::h(), [&group_id]),
                Tag::custom(
                    TagKind::p(),
                    [member_keys.public_key().to_string(), "admin".to_string()],
                ),
            ],
        )
        .await;
        groups.handle_set_roles(set_roles_event, &scope).unwrap();

        // Original admin tries to leave
        let leave_tags = vec![Tag::custom(TagKind::h(), [&group_id])];
        let leave_event =
            create_test_event(&admin_keys, KIND_GROUP_USER_LEAVE_REQUEST_9022, leave_tags).await;
        assert!(!groups
            .handle_leave_request(leave_event, &scope)
            .unwrap()
            .is_empty());

        let group = groups.get_group(&scope, &group_id).unwrap();
        assert!(!group.value().is_member(&admin_keys.public_key()));
        assert!(group.value().is_admin(&member_keys.public_key()));
    }

    #[tokio::test]
    async fn test_handle_leave_request_last_admin_can_leave() {
        let (groups, admin_keys, _, _, group_id, scope) = setup_test_groups().await;

        // Test leave request from last admin
        let leave_tags = vec![Tag::custom(TagKind::h(), [&group_id])];
        let leave_event =
            create_test_event(&admin_keys, KIND_GROUP_USER_LEAVE_REQUEST_9022, leave_tags).await;

        // The last admin should not be able to leave
        let result = groups.handle_leave_request(leave_event, &scope);
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "Notice: Cannot remove last admin"
        );

        // Verify admin is still in the group
        let group = groups.get_group(&scope, &group_id).unwrap();
        assert!(group.value().is_member(&admin_keys.public_key()));
        assert!(group.value().is_admin(&admin_keys.public_key()));
    }

    #[tokio::test]
    async fn test_handle_leave_request_removes_from_join_requests() {
        let (groups, _, member_keys, _, group_id, scope) = setup_test_groups().await;

        // Add to join requests
        let join_tags = vec![Tag::custom(TagKind::h(), [&group_id])];
        let join_event =
            create_test_event(&member_keys, KIND_GROUP_USER_JOIN_REQUEST_9021, join_tags).await;
        groups.handle_join_request(join_event, &scope).unwrap();

        // Test leave request
        let leave_tags = vec![Tag::custom(TagKind::h(), [&group_id])];
        let leave_event =
            create_test_event(&member_keys, KIND_GROUP_USER_LEAVE_REQUEST_9022, leave_tags).await;
        assert!(groups
            .handle_leave_request(leave_event, &scope)
            .unwrap()
            .is_empty());

        let group = groups.get_group(&scope, &group_id).unwrap();
        assert!(!group
            .value()
            .join_requests
            .contains(&member_keys.public_key()));
    }

    #[tokio::test]
    async fn test_handle_edit_metadata_non_admin_cannot_edit() {
        let (groups, _, non_member_keys, _, group_id, scope) = setup_test_groups().await;

        let tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::custom(TagKind::Name, ["New Group Name"]),
        ];
        let event = create_test_event(&non_member_keys, KIND_GROUP_EDIT_METADATA_9002, tags).await;
        assert!(groups.handle_edit_metadata(event, &scope).is_err());
    }

    #[tokio::test]
    async fn test_handle_edit_metadata_member_cannot_edit() {
        let (groups, admin_keys, member_keys, _, group_id, scope) = setup_test_groups().await;

        // Add member first
        let add_tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::public_key(member_keys.public_key()),
        ];
        let add_event = create_test_event(&admin_keys, KIND_GROUP_ADD_USER_9000, add_tags).await;
        groups.handle_put_user(add_event, &scope).unwrap();

        // Try to edit metadata as member
        let tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::custom(TagKind::Name, ["New Group Name"]),
        ];
        let event = create_test_event(&member_keys, KIND_GROUP_EDIT_METADATA_9002, tags).await;
        assert!(groups.handle_edit_metadata(event, &scope).is_err());
    }

    #[tokio::test]
    async fn test_handle_edit_metadata_rejects_invalid_group() {
        let (groups, admin_keys, _, _, _, scope) = setup_test_groups().await;

        let tags = vec![
            Tag::custom(TagKind::h(), ["invalid_group_id"]),
            Tag::custom(TagKind::Name, ["New Group Name"]),
        ];
        let event = create_test_event(&admin_keys, KIND_GROUP_EDIT_METADATA_9002, tags).await;
        assert!(groups.handle_edit_metadata(event, &scope).is_err());
    }

    #[tokio::test]
    async fn test_handle_edit_metadata_preserves_unmodified_fields() {
        let (groups, admin_keys, _, _, group_id, scope) = setup_test_groups().await;

        // First set multiple fields
        let initial_tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::custom(TagKind::Name, ["Initial Name"]),
            Tag::custom(TagKind::custom("about"), ["Initial About"]),
            Tag::custom(TagKind::custom("picture"), ["initial_picture_url"]),
        ];
        let initial_event =
            create_test_event(&admin_keys, KIND_GROUP_EDIT_METADATA_9002, initial_tags).await;
        groups.handle_edit_metadata(initial_event, &scope).unwrap();

        // Then update only the name
        let update_tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::custom(TagKind::Name, ["Updated Name"]),
        ];
        let update_event =
            create_test_event(&admin_keys, KIND_GROUP_EDIT_METADATA_9002, update_tags).await;
        groups.handle_edit_metadata(update_event, &scope).unwrap();

        // Verify other fields are preserved
        let group = groups.get_group(&scope, &group_id).unwrap();
        assert_eq!(group.value().metadata.name, "Updated Name");
        assert_eq!(
            group.value().metadata.about,
            Some("Initial About".to_string())
        );
        assert_eq!(
            group.value().metadata.picture,
            Some("initial_picture_url".to_string())
        );
    }

    #[tokio::test]
    async fn test_handle_edit_metadata_preserves_g_tag_in_generated_event() {
        let (groups, admin_keys, _, _, group_id, scope) = setup_test_groups().await;

        // Send kind 9002 with a "g" tag (single-letter unknown tag)
        let tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::custom(TagKind::Name, ["Test Group"]),
            Tag::custom(TagKind::custom("g"), ["test_value"]),
        ];
        let event = create_test_event(&admin_keys, KIND_GROUP_EDIT_METADATA_9002, tags).await;

        let commands = groups.handle_edit_metadata(event, &scope).unwrap();

        // Find the generated kind 39000 event in the commands
        let mut found_metadata_event = false;
        for cmd in &commands {
            if let StoreCommand::SaveUnsignedEvent(unsigned_event, _, _) = cmd {
                if unsigned_event.kind == KIND_GROUP_METADATA_39000 {
                    found_metadata_event = true;

                    // Check if the "g" tag is preserved in the generated event
                    let g_tag = unsigned_event
                        .tags
                        .iter()
                        .find(|t| t.kind() == TagKind::custom("g"));

                    assert!(
                        g_tag.is_some(),
                        "The 'g' tag should be preserved in the generated kind 39000 event"
                    );
                    assert_eq!(
                        g_tag.unwrap().content(),
                        Some("test_value"),
                        "The 'g' tag value should be preserved"
                    );
                }
            }
        }

        assert!(
            found_metadata_event,
            "Should generate a kind 39000 metadata event"
        );
    }

    #[tokio::test]
    async fn test_group_not_found_returns_event_error() {
        let (admin_keys, _, _) = create_test_keys().await;
        let groups = create_test_groups_with_db(&admin_keys).await;
        let scope = Scope::Default;

        // Test handle_edit_metadata with non-existent group
        let tags = vec![
            Tag::custom(TagKind::h(), ["non_existent_group"]),
            Tag::custom(TagKind::Name, ["Test"]),
        ];
        let event = create_test_event(&admin_keys, KIND_GROUP_EDIT_METADATA_9002, tags).await;
        let result = groups.handle_edit_metadata(event, &scope);

        // Should return an EventError, not a Notice
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, Error::EventError { .. }),
            "Should return EventError for non-existent group, got: {err:?}"
        );

        // Test handle_put_user with non-existent group
        let tags = vec![
            Tag::custom(TagKind::h(), ["non_existent_group"]),
            Tag::public_key(admin_keys.public_key()),
        ];
        let event = create_test_event(&admin_keys, KIND_GROUP_ADD_USER_9000, tags).await;
        let result = groups.handle_put_user(event, &scope);

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, Error::EventError { .. }),
            "Should return EventError for non-existent group, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn test_handle_create_invite_non_admin_cannot_create() {
        let (groups, _, non_member_keys, _, group_id, scope) = setup_test_groups().await;

        let invite_code = "test_invite_1234567890ab";
        let tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::custom(TagKind::custom("code"), [invite_code]),
        ];
        let event = create_test_event(&non_member_keys, KIND_GROUP_CREATE_INVITE_9009, tags).await;
        assert!(groups.handle_create_invite(event, &scope).is_err());
    }

    #[tokio::test]
    async fn test_handle_create_invite_member_cannot_create() {
        let (groups, admin_keys, member_keys, _, group_id, scope) = setup_test_groups().await;

        // Add member first
        let add_tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::public_key(member_keys.public_key()),
        ];
        let add_event = create_test_event(&admin_keys, KIND_GROUP_ADD_USER_9000, add_tags).await;
        groups.handle_put_user(add_event, &scope).unwrap();

        // Try to create invite as member
        let invite_code = "test_invite_1234567890ab";
        let tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::custom(TagKind::custom("code"), [invite_code]),
        ];
        let event = create_test_event(&member_keys, KIND_GROUP_CREATE_INVITE_9009, tags).await;
        assert!(groups.handle_create_invite(event, &scope).is_err());
    }

    #[tokio::test]
    async fn test_handle_create_invite_rejects_duplicate_code() {
        let (groups, admin_keys, _, _, group_id, scope) = setup_test_groups().await;

        // Create first invite
        let invite_code = "test_invite_1234567890ab";
        let tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::custom(TagKind::custom("code"), [invite_code]),
        ];
        let event =
            create_test_event(&admin_keys, KIND_GROUP_CREATE_INVITE_9009, tags.clone()).await;
        groups.handle_create_invite(event, &scope).unwrap();

        // Try to create invite with same code
        let duplicate_event =
            create_test_event(&admin_keys, KIND_GROUP_CREATE_INVITE_9009, tags).await;
        assert!(groups
            .handle_create_invite(duplicate_event, &scope)
            .is_err());
    }

    #[tokio::test]
    async fn test_handle_create_invite_rejects_missing_code() {
        let (groups, admin_keys, _, _, group_id, scope) = setup_test_groups().await;

        let tags = vec![Tag::custom(TagKind::h(), [&group_id])];
        let event = create_test_event(&admin_keys, KIND_GROUP_CREATE_INVITE_9009, tags).await;
        assert!(groups.handle_create_invite(event, &scope).is_err());
    }

    #[tokio::test]
    async fn test_handle_create_invite_rejects_invalid_group() {
        let (groups, admin_keys, _, _, _, scope) = setup_test_groups().await;

        let invite_code = "test_invite_1234567890ab";
        let tags = vec![
            Tag::custom(TagKind::h(), ["invalid_group_id"]),
            Tag::custom(TagKind::custom("code"), [invite_code]),
        ];
        let event = create_test_event(&admin_keys, KIND_GROUP_CREATE_INVITE_9009, tags).await;
        assert!(groups.handle_create_invite(event, &scope).is_err());
    }

    #[tokio::test]
    async fn test_handle_join_request_with_used_invite() {
        let (groups, admin_keys, member_keys, non_member_keys, group_id, scope) =
            setup_test_groups().await;

        // Create and use invite
        let invite_code = "test_invite_1234567890ab";
        let tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::custom(TagKind::custom("code"), [invite_code]),
        ];
        let event =
            create_test_event(&admin_keys, KIND_GROUP_CREATE_INVITE_9009, tags.clone()).await;
        groups.handle_create_invite(event, &scope).unwrap();

        // First member uses invite
        let join_tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::custom(TagKind::custom("code"), [invite_code]),
        ];
        let join_event =
            create_test_event(&member_keys, KIND_GROUP_USER_JOIN_REQUEST_9021, join_tags).await;
        groups.handle_join_request(join_event, &scope).unwrap();

        // Second member tries to use same invite
        let join_tags2 = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::custom(TagKind::custom("code"), [invite_code]),
        ];
        let join_event2 = create_test_event(
            &non_member_keys,
            KIND_GROUP_USER_JOIN_REQUEST_9021,
            join_tags2,
        )
        .await;
        groups.handle_join_request(join_event2, &scope).unwrap();

        // With single-use invites, second user should be added to join_requests instead of members
        let group = groups.get_group(&scope, &group_id).unwrap();
        assert!(!group.value().is_member(&non_member_keys.public_key()));
        assert!(group
            .value()
            .join_requests
            .contains(&non_member_keys.public_key()));
    }

    #[tokio::test]
    async fn test_handle_join_request_with_reusable_invite() {
        let (groups, admin_keys, member_keys, non_member_keys, group_id, scope) =
            setup_test_groups().await;

        // Create reusable invite - note: using local create_test_event that returns Box<Event>
        let invite_code = "test_reusable_invite";
        let tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::custom(TagKind::custom("code"), [invite_code]),
            // Add reusable tag
            Tag::custom(TagKind::custom("reusable"), Vec::<String>::new()),
        ];
        let create_invite_event =
            create_test_event(&admin_keys, KIND_GROUP_CREATE_INVITE_9009, tags).await;
        groups
            .handle_create_invite(create_invite_event, &scope)
            .unwrap();

        // Verify the invite exists and is reusable - IN A SCOPE
        {
            let group = groups.get_group(&scope, &group_id).unwrap();
            assert!(group.value().invites.contains_key(invite_code));
            assert!(group.value().invites.get(invite_code).unwrap().reusable);
        }

        // First user joins with reusable invite
        let join_tags = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::custom(TagKind::custom("code"), [invite_code]),
        ];
        let join_event =
            create_test_event(&member_keys, KIND_GROUP_USER_JOIN_REQUEST_9021, join_tags).await;
        groups.handle_join_request(join_event, &scope).unwrap();

        // Verify first user was added - IN A SCOPE
        {
            let group = groups.get_group(&scope, &group_id).unwrap();
            assert!(group.value().is_member(&member_keys.public_key()));
        }

        // Second user tries to use the same reusable invite
        let join_tags2 = vec![
            Tag::custom(TagKind::h(), [&group_id]),
            Tag::custom(TagKind::custom("code"), [invite_code]),
        ];
        let join_event2 = create_test_event(
            &non_member_keys,
            KIND_GROUP_USER_JOIN_REQUEST_9021,
            join_tags2,
        )
        .await;
        groups.handle_join_request(join_event2, &scope).unwrap();

        // With reusable invites, both users should become members - IN A SCOPE
        {
            let group = groups.get_group(&scope, &group_id).unwrap();
            assert!(group.value().is_member(&member_keys.public_key()));
            assert!(group.value().is_member(&non_member_keys.public_key()));
        }
    }

    /// Wiping the user who created a group must not delete the group's own
    /// management events. Before this guard, a wipe used a bare
    /// `Filter::author(pubkey)` and silently orphaned every group that user made.
    #[tokio::test]
    async fn test_user_wipe_retains_protected_group_kinds() {
        let (groups, admin_keys, _, _, group_id, scope) = setup_test_groups().await;

        // handle_group_create only mutates in-memory state; persisting is the
        // caller's job, so store both events explicitly here.
        let creation = create_test_event(
            &admin_keys,
            KIND_GROUP_CREATE_9007,
            vec![Tag::custom(TagKind::h(), [&group_id])],
        )
        .await;
        groups
            .db
            .save_event(&creation, &scope)
            .await
            .expect("store creation event");

        // The group-creation event (9007) is authored by admin_keys and is
        // protected. The chat message (9) is ordinary content and is not.
        let chat = create_test_event(
            &admin_keys,
            Kind::from(9u16),
            vec![Tag::custom(TagKind::h(), [&group_id])],
        )
        .await;
        groups
            .db
            .save_event(&chat, &scope)
            .await
            .expect("store chat event");

        let author = admin_keys.public_key();
        let count_kind = |kind: u16| {
            let db = Arc::clone(&groups.db);
            let scope = scope.clone();
            async move {
                db.count(
                    vec![Filter::new().author(author).kind(Kind::from(kind))],
                    &scope,
                )
                .await
                .unwrap()
            }
        };

        assert_eq!(count_kind(9007).await, 1, "group creation event stored");
        assert_eq!(count_kind(9).await, 1, "chat event stored");

        let deleted = groups
            .admin_delete_user_events(&author.to_hex(), None)
            .await
            .expect("wipe succeeds");

        assert_eq!(
            count_kind(9007).await,
            1,
            "kind 9007 is protected and must survive a user wipe"
        );
        assert_eq!(count_kind(9).await, 0, "ordinary content is deleted");
        assert_eq!(deleted, 1, "only the unprotected event is counted");
    }

    /// Group membership events carry a `p` tag naming the member, so deleting
    /// "everything addressed to X" must not take 9000s with it.
    #[tokio::test]
    async fn test_recipient_delete_spares_group_membership_events() {
        let (groups, admin_keys, member_keys, _, group_id, scope) = setup_test_groups().await;
        let member = member_keys.public_key();

        // A 9000 addressed to the member, and a gift wrap addressed to them.
        let add_user = create_test_event(
            &admin_keys,
            KIND_GROUP_ADD_USER_9000,
            vec![
                Tag::custom(TagKind::h(), [&group_id]),
                Tag::public_key(member),
            ],
        )
        .await;
        groups.db.save_event(&add_user, &scope).await.unwrap();

        let wrap = create_test_event(
            &admin_keys,
            Kind::from(1059u16),
            vec![Tag::public_key(member)],
        )
        .await;
        groups.db.save_event(&wrap, &scope).await.unwrap();

        // Asking for both kinds: the protected one must be refused, not honoured.
        let deleted = groups
            .admin_delete_events_by_recipient(&member.to_hex(), &[1059, 9000])
            .await
            .expect("delete succeeds");

        assert_eq!(deleted, 1, "only the gift wrap is deleted");

        let remaining_9000 = groups
            .db
            .count(vec![Filter::new().kind(KIND_GROUP_ADD_USER_9000)], &scope)
            .await
            .unwrap();
        assert_eq!(remaining_9000, 1, "membership event survives");

        let remaining_wraps = groups
            .db
            .count(vec![Filter::new().kind(Kind::from(1059u16))], &scope)
            .await
            .unwrap();
        assert_eq!(remaining_wraps, 0, "gift wrap is gone");
    }

    /// An explicit kind list must not become a way around the protected set.
    #[tokio::test]
    async fn test_user_wipe_ignores_requested_protected_kinds() {
        let (groups, admin_keys, _, _, group_id, scope) = setup_test_groups().await;
        let author = admin_keys.public_key();

        let creation = create_test_event(
            &admin_keys,
            KIND_GROUP_CREATE_9007,
            vec![Tag::custom(TagKind::h(), [&group_id])],
        )
        .await;
        groups
            .db
            .save_event(&creation, &scope)
            .await
            .expect("store creation event");

        let deleted = groups
            .admin_delete_user_events(&author.to_hex(), Some(&[9007, 39000]))
            .await
            .expect("wipe succeeds");

        assert_eq!(
            deleted, 0,
            "requesting only protected kinds deletes nothing"
        );
        let remaining = groups
            .db
            .count(
                vec![Filter::new().author(author).kind(KIND_GROUP_CREATE_9007)],
                &scope,
            )
            .await
            .unwrap();
        assert_eq!(remaining, 1, "group creation event still present");
    }

    /// The whole point of the attribution tables: a gift wrap must be charged
    /// to its `p` tag recipient, never to the one-time key that signed it.
    /// Getting this backwards would name the victim of a flood as its source.
    #[tokio::test]
    async fn gift_wraps_are_attributed_to_the_recipient_not_the_throwaway_signer() {
        let (groups, _admin_keys, _, _, _group_id, scope) = setup_test_groups().await;

        let recipient = Keys::generate().public_key();
        let mut wrap_signers = Vec::new();

        // Three wraps, each signed by a different ephemeral key, all addressed
        // to the same recipient — exactly the shape of real 1059 traffic.
        for _ in 0..3 {
            let throwaway = Keys::generate();
            wrap_signers.push(throwaway.public_key().to_hex());
            let wrap = create_test_event(
                &throwaway,
                Kind::from(GIFT_WRAP_KIND),
                vec![Tag::public_key(recipient)],
            )
            .await;
            groups.db.save_event(&wrap, &scope).await.unwrap();
        }

        let stats = groups.admin_storage_stats(1000, &[], 0).await.unwrap();

        let wrap_row = stats
            .kind_authors
            .iter()
            .find(|k| k.kind == GIFT_WRAP_KIND)
            .expect("gift wraps appear in the per-kind breakdown");

        assert_eq!(wrap_row.attributed_by, Attribution::Recipient);
        assert_eq!(
            wrap_row.authors.len(),
            1,
            "three wraps to one recipient are one row, not three"
        );
        assert_eq!(wrap_row.authors[0].pubkey, recipient.to_hex());
        assert_eq!(wrap_row.authors[0].count, 3);
        assert!(wrap_row.authors[0].sampled_bytes > 0);

        for signer in &wrap_signers {
            assert!(
                !wrap_row.authors.iter().any(|a| &a.pubkey == signer),
                "the throwaway signer {signer} must not be charged for the wrap"
            );
        }
    }

    /// Everything that is not a gift wrap is charged to whoever signed it.
    #[tokio::test]
    async fn ordinary_kinds_are_attributed_to_their_author() {
        let (groups, admin_keys, _, _, group_id, scope) = setup_test_groups().await;

        let chat = create_test_event(
            &admin_keys,
            Kind::from(9u16),
            vec![Tag::custom(TagKind::h(), [&group_id])],
        )
        .await;
        groups.db.save_event(&chat, &scope).await.unwrap();

        let stats = groups.admin_storage_stats(1000, &[], 0).await.unwrap();

        let chat_row = stats
            .kind_authors
            .iter()
            .find(|k| k.kind == 9)
            .expect("kind 9 appears in the breakdown");

        assert_eq!(chat_row.attributed_by, Attribution::Author);
        assert!(chat_row
            .authors
            .iter()
            .any(|a| a.pubkey == admin_keys.public_key().to_hex()));

        // And the relay-wide table agrees with the per-kind one.
        assert!(stats
            .top_authors
            .iter()
            .any(|a| a.pubkey == admin_keys.public_key().to_hex()
                && a.attributed_by == Attribution::Author));
    }

    /// The relay-wide table mixes both rules, so each row has to carry the one
    /// that produced most of its bytes rather than a blanket label.
    #[tokio::test]
    async fn the_relay_wide_table_labels_each_row_with_its_own_rule() {
        let (groups, _admin_keys, _, _, _group_id, scope) = setup_test_groups().await;

        let wrap_recipient = Keys::generate().public_key();
        let wrap = create_test_event(
            &Keys::generate(),
            Kind::from(GIFT_WRAP_KIND),
            vec![Tag::public_key(wrap_recipient)],
        )
        .await;
        groups.db.save_event(&wrap, &scope).await.unwrap();

        let poster = Keys::generate();
        let note = create_test_event(&poster, Kind::from(1u16), vec![]).await;
        groups.db.save_event(&note, &scope).await.unwrap();

        let stats = groups.admin_storage_stats(1000, &[], 0).await.unwrap();

        let wrap_row = stats
            .top_authors
            .iter()
            .find(|a| a.pubkey == wrap_recipient.to_hex())
            .expect("recipient is listed");
        assert_eq!(wrap_row.attributed_by, Attribution::Recipient);

        let poster_row = stats
            .top_authors
            .iter()
            .find(|a| a.pubkey == poster.public_key().to_hex())
            .expect("poster is listed");
        assert_eq!(poster_row.attributed_by, Attribution::Author);
    }

    /// A preview that disagreed with the delete would make the confirmation
    /// meaningless, so both run off the same filter. This pins that.
    #[tokio::test]
    async fn prune_preview_matches_what_is_deleted() {
        let (groups, _admin_keys, _, _, _group_id, scope) = setup_test_groups().await;
        let author = Keys::generate();

        // Distinct content: identical events hash to the same id and the
        // database would store one.
        for i in 0..4 {
            let note = EventBuilder::new(Kind::from(1u16), format!("note {i}"))
                .sign_with_keys(&author)
                .unwrap();
            groups.db.save_event(&note, &scope).await.unwrap();
        }

        let hex = author.public_key().to_hex();
        let preview = groups
            .admin_prune_events(&hex, Attribution::Author, &[1], None, None, true)
            .await
            .unwrap();
        assert_eq!(preview, 4);

        // The preview must not have removed anything.
        let still_there = groups
            .admin_prune_events(&hex, Attribution::Author, &[1], None, None, true)
            .await
            .unwrap();
        assert_eq!(still_there, 4, "a dry run must not delete");

        let deleted = groups
            .admin_prune_events(&hex, Attribution::Author, &[1], None, None, false)
            .await
            .unwrap();
        assert_eq!(deleted, preview);

        let after = groups
            .admin_prune_events(&hex, Attribution::Author, &[1], None, None, true)
            .await
            .unwrap();
        assert_eq!(after, 0);
    }

    /// Protected NIP-29 kinds survive a targeted prune, as they do every other
    /// deletion path -- otherwise pruning a group creator orphans the group.
    #[tokio::test]
    async fn prune_never_touches_protected_kinds() {
        let (groups, admin_keys, _, _, group_id, scope) = setup_test_groups().await;

        let creation = create_test_event(
            &admin_keys,
            KIND_GROUP_CREATE_9007,
            vec![Tag::custom(TagKind::h(), [&group_id])],
        )
        .await;
        groups.db.save_event(&creation, &scope).await.unwrap();

        let deleted = groups
            .admin_prune_events(
                &admin_keys.public_key().to_hex(),
                Attribution::Author,
                &[9007],
                None,
                None,
                false,
            )
            .await
            .unwrap();

        assert_eq!(deleted, 0, "9007 is protected and must be refused");
        let remaining = groups
            .db
            .count(
                vec![Filter::new()
                    .author(admin_keys.public_key())
                    .kind(KIND_GROUP_CREATE_9007)],
                &scope,
            )
            .await
            .unwrap();
        assert_eq!(remaining, 1);
    }

    /// A window must bound the delete, not be quietly ignored.
    #[tokio::test]
    async fn prune_respects_the_date_window() {
        let (groups, _admin_keys, _, _, _group_id, scope) = setup_test_groups().await;
        let author = Keys::generate();
        let now = Timestamp::now().as_secs();

        // One old note, one recent.
        for age in [10_000u64, 10u64] {
            let event = EventBuilder::new(Kind::from(1u16), format!("age {age}"))
                .custom_created_at(Timestamp::from_secs(now - age))
                .sign_with_keys(&author)
                .unwrap();
            groups.db.save_event(&event, &scope).await.unwrap();
        }

        let hex = author.public_key().to_hex();

        // Only what is older than an hour.
        let old_only = groups
            .admin_prune_events(
                &hex,
                Attribution::Author,
                &[1],
                None,
                Some(now - 3600),
                true,
            )
            .await
            .unwrap();
        assert_eq!(old_only, 1, "the recent note is outside the window");

        groups
            .admin_prune_events(
                &hex,
                Attribution::Author,
                &[1],
                None,
                Some(now - 3600),
                false,
            )
            .await
            .unwrap();

        let left = groups
            .admin_prune_events(&hex, Attribution::Author, &[1], None, None, true)
            .await
            .unwrap();
        assert_eq!(left, 1, "the recent note survives");
    }

    /// Gift wraps must be prunable by recipient; matching on author would hit
    /// the throwaway key and delete nothing.
    #[tokio::test]
    async fn prune_matches_gift_wraps_by_recipient() {
        let (groups, _admin_keys, _, _, _group_id, scope) = setup_test_groups().await;
        let recipient = Keys::generate().public_key();

        // Each wrap is signed by a different throwaway key, so these are
        // distinct events even with identical content -- which is the point.
        for i in 0..3 {
            let wrap = EventBuilder::new(Kind::from(GIFT_WRAP_KIND), format!("wrap {i}"))
                .tag(Tag::public_key(recipient))
                .sign_with_keys(&Keys::generate())
                .unwrap();
            groups.db.save_event(&wrap, &scope).await.unwrap();
        }

        let hex = recipient.to_hex();
        assert_eq!(
            groups
                .admin_prune_events(
                    &hex,
                    Attribution::Author,
                    &[GIFT_WRAP_KIND],
                    None,
                    None,
                    true
                )
                .await
                .unwrap(),
            0,
            "the recipient authored none of them"
        );
        assert_eq!(
            groups
                .admin_prune_events(
                    &hex,
                    Attribution::Recipient,
                    &[GIFT_WRAP_KIND],
                    None,
                    None,
                    false
                )
                .await
                .unwrap(),
            3,
        );
    }

    /// Rows are ordered by bytes, because the screen exists to explain disk
    /// usage — a few large events outrank many tiny ones.
    #[tokio::test]
    async fn authors_are_ranked_by_bytes_not_event_count() {
        let (groups, _admin_keys, _, _, _group_id, scope) = setup_test_groups().await;

        let chatty = Keys::generate();
        for _ in 0..5 {
            let small = create_test_event(&chatty, Kind::from(1u16), vec![]).await;
            groups.db.save_event(&small, &scope).await.unwrap();
        }

        let heavy = Keys::generate();
        let big = EventBuilder::new(Kind::from(1u16), "x".repeat(50_000))
            .sign_with_keys(&heavy)
            .unwrap();
        groups.db.save_event(&big, &scope).await.unwrap();

        let stats = groups.admin_storage_stats(1000, &[], 0).await.unwrap();

        let heavy_pos = stats
            .top_authors
            .iter()
            .position(|a| a.pubkey == heavy.public_key().to_hex())
            .expect("heavy author listed");
        let chatty_pos = stats
            .top_authors
            .iter()
            .position(|a| a.pubkey == chatty.public_key().to_hex())
            .expect("chatty author listed");

        assert!(
            heavy_pos < chatty_pos,
            "one 50 KB note should outrank five short ones"
        );
    }
}
