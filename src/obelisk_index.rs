use crate::config::ObeliskIndexSettings;
use crate::group::KIND_GROUP_ROLES_39003;
use crate::groups::{
    Groups, KIND_GROUP_ADMINS_39001, KIND_GROUP_CREATE_9007, KIND_GROUP_CREATE_INVITE_9009,
    KIND_GROUP_DELETE_EVENT_9005, KIND_GROUP_MEMBERS_39002, KIND_GROUP_METADATA_39000,
};
use crate::{RelayDatabase, StoreCommand};
use dashmap::DashMap;
use nostr_lmdb::Scope;
use nostr_sdk::prelude::*;
use relay_builder::Error;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time;
use tracing::warn;

type ScopedGroupKey = (Scope, String);

#[derive(Debug, Clone)]
pub struct ObeliskReadContext {
    pub pubkey: PublicKey,
    pub relay_pubkey: PublicKey,
    pub admin_pubkeys: Vec<PublicKey>,
}

impl ObeliskReadContext {
    fn is_relay_admin(&self) -> bool {
        self.pubkey == self.relay_pubkey || self.admin_pubkeys.contains(&self.pubkey)
    }
}

#[derive(Debug, Clone, Default)]
struct IndexedGroup {
    state_events: Vec<Event>,
    recent_events: Vec<Event>,
    high_water: u64,
}

pub struct ObeliskIndex {
    database: Arc<RelayDatabase>,
    groups: Arc<Groups>,
    settings: ObeliskIndexSettings,
    relay_keys: Keys,
    entries: DashMap<ScopedGroupKey, IndexedGroup>,
    rebuild_scheduled: AtomicBool,
}

#[derive(Debug, Serialize)]
pub struct BootstrapResponse {
    pub version: u8,
    pub relay: String,
    pub generated_at: u64,
    pub cursor: BootstrapCursor,
    pub scopes: Vec<BootstrapScope>,
}

#[derive(Debug, Serialize)]
pub struct BootstrapCursor {
    pub since: u64,
}

#[derive(Debug, Serialize)]
pub struct BootstrapScope {
    pub scope: String,
    pub groups: Vec<BootstrapGroup>,
}

#[derive(Debug, Serialize)]
pub struct BootstrapGroup {
    pub id: String,
    pub events: Vec<Event>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_before: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct MessagesResponse {
    pub version: u8,
    pub scope: String,
    pub group_id: String,
    pub events: Vec<Event>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_before: Option<u64>,
}

#[derive(Debug)]
pub enum MessageScopeResolution {
    Found(Scope),
    NotFound,
    Ambiguous,
}

impl ObeliskIndex {
    pub async fn new(
        database: Arc<RelayDatabase>,
        groups: Arc<Groups>,
        settings: ObeliskIndexSettings,
        relay_keys: Keys,
    ) -> Result<Self, Error> {
        let index = Self {
            database,
            groups,
            settings,
            relay_keys,
            entries: DashMap::new(),
            rebuild_scheduled: AtomicBool::new(false),
        };
        index.rebuild().await?;
        Ok(index)
    }

    pub fn settings(&self) -> &ObeliskIndexSettings {
        &self.settings
    }

    pub async fn rebuild(&self) -> Result<(), Error> {
        self.entries.clear();

        for (scope, group_id, _) in self.groups.list_all_groups() {
            match self.load_group_from_lmdb(&scope, &group_id).await {
                Ok(entry) => {
                    self.entries.insert((scope, group_id), entry);
                }
                Err(err) => {
                    warn!("Obelisk index skipped group during rebuild: {}", err);
                }
            }
        }

        Ok(())
    }

    async fn load_group_from_lmdb(
        &self,
        scope: &Scope,
        group_id: &str,
    ) -> Result<IndexedGroup, Error> {
        let state_filter = Filter::new()
            .kinds(state_kinds())
            .custom_tag(
                SingleLetterTag::lowercase(Alphabet::D),
                group_id.to_string(),
            )
            .since(Timestamp::from(0));
        let content_filter = Filter::new()
            .kinds(indexed_content_kinds())
            .custom_tag(
                SingleLetterTag::lowercase(Alphabet::H),
                group_id.to_string(),
            )
            .limit(self.settings.recent_per_group.saturating_mul(4).max(1))
            .since(Timestamp::from(0));

        let state_events = self
            .database
            .query(vec![state_filter], scope)
            .await
            .map_err(|e| Error::internal(e.to_string()))?;
        let recent_events = self
            .database
            .query(vec![content_filter], scope)
            .await
            .map_err(|e| Error::internal(e.to_string()))?;

        let mut entry = IndexedGroup::default();
        for event in state_events {
            entry.apply_state_event(event);
        }
        for event in recent_events {
            entry.apply_recent_event(event, self.settings.recent_per_group);
        }
        Ok(entry)
    }

    pub fn apply_store_commands(self: &Arc<Self>, commands: &[StoreCommand]) {
        let mut needs_rebuild_after_persist = false;

        for command in commands {
            match command {
                StoreCommand::SaveSignedEvent(event, scope, _) => {
                    self.apply_saved_event(scope, event.as_ref());
                }
                StoreCommand::SaveUnsignedEvent(event, scope, _) => {
                    let mut event = event.clone();
                    event.ensure_id();
                    match event.sign_with_keys(&self.relay_keys) {
                        Ok(signed_event) => self.apply_saved_event(scope, &signed_event),
                        Err(err) => {
                            warn!("Obelisk index could not sign generated event: {}", err);
                            needs_rebuild_after_persist = true;
                        }
                    }
                }
                StoreCommand::DeleteEvents(_, _, _) => {
                    needs_rebuild_after_persist = true;
                }
            }
        }

        if needs_rebuild_after_persist {
            self.schedule_rebuild_after_persist();
        }
    }

    pub fn apply_saved_event(&self, scope: &Scope, event: &Event) {
        let Some(group_id) = crate::Group::extract_group_id(event).map(str::to_string) else {
            return;
        };

        if !is_state_kind(event.kind) && !is_indexed_content_kind(event.kind) {
            return;
        }

        let key = (scope.clone(), group_id);
        let mut entry = self.entries.entry(key).or_default();
        if is_state_kind(event.kind) {
            entry.apply_state_event(event.clone());
        } else {
            entry.apply_recent_event(event.clone(), self.settings.recent_per_group);
        }
    }

    fn schedule_rebuild_after_persist(self: &Arc<Self>) {
        if self.rebuild_scheduled.swap(true, Ordering::AcqRel) {
            return;
        }

        let index = Arc::clone(self);
        tokio::spawn(async move {
            time::sleep(Duration::from_secs(1)).await;
            if let Err(err) = index.rebuild().await {
                warn!("Obelisk index delayed rebuild failed: {}", err);
            }
            index.rebuild_scheduled.store(false, Ordering::Release);
        });
    }

    pub fn bootstrap(
        &self,
        relay_url: &str,
        context: &ObeliskReadContext,
        requested_limit: Option<usize>,
    ) -> BootstrapResponse {
        let generated_at = unix_now();
        let limit = requested_limit
            .unwrap_or(self.settings.recent_per_group)
            .min(self.settings.recent_per_group)
            .max(1);
        let mut grouped: std::collections::BTreeMap<String, Vec<BootstrapGroup>> =
            std::collections::BTreeMap::new();
        let mut high_water = 0;
        let mut included_groups = 0usize;

        let mut entries: Vec<_> = self
            .entries
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect();
        entries.sort_by(|a, b| {
            let (a_scope, a_group_id) = &a.0;
            let (b_scope, b_group_id) = &b.0;
            scope_to_wire(a_scope)
                .cmp(&scope_to_wire(b_scope))
                .then_with(|| a_group_id.cmp(b_group_id))
        });

        for ((scope, group_id), entry) in entries {
            if included_groups >= self.settings.max_bootstrap_groups {
                break;
            }
            if !self.can_read_group(&scope, &group_id, context) {
                continue;
            }

            let mut events = Vec::new();
            events.extend(
                entry
                    .state_events
                    .iter()
                    .filter(|event| self.can_see_event(&scope, &group_id, context, event))
                    .cloned(),
            );
            let recent = entry
                .recent_events
                .iter()
                .filter(|event| self.can_see_event(&scope, &group_id, context, event))
                .take(limit)
                .cloned()
                .collect::<Vec<_>>();
            let next_before = recent.last().map(event_secs);
            events.extend(recent);

            if events.is_empty() {
                continue;
            }

            high_water = high_water.max(entry.high_water);
            grouped
                .entry(scope_to_wire(&scope))
                .or_default()
                .push(BootstrapGroup {
                    id: group_id,
                    events,
                    next_before,
                });
            included_groups += 1;
        }

        let scopes = grouped
            .into_iter()
            .map(|(scope, groups)| BootstrapScope { scope, groups })
            .collect();

        BootstrapResponse {
            version: 1,
            relay: relay_url.to_string(),
            generated_at,
            cursor: BootstrapCursor {
                since: if high_water == 0 {
                    generated_at
                } else {
                    high_water
                },
            },
            scopes,
        }
    }

    pub fn resolve_message_scope(
        &self,
        group_id: &str,
        requested_scope: Option<&str>,
    ) -> MessageScopeResolution {
        let mut matches = self
            .groups
            .list_all_groups()
            .into_iter()
            .filter(|(_, id, _)| id == group_id)
            .map(|(scope, _, _)| scope)
            .collect::<Vec<_>>();

        if let Some(scope_name) = requested_scope {
            return matches
                .into_iter()
                .find(|scope| scope_to_wire(scope) == scope_name)
                .map_or(
                    MessageScopeResolution::NotFound,
                    MessageScopeResolution::Found,
                );
        }

        matches.sort_by_key(scope_to_wire);
        matches.dedup();
        match matches.len() {
            0 => MessageScopeResolution::NotFound,
            1 => MessageScopeResolution::Found(matches.remove(0)),
            _ => MessageScopeResolution::Ambiguous,
        }
    }

    pub async fn messages_for_group(
        &self,
        scope: &Scope,
        group_id: &str,
        context: &ObeliskReadContext,
        before: Option<u64>,
        requested_limit: Option<usize>,
    ) -> Result<Option<MessagesResponse>, Error> {
        if !self.can_read_group(scope, group_id, context) {
            return Ok(None);
        }

        let limit = requested_limit
            .unwrap_or(50)
            .min(self.settings.max_page_limit)
            .max(1);
        let mut filter = Filter::new()
            .kinds(message_kinds())
            .custom_tag(
                SingleLetterTag::lowercase(Alphabet::H),
                group_id.to_string(),
            )
            .limit(limit.saturating_mul(2))
            .since(Timestamp::from(0));

        if let Some(before) = before {
            filter = filter.until(Timestamp::from(before.saturating_sub(1)));
        }

        let raw = self
            .database
            .query(vec![filter], scope)
            .await
            .map_err(|e| Error::internal(e.to_string()))?;
        let mut events = raw
            .into_iter()
            .filter(|event| self.can_see_event(scope, group_id, context, event))
            .collect::<Vec<_>>();

        sort_desc(&mut events);
        events.truncate(limit);

        let next_before = events.last().map(event_secs);
        Ok(Some(MessagesResponse {
            version: 1,
            scope: scope_to_wire(scope),
            group_id: group_id.to_string(),
            events,
            next_before,
        }))
    }

    fn can_read_group(&self, scope: &Scope, group_id: &str, context: &ObeliskReadContext) -> bool {
        let Some(group) = self.groups.get_group(scope, group_id) else {
            return false;
        };
        if !group.metadata.private {
            return true;
        }
        context.is_relay_admin() || group.is_member(&context.pubkey)
    }

    fn can_see_event(
        &self,
        scope: &Scope,
        group_id: &str,
        context: &ObeliskReadContext,
        event: &Event,
    ) -> bool {
        if context.is_relay_admin() {
            return true;
        }
        self.groups
            .get_group(scope, group_id)
            .map(|group| {
                group
                    .can_see_event(&Some(context.pubkey), &context.relay_pubkey, event)
                    .unwrap_or(false)
            })
            .unwrap_or(false)
    }
}

impl IndexedGroup {
    fn apply_state_event(&mut self, event: Event) {
        self.high_water = self.high_water.max(event_secs(&event));
        if self.state_events.iter().any(|existing| {
            existing.kind == event.kind && event_secs(existing) > event_secs(&event)
        }) {
            return;
        }
        self.state_events
            .retain(|existing| existing.kind != event.kind);
        self.state_events.push(event);
        self.state_events
            .sort_by_key(|event| (event.kind.as_u16(), event_secs(event)));
    }

    fn apply_recent_event(&mut self, event: Event, limit: usize) {
        self.high_water = self.high_water.max(event_secs(&event));
        self.recent_events
            .retain(|existing| existing.id != event.id);
        self.recent_events.push(event);
        sort_desc(&mut self.recent_events);
        self.recent_events.truncate(limit.max(1));
    }
}

pub fn scope_to_wire(scope: &Scope) -> String {
    match scope {
        Scope::Default => "default".to_string(),
        Scope::Named { name, .. } => name.clone(),
    }
}

fn state_kinds() -> Vec<Kind> {
    vec![
        KIND_GROUP_METADATA_39000,
        KIND_GROUP_ADMINS_39001,
        KIND_GROUP_MEMBERS_39002,
        KIND_GROUP_ROLES_39003,
    ]
}

fn message_kinds() -> Vec<Kind> {
    vec![Kind::from(9), Kind::from(11)]
}

fn indexed_content_kinds() -> Vec<Kind> {
    vec![
        Kind::from(9),
        Kind::from(11),
        Kind::from(7),
        Kind::from(5),
        KIND_GROUP_DELETE_EVENT_9005,
        KIND_GROUP_CREATE_9007,
        KIND_GROUP_CREATE_INVITE_9009,
    ]
}

fn is_state_kind(kind: Kind) -> bool {
    state_kinds().contains(&kind)
}

fn is_indexed_content_kind(kind: Kind) -> bool {
    indexed_content_kinds().contains(&kind)
}

fn sort_desc(events: &mut [Event]) {
    events.sort_by(|a, b| {
        event_secs(b)
            .cmp(&event_secs(a))
            .then_with(|| b.id.to_hex().cmp(&a.id.to_hex()))
    });
}

fn event_secs(event: &Event) -> u64 {
    event.created_at.as_secs()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
