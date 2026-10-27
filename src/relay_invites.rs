//! Relay-level invitation codes: admission to the relay itself, not to a group.
//!
//! NIP-29 group invites (kind 9009 / 9021) only open a door into one group, and
//! only for someone the relay already admits. On a relay whose admission is the
//! follow graph, a newcomer with a fresh key is on no list and in no graph, so
//! a group invite gets them nowhere: every REQ and EVENT is refused before the
//! group logic is reached. This is the missing front door.
//!
//! A code is minted by a relay admin, or by a pubkey an admin has granted
//! minting rights with quotas. It is redeemed with a NIP-43 kind 28934 event
//! carrying `["claim", <code>]`, signed by the key being admitted. Each distinct
//! pubkey that redeems consumes one use; the same pubkey redeeming again is a
//! no-op. That is what lets a client keep the code and replay it whenever the
//! user generates a new key, without the user ever seeing the code.
//!
//! Kept out of the event store, like `reports_state.json`: who may mint, and
//! who came in on whose code, is the operator's bookkeeping, and must not be
//! something a client can publish, delete or query.

use nostr_sdk::prelude::*;
use parking_lot::RwLock;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::Arc;

pub const RELAY_INVITES_FILE: &str = "relay_invites.json";

/// NIP-43 relay join request. Already on the validation allow-lists in
/// `group.rs` as `KIND_CLAIM_28934`; this module is what gives it a meaning.
pub const KIND_RELAY_CLAIM: u16 = 28934;

/// How far a claim's `created_at` may be from now. A claim is a fresh request,
/// not a credential to be stored and replayed later.
pub const CLAIM_FRESHNESS_SECS: u64 = 10 * 60;

/// Generated codes are this long, from a 62-character alphabet: ~119 bits, far
/// past anything a guessing loop reaches. Never below
/// `group::MIN_INVITE_CODE_LEN` (see docs/known-issues.md §15).
pub const RELAY_INVITE_CODE_LEN: usize = 20;
const _: () = assert!(RELAY_INVITE_CODE_LEN >= crate::group::MIN_INVITE_CODE_LEN);

/// Longest code accepted on input. Bounds the lookup work an anonymous claim
/// can cause; real codes are `RELAY_INVITE_CODE_LEN`.
const MAX_CODE_INPUT_LEN: usize = 64;

/// Admin-minted defaults, matching the NIP-29 invite defaults: a credential
/// handed out in chat should expire and have a blast radius.
pub const DEFAULT_RELAY_INVITE_TTL_SECS: u64 = 60 * 60 * 24 * 30;
pub const DEFAULT_RELAY_INVITE_MAX_USES: u32 = 100;

const LABEL_MAX_CHARS: usize = 80;

const CODE_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Redemption {
    pub pubkey: PublicKey,
    pub at: u64,
    /// Set when an admin removed this invitee's admission. The record stays so
    /// the referral history is not rewritten, and so the use stays spent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evicted_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayInvite {
    pub code: String,
    pub created_by: PublicKey,
    /// Whether the creator was a relay admin when the code was minted. Admin
    /// codes are not bound by any grant; minter codes stop working when the
    /// grant is withdrawn or its admit quota is spent.
    pub created_by_admin: bool,
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// `None` means unlimited, which only an admin can ask for.
    pub max_uses: Option<u32>,
    /// `None` means it never expires, which only an admin can ask for.
    pub expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<u64>,
    #[serde(default)]
    pub redemptions: Vec<Redemption>,
}

impl RelayInvite {
    pub fn uses(&self) -> u32 {
        self.redemptions.len() as u32
    }

    pub fn is_expired(&self, now: u64) -> bool {
        self.expires_at.is_some_and(|at| now > at)
    }

    pub fn is_exhausted(&self) -> bool {
        self.max_uses.is_some_and(|max| self.uses() >= max)
    }

    /// Still able to admit someone, ignoring the minter's grant.
    pub fn is_live(&self, now: u64) -> bool {
        self.revoked_at.is_none() && !self.is_expired(now) && !self.is_exhausted()
    }

    /// Short state word for the API and the console.
    pub fn status(&self, now: u64) -> &'static str {
        if self.revoked_at.is_some() {
            "revoked"
        } else if self.is_expired(now) {
            "expired"
        } else if self.is_exhausted() {
            "exhausted"
        } else {
            "active"
        }
    }
}

/// Minting rights an admin has handed to a non-admin pubkey.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MinterGrant {
    pub pubkey: PublicKey,
    pub granted_by: PublicKey,
    pub granted_at: u64,
    /// Codes that may be live (not revoked, expired or exhausted) at once.
    pub max_codes: u32,
    /// Ceiling on each code's `max_uses`.
    pub max_uses_per_code: u32,
    /// People this minter may bring in across all their codes, ever. When it
    /// is spent, every code they minted stops admitting.
    pub max_total_admits: u32,
    /// Ceiling on each code's lifetime.
    pub max_ttl_secs: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct InviteState {
    #[serde(default)]
    invites: BTreeMap<String, RelayInvite>,
    /// Keyed by hex pubkey so the file stays readable.
    #[serde(default)]
    minters: BTreeMap<String, MinterGrant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedeemError {
    Unknown,
    Revoked,
    Expired,
    Exhausted,
    /// The minter's grant was withdrawn, or their admit quota is spent.
    MinterInactive,
}

impl RedeemError {
    pub fn as_str(self) -> &'static str {
        match self {
            RedeemError::Unknown => "unknown",
            RedeemError::Revoked => "revoked",
            RedeemError::Expired => "expired",
            RedeemError::Exhausted => "exhausted",
            RedeemError::MinterInactive => "inactive",
        }
    }

    /// The OK message a client sees. Prefixed by the caller with `restricted:`.
    /// Always contains "invite" so clients can tell it from a plain whitelist
    /// refusal, and the reason word so they can say something useful.
    pub fn message(self) -> String {
        let detail = match self {
            RedeemError::Unknown => "this code does not exist on this relay",
            RedeemError::Revoked => "this code was revoked",
            RedeemError::Expired => "this code has expired",
            RedeemError::Exhausted => "this code has no uses left",
            RedeemError::MinterInactive => "the person who shared this code can no longer invite",
        };
        format!("invite {}: {}", self.as_str(), detail)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedeemOutcome {
    /// A use was consumed and the pubkey is now admitted.
    Admitted { invited_by: PublicKey },
    /// The pubkey already holds a live redemption (of this or another code), so
    /// nothing was consumed. Replaying a stored code is always safe.
    AlreadyAdmitted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MintError {
    NotAllowed,
    CodeQuotaReached,
    AdmitQuotaReached,
}

impl MintError {
    pub fn message(self) -> &'static str {
        match self {
            MintError::NotAllowed => "This pubkey is not allowed to create invite codes",
            MintError::CodeQuotaReached => {
                "You already have as many active invite codes as you are allowed"
            }
            MintError::AdmitQuotaReached => "You have used all of your invites",
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct MintOptions {
    pub label: Option<String>,
    /// For admins, `Some(0)` means unlimited. Minters are clamped to their grant.
    pub max_uses: Option<u32>,
    /// Seconds. For admins, `Some(0)` means never. Minters are clamped.
    pub expires_in: Option<u64>,
}

/// Where a minter stands: what the clients show in the "Invite people" panel.
#[derive(Debug, Clone, Serialize)]
pub struct MinterStatus {
    pub can_mint: bool,
    pub is_admin: bool,
    pub grant: Option<MinterGrant>,
    pub active_codes: u32,
    pub codes_left: Option<u32>,
    pub admits_used: u32,
    pub admits_left: Option<u32>,
}

/// One inviter's slice of the referral tree.
#[derive(Debug, Clone, Serialize)]
pub struct ReferralBranch {
    pub inviter: PublicKey,
    pub is_admin_code: bool,
    pub total_admitted: u32,
    pub codes: Vec<RelayInvite>,
}

#[derive(Debug, Clone)]
pub struct RelayInviteStore {
    inner: Arc<RwLock<InviteState>>,
}

impl Default for RelayInviteStore {
    fn default() -> Self {
        Self::new(None)
    }
}

pub fn unix_now() -> u64 {
    Timestamp::now().as_secs()
}

pub fn generate_code() -> String {
    let mut rng = rand::thread_rng();
    (0..RELAY_INVITE_CODE_LEN)
        .map(|_| CODE_ALPHABET[rng.gen_range(0..CODE_ALPHABET.len())] as char)
        .collect()
}

fn clean_label(label: Option<String>) -> Option<String> {
    label
        .map(|l| l.trim().chars().take(LABEL_MAX_CHARS).collect::<String>())
        .filter(|l| !l.is_empty())
}

impl RelayInviteStore {
    /// Load from `config_dir/relay_invites.json`, or start empty. A file that
    /// fails to parse is left on disk untouched and logged; starting empty is
    /// recoverable, overwriting the operator's history is not.
    pub fn new(config_dir: Option<&Path>) -> Self {
        let mut state = InviteState::default();
        if let Some(dir) = config_dir {
            let path = dir.join(RELAY_INVITES_FILE);
            if path.exists() {
                match std::fs::read_to_string(&path)
                    .map_err(|e| e.to_string())
                    .and_then(|s| {
                        serde_json::from_str::<InviteState>(&s).map_err(|e| e.to_string())
                    }) {
                    Ok(loaded) => {
                        tracing::info!(
                            "Loaded {} relay invite codes and {} minter grants from {}",
                            loaded.invites.len(),
                            loaded.minters.len(),
                            path.display()
                        );
                        state = loaded;
                    }
                    Err(e) => tracing::warn!("Failed to load {}: {}", path.display(), e),
                }
            }
        }
        Self {
            inner: Arc::new(RwLock::new(state)),
        }
    }

    /// Write the whole state atomically: a temp file and a rename, so a crash
    /// mid-write cannot leave half a file that then fails to load and silently
    /// drops every admission it recorded.
    pub fn persist(&self, config_dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(config_dir)?;
        let json = {
            let state = self.inner.read();
            serde_json::to_string_pretty(&*state)
                .map_err(|e| std::io::Error::other(e.to_string()))?
        };
        let path = config_dir.join(RELAY_INVITES_FILE);
        let tmp = config_dir.join(format!("{RELAY_INVITES_FILE}.tmp"));
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &path)
    }

    /// Every pubkey currently admitted by an invite: the set the whitelist's
    /// invite tier is seeded with at startup.
    pub fn admitted_pubkeys(&self) -> Vec<PublicKey> {
        let state = self.inner.read();
        let mut seen = HashSet::new();
        for invite in state.invites.values() {
            for r in &invite.redemptions {
                if r.evicted_at.is_none() {
                    seen.insert(r.pubkey);
                }
            }
        }
        seen.into_iter().collect()
    }

    fn live_redemption_of<'a>(
        state: &'a InviteState,
        pk: &PublicKey,
    ) -> Option<(&'a RelayInvite, &'a Redemption)> {
        state.invites.values().find_map(|invite| {
            invite
                .redemptions
                .iter()
                .find(|r| r.pubkey == *pk && r.evicted_at.is_none())
                .map(|r| (invite, r))
        })
    }

    /// Who brought `pk` in, and on which code.
    pub fn invited_by(&self, pk: &PublicKey) -> Option<(String, PublicKey, u64)> {
        let state = self.inner.read();
        Self::live_redemption_of(&state, pk).map(|(i, r)| (i.code.clone(), i.created_by, r.at))
    }

    /// Admits a minter has spent across all of their codes.
    fn admits_by(state: &InviteState, minter: &PublicKey) -> u32 {
        state
            .invites
            .values()
            .filter(|i| i.created_by == *minter && !i.created_by_admin)
            .map(|i| i.uses())
            .sum()
    }

    fn live_codes_by(state: &InviteState, minter: &PublicKey, now: u64) -> u32 {
        state
            .invites
            .values()
            .filter(|i| i.created_by == *minter && i.is_live(now))
            .count() as u32
    }

    /// Redeem `code` for `pk`. Idempotent per pubkey: a key that already holds
    /// a live redemption consumes nothing, whichever code it presents.
    pub fn redeem(
        &self,
        code: &str,
        pk: PublicKey,
        now: u64,
    ) -> Result<RedeemOutcome, RedeemError> {
        let code = code.trim();
        if code.is_empty() || code.len() > MAX_CODE_INPUT_LEN {
            return Err(RedeemError::Unknown);
        }
        let mut state = self.inner.write();
        if Self::live_redemption_of(&state, &pk).is_some() {
            return Ok(RedeemOutcome::AlreadyAdmitted);
        }

        let (created_by, created_by_admin) = {
            let invite = state.invites.get(code).ok_or(RedeemError::Unknown)?;
            if invite.revoked_at.is_some() {
                return Err(RedeemError::Revoked);
            }
            if invite.is_expired(now) {
                return Err(RedeemError::Expired);
            }
            if invite.is_exhausted() {
                return Err(RedeemError::Exhausted);
            }
            (invite.created_by, invite.created_by_admin)
        };

        if !created_by_admin {
            let grant = state.minters.get(&created_by.to_hex()).cloned();
            match grant {
                Some(g) if Self::admits_by(&state, &created_by) < g.max_total_admits => {}
                _ => return Err(RedeemError::MinterInactive),
            }
        }

        let invite = state.invites.get_mut(code).ok_or(RedeemError::Unknown)?;
        invite.redemptions.push(Redemption {
            pubkey: pk,
            at: now,
            evicted_at: None,
        });
        Ok(RedeemOutcome::Admitted {
            invited_by: created_by,
        })
    }

    pub fn grant_of(&self, pk: &PublicKey) -> Option<MinterGrant> {
        self.inner.read().minters.get(&pk.to_hex()).cloned()
    }

    pub fn minter_status(&self, pk: &PublicKey, is_admin: bool, now: u64) -> MinterStatus {
        let state = self.inner.read();
        let grant = state.minters.get(&pk.to_hex()).cloned();
        let active_codes = Self::live_codes_by(&state, pk, now);
        let admits_used = Self::admits_by(&state, pk);
        if is_admin {
            return MinterStatus {
                can_mint: true,
                is_admin: true,
                grant,
                active_codes,
                codes_left: None,
                admits_used,
                admits_left: None,
            };
        }
        match grant {
            Some(g) => {
                let codes_left = g.max_codes.saturating_sub(active_codes);
                let admits_left = g.max_total_admits.saturating_sub(admits_used);
                MinterStatus {
                    can_mint: codes_left > 0 && admits_left > 0,
                    is_admin: false,
                    grant: Some(g),
                    active_codes,
                    codes_left: Some(codes_left),
                    admits_used,
                    admits_left: Some(admits_left),
                }
            }
            None => MinterStatus {
                can_mint: false,
                is_admin: false,
                grant: None,
                active_codes,
                codes_left: Some(0),
                admits_used,
                admits_left: Some(0),
            },
        }
    }

    /// Mint a code. Admins get the defaults unless they ask otherwise and may
    /// ask for unlimited (`0`); minters are clamped to their grant.
    pub fn mint(
        &self,
        by: PublicKey,
        is_admin: bool,
        opts: MintOptions,
        now: u64,
    ) -> Result<RelayInvite, MintError> {
        let mut state = self.inner.write();
        let (max_uses, expires_at) = if is_admin {
            let max_uses = match opts.max_uses {
                Some(0) => None,
                Some(n) => Some(n),
                None => Some(DEFAULT_RELAY_INVITE_MAX_USES),
            };
            let expires_at = match opts.expires_in {
                Some(0) => None,
                Some(secs) => Some(now.saturating_add(secs)),
                None => Some(now + DEFAULT_RELAY_INVITE_TTL_SECS),
            };
            (max_uses, expires_at)
        } else {
            let grant = state
                .minters
                .get(&by.to_hex())
                .cloned()
                .ok_or(MintError::NotAllowed)?;
            if Self::live_codes_by(&state, &by, now) >= grant.max_codes {
                return Err(MintError::CodeQuotaReached);
            }
            let admits_left = grant
                .max_total_admits
                .saturating_sub(Self::admits_by(&state, &by));
            if admits_left == 0 {
                return Err(MintError::AdmitQuotaReached);
            }
            let max_uses = opts
                .max_uses
                .filter(|n| *n > 0)
                .unwrap_or(grant.max_uses_per_code)
                .min(grant.max_uses_per_code)
                .min(admits_left);
            let ttl = opts
                .expires_in
                .filter(|s| *s > 0)
                .unwrap_or(grant.max_ttl_secs)
                .min(grant.max_ttl_secs);
            (Some(max_uses), Some(now.saturating_add(ttl)))
        };

        let mut code = generate_code();
        while state.invites.contains_key(&code) {
            code = generate_code();
        }
        let invite = RelayInvite {
            code: code.clone(),
            created_by: by,
            created_by_admin: is_admin,
            created_at: now,
            label: clean_label(opts.label),
            max_uses,
            expires_at,
            revoked_at: None,
            redemptions: Vec::new(),
        };
        state.invites.insert(code, invite.clone());
        Ok(invite)
    }

    pub fn get(&self, code: &str) -> Option<RelayInvite> {
        self.inner.read().invites.get(code).cloned()
    }

    /// Revoke a code so it admits nobody else. With `evict`, the people who
    /// already came in on it lose their invite admission too; the pubkeys
    /// evicted are returned so the caller can update the whitelist.
    pub fn revoke(&self, code: &str, evict: bool, now: u64) -> Option<Vec<PublicKey>> {
        let mut state = self.inner.write();
        let invite = state.invites.get_mut(code)?;
        if invite.revoked_at.is_none() {
            invite.revoked_at = Some(now);
        }
        let mut evicted = Vec::new();
        if evict {
            for r in invite.redemptions.iter_mut() {
                if r.evicted_at.is_none() {
                    r.evicted_at = Some(now);
                    evicted.push(r.pubkey);
                }
            }
        }
        Some(evicted)
    }

    /// Remove one invitee's invite admission. Returns false if they held none.
    pub fn evict(&self, pk: &PublicKey, now: u64) -> bool {
        let mut state = self.inner.write();
        let mut found = false;
        for invite in state.invites.values_mut() {
            for r in invite.redemptions.iter_mut() {
                if r.pubkey == *pk && r.evicted_at.is_none() {
                    r.evicted_at = Some(now);
                    found = true;
                }
            }
        }
        found
    }

    pub fn set_grant(&self, grant: MinterGrant) {
        self.inner
            .write()
            .minters
            .insert(grant.pubkey.to_hex(), grant);
    }

    /// Withdraw minting rights. Their codes stay on record but stop admitting
    /// (see `RedeemError::MinterInactive`); the people they already invited
    /// keep their access.
    pub fn remove_grant(&self, pk: &PublicKey) -> bool {
        self.inner.write().minters.remove(&pk.to_hex()).is_some()
    }

    pub fn list_invites(&self) -> Vec<RelayInvite> {
        let mut all: Vec<RelayInvite> = self.inner.read().invites.values().cloned().collect();
        all.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        all
    }

    pub fn invites_by(&self, pk: &PublicKey) -> Vec<RelayInvite> {
        let mut mine: Vec<RelayInvite> = self
            .inner
            .read()
            .invites
            .values()
            .filter(|i| i.created_by == *pk)
            .cloned()
            .collect();
        mine.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        mine
    }

    pub fn list_grants(&self) -> Vec<MinterGrant> {
        self.inner.read().minters.values().cloned().collect()
    }

    /// Who came in through whom: one branch per inviter, most prolific first.
    pub fn referrals(&self) -> Vec<ReferralBranch> {
        let state = self.inner.read();
        let mut branches: BTreeMap<String, ReferralBranch> = BTreeMap::new();
        for invite in state.invites.values() {
            let branch = branches
                .entry(invite.created_by.to_hex())
                .or_insert_with(|| ReferralBranch {
                    inviter: invite.created_by,
                    is_admin_code: invite.created_by_admin,
                    total_admitted: 0,
                    codes: Vec::new(),
                });
            branch.total_admitted += invite.uses();
            branch.codes.push(invite.clone());
        }
        let mut out: Vec<ReferralBranch> = branches.into_values().collect();
        for b in out.iter_mut() {
            b.codes.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        }
        out.sort_by(|a, b| b.total_admitted.cmp(&a.total_admitted));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    fn pk() -> PublicKey {
        Keys::generate().public_key()
    }

    fn grant(minter: PublicKey, max_codes: u32, per_code: u32, total: u32) -> MinterGrant {
        MinterGrant {
            pubkey: minter,
            granted_by: pk(),
            granted_at: NOW,
            max_codes,
            max_uses_per_code: per_code,
            max_total_admits: total,
            max_ttl_secs: 7 * 24 * 3600,
        }
    }

    fn admin_code(store: &RelayInviteStore, max_uses: u32) -> String {
        store
            .mint(
                pk(),
                true,
                MintOptions {
                    max_uses: Some(max_uses),
                    ..Default::default()
                },
                NOW,
            )
            .unwrap()
            .code
    }

    #[test]
    fn generated_codes_are_long_enough_to_be_secret() {
        let code = generate_code();
        assert_eq!(code.len(), RELAY_INVITE_CODE_LEN);
        assert!(code.bytes().all(|b| CODE_ALPHABET.contains(&b)));
    }

    #[test]
    fn redeeming_twice_with_the_same_key_costs_one_use() {
        let store = RelayInviteStore::default();
        let code = admin_code(&store, 5);
        let user = pk();
        assert!(matches!(
            store.redeem(&code, user, NOW),
            Ok(RedeemOutcome::Admitted { .. })
        ));
        assert_eq!(
            store.redeem(&code, user, NOW),
            Ok(RedeemOutcome::AlreadyAdmitted)
        );
        assert_eq!(store.get(&code).unwrap().uses(), 1);
    }

    #[test]
    fn a_new_key_with_the_stored_code_consumes_a_use() {
        // The rotation case: the client replays the code for a freshly
        // generated key. Each distinct npub is one use.
        let store = RelayInviteStore::default();
        let code = admin_code(&store, 2);
        store.redeem(&code, pk(), NOW).unwrap();
        store.redeem(&code, pk(), NOW).unwrap();
        assert_eq!(store.redeem(&code, pk(), NOW), Err(RedeemError::Exhausted));
        assert_eq!(store.get(&code).unwrap().uses(), 2);
    }

    #[test]
    fn expired_revoked_and_unknown_codes_are_refused() {
        let store = RelayInviteStore::default();
        let code = store
            .mint(
                pk(),
                true,
                MintOptions {
                    expires_in: Some(60),
                    ..Default::default()
                },
                NOW,
            )
            .unwrap()
            .code;
        assert_eq!(
            store.redeem(&code, pk(), NOW + 61),
            Err(RedeemError::Expired)
        );

        let code = admin_code(&store, 10);
        store.revoke(&code, false, NOW);
        assert_eq!(store.redeem(&code, pk(), NOW), Err(RedeemError::Revoked));

        assert_eq!(store.redeem("nope", pk(), NOW), Err(RedeemError::Unknown));
        assert_eq!(
            store.redeem(&"x".repeat(MAX_CODE_INPUT_LEN + 1), pk(), NOW),
            Err(RedeemError::Unknown)
        );
    }

    #[test]
    fn admin_codes_default_to_capped_and_expiring_but_can_be_unlimited() {
        let store = RelayInviteStore::default();
        let admin = pk();
        let default = store
            .mint(admin, true, MintOptions::default(), NOW)
            .unwrap();
        assert_eq!(default.max_uses, Some(DEFAULT_RELAY_INVITE_MAX_USES));
        assert_eq!(
            default.expires_at,
            Some(NOW + DEFAULT_RELAY_INVITE_TTL_SECS)
        );

        let open = store
            .mint(
                admin,
                true,
                MintOptions {
                    max_uses: Some(0),
                    expires_in: Some(0),
                    label: Some("  launch party  ".into()),
                },
                NOW,
            )
            .unwrap();
        assert_eq!(open.max_uses, None);
        assert_eq!(open.expires_at, None);
        assert_eq!(open.label.as_deref(), Some("launch party"));
    }

    #[test]
    fn minting_without_a_grant_is_refused() {
        let store = RelayInviteStore::default();
        assert_eq!(
            store
                .mint(pk(), false, MintOptions::default(), NOW)
                .unwrap_err(),
            MintError::NotAllowed
        );
        assert!(!store.minter_status(&pk(), false, NOW).can_mint);
    }

    #[test]
    fn minters_are_clamped_to_their_grant() {
        let store = RelayInviteStore::default();
        let minter = pk();
        store.set_grant(grant(minter, 2, 5, 8));
        let invite = store
            .mint(
                minter,
                false,
                MintOptions {
                    max_uses: Some(0),
                    expires_in: Some(0),
                    ..Default::default()
                },
                NOW,
            )
            .unwrap();
        assert_eq!(
            invite.max_uses,
            Some(5),
            "0 does not mean unlimited for a minter"
        );
        assert_eq!(invite.expires_at, Some(NOW + 7 * 24 * 3600));

        let big = store
            .mint(
                minter,
                false,
                MintOptions {
                    max_uses: Some(1000),
                    ..Default::default()
                },
                NOW,
            )
            .unwrap();
        assert_eq!(big.max_uses, Some(5));

        assert_eq!(
            store
                .mint(minter, false, MintOptions::default(), NOW)
                .unwrap_err(),
            MintError::CodeQuotaReached
        );
        let status = store.minter_status(&minter, false, NOW);
        assert_eq!(status.active_codes, 2);
        assert_eq!(status.codes_left, Some(0));
        assert!(!status.can_mint);
    }

    #[test]
    fn a_spent_admit_quota_stops_every_code_the_minter_made() {
        let store = RelayInviteStore::default();
        let minter = pk();
        store.set_grant(grant(minter, 3, 5, 3));
        let a = store
            .mint(minter, false, MintOptions::default(), NOW)
            .unwrap()
            .code;
        let b = store
            .mint(minter, false, MintOptions::default(), NOW)
            .unwrap()
            .code;
        store.redeem(&a, pk(), NOW).unwrap();
        store.redeem(&a, pk(), NOW).unwrap();
        store.redeem(&b, pk(), NOW).unwrap();
        assert_eq!(
            store.redeem(&b, pk(), NOW),
            Err(RedeemError::MinterInactive)
        );
        assert_eq!(
            store
                .mint(minter, false, MintOptions::default(), NOW)
                .unwrap_err(),
            MintError::AdmitQuotaReached
        );
    }

    #[test]
    fn withdrawing_a_grant_stops_codes_but_keeps_invitees() {
        let store = RelayInviteStore::default();
        let minter = pk();
        store.set_grant(grant(minter, 3, 5, 10));
        let code = store
            .mint(minter, false, MintOptions::default(), NOW)
            .unwrap()
            .code;
        let invitee = pk();
        store.redeem(&code, invitee, NOW).unwrap();
        assert!(store.remove_grant(&minter));
        assert_eq!(
            store.redeem(&code, pk(), NOW),
            Err(RedeemError::MinterInactive)
        );
        assert!(store.admitted_pubkeys().contains(&invitee));
    }

    #[test]
    fn revoking_with_evict_removes_the_invitees() {
        let store = RelayInviteStore::default();
        let code = admin_code(&store, 10);
        let (a, b) = (pk(), pk());
        store.redeem(&code, a, NOW).unwrap();
        store.redeem(&code, b, NOW).unwrap();
        let evicted = store.revoke(&code, true, NOW).unwrap();
        assert_eq!(evicted.len(), 2);
        assert!(store.admitted_pubkeys().is_empty());
        assert!(store.invited_by(&a).is_none());
        // The history and the spent uses remain.
        assert_eq!(store.get(&code).unwrap().uses(), 2);
    }

    #[test]
    fn an_evicted_key_can_be_readmitted_by_another_code() {
        let store = RelayInviteStore::default();
        let first = admin_code(&store, 10);
        let second = admin_code(&store, 10);
        let user = pk();
        store.redeem(&first, user, NOW).unwrap();
        assert!(store.evict(&user, NOW));
        assert!(matches!(
            store.redeem(&second, user, NOW),
            Ok(RedeemOutcome::Admitted { .. })
        ));
        assert_eq!(store.invited_by(&user).unwrap().0, second);
    }

    #[test]
    fn referrals_attribute_each_invitee_to_their_inviter() {
        let store = RelayInviteStore::default();
        let minter = pk();
        store.set_grant(grant(minter, 3, 5, 10));
        let code = store
            .mint(minter, false, MintOptions::default(), NOW)
            .unwrap()
            .code;
        let invitee = pk();
        store.redeem(&code, invitee, NOW).unwrap();
        let tree = store.referrals();
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].inviter, minter);
        assert_eq!(tree[0].total_admitted, 1);
        assert_eq!(tree[0].codes[0].redemptions[0].pubkey, invitee);
        assert_eq!(store.invited_by(&invitee).unwrap().1, minter);
    }

    #[test]
    fn state_survives_a_persist_and_reload() {
        let dir = tempfile::tempdir().unwrap();
        let store = RelayInviteStore::new(Some(dir.path()));
        let minter = pk();
        store.set_grant(grant(minter, 3, 5, 10));
        let code = store
            .mint(minter, false, MintOptions::default(), NOW)
            .unwrap()
            .code;
        let invitee = pk();
        store.redeem(&code, invitee, NOW).unwrap();
        store.persist(dir.path()).unwrap();
        assert!(!dir
            .path()
            .join(format!("{RELAY_INVITES_FILE}.tmp"))
            .exists());

        let reloaded = RelayInviteStore::new(Some(dir.path()));
        assert_eq!(reloaded.get(&code).unwrap().uses(), 1);
        assert_eq!(reloaded.grant_of(&minter), store.grant_of(&minter));
        assert_eq!(reloaded.admitted_pubkeys(), vec![invitee]);
    }
}
