//! HTTP API for relay invitation codes. See `relay_invites` for the model and
//! docs/relay-invites.md for the protocol.
//!
//! Two audiences, two auth schemes:
//!
//! - `/api/invites/*` is for clients (obelisk-dex, obelisk-sky). Authenticated
//!   per request with NIP-98, because the people calling it are ordinary users
//!   who will never hold an admin session. `GET /me` answers any signer --
//!   including one that is not admitted yet -- so a client can decide whether to
//!   show "Invite people" without knowing anything about grants.
//! - `/api/admin/relay-invites/*` is for the console, behind the admin session
//!   like every other admin route.
//!
//! Redemption is not here. It happens over the websocket, as a NIP-43 kind
//! 28934 claim, in `GroupsRelayProcessor::handle_relay_claim`.

use crate::admin::{admin_session_pubkey, is_relay_admin};
use crate::obelisk_api::{verify_nip98, ObeliskQuotaKind};
use crate::relay_invites::{unix_now, MintOptions, MinterGrant, RelayInvite};
use crate::server::ServerState;
use axum::{
    extract::{OriginalUri, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get},
    Json, Router,
};
use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::warn;

/// Per-pubkey budget for the client API. A settings panel makes a handful of
/// calls; this only exists so a script cannot spin on mint.
const INVITE_REQUESTS_PER_MINUTE: u32 = 30;

/// Defaults for a grant when the console leaves a field out. Small on purpose:
/// widening a grant is one click, recovering from a leaked generous one is not.
const DEFAULT_GRANT_MAX_CODES: u32 = 5;
const DEFAULT_GRANT_USES_PER_CODE: u32 = 10;
const DEFAULT_GRANT_TOTAL_ADMITS: u32 = 25;
const DEFAULT_GRANT_TTL_DAYS: u64 = 30;

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(ErrorBody {
            error: message.into(),
        }),
    )
        .into_response()
}

fn npub(pk: &PublicKey) -> String {
    pk.to_bech32().unwrap_or_else(|_| pk.to_hex())
}

fn parse_pubkey(input: &str) -> Option<PublicKey> {
    let input = input.trim();
    if input.starts_with("npub") {
        PublicKey::from_bech32(input).ok()
    } else {
        PublicKey::from_hex(input).ok()
    }
}

fn persist(state: &ServerState) {
    if let Err(e) = state
        .relay_invites
        .persist(std::path::Path::new(&state.config_dir))
    {
        warn!("Failed to persist relay invites: {}", e);
    }
}

#[derive(Serialize)]
struct RedemptionView {
    pubkey: String,
    npub: String,
    at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    evicted_at: Option<u64>,
}

#[derive(Serialize)]
struct InviteView {
    code: String,
    label: Option<String>,
    created_by: String,
    created_by_npub: String,
    created_by_admin: bool,
    created_at: u64,
    max_uses: Option<u32>,
    uses: u32,
    expires_at: Option<u64>,
    revoked_at: Option<u64>,
    /// `active`, `revoked`, `expired` or `exhausted`.
    status: &'static str,
    redemptions: Vec<RedemptionView>,
}

impl InviteView {
    fn from(invite: &RelayInvite, now: u64) -> Self {
        Self {
            code: invite.code.clone(),
            label: invite.label.clone(),
            created_by: invite.created_by.to_hex(),
            created_by_npub: npub(&invite.created_by),
            created_by_admin: invite.created_by_admin,
            created_at: invite.created_at,
            max_uses: invite.max_uses,
            uses: invite.uses(),
            expires_at: invite.expires_at,
            revoked_at: invite.revoked_at,
            status: invite.status(now),
            redemptions: invite
                .redemptions
                .iter()
                .map(|r| RedemptionView {
                    pubkey: r.pubkey.to_hex(),
                    npub: npub(&r.pubkey),
                    at: r.at,
                    evicted_at: r.evicted_at,
                })
                .collect(),
        }
    }
}

#[derive(Serialize)]
struct GrantView {
    pubkey: String,
    npub: String,
    granted_by: String,
    granted_at: u64,
    max_codes: u32,
    max_uses_per_code: u32,
    max_total_admits: u32,
    max_ttl_secs: u64,
}

impl From<&MinterGrant> for GrantView {
    fn from(g: &MinterGrant) -> Self {
        Self {
            pubkey: g.pubkey.to_hex(),
            npub: npub(&g.pubkey),
            granted_by: g.granted_by.to_hex(),
            granted_at: g.granted_at,
            max_codes: g.max_codes,
            max_uses_per_code: g.max_uses_per_code,
            max_total_admits: g.max_total_admits,
            max_ttl_secs: g.max_ttl_secs,
        }
    }
}

// ---------------------------------------------------------------------------
// Client API (NIP-98)
// ---------------------------------------------------------------------------

pub fn minter_routes() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/me", get(handle_me))
        .route("/", get(handle_my_invites).post(handle_mint))
        .route("/{code}", delete(handle_revoke_mine))
}

/// Verify NIP-98 and apply the per-pubkey budget.
fn authenticate(
    state: &ServerState,
    headers: &HeaderMap,
    uri: &OriginalUri,
    method: &str,
) -> Result<PublicKey, Response> {
    let pubkey = verify_nip98(state, headers, &uri.0, method)?;
    if state
        .obelisk_http_limiter
        .check(
            &pubkey,
            ObeliskQuotaKind::Invites,
            INVITE_REQUESTS_PER_MINUTE,
        )
        .is_some()
    {
        return Err(error(StatusCode::TOO_MANY_REQUESTS, "Rate limited"));
    }
    if state.whitelist.blacklist().contains(&pubkey) {
        return Err(error(StatusCode::FORBIDDEN, "This pubkey is blocked"));
    }
    Ok(pubkey)
}

#[derive(Serialize)]
struct MeResponse {
    pubkey: String,
    /// Whether this relay admits the signer at all, by any tier.
    admitted: bool,
    /// Present when the signer came in on a code.
    invited_by: Option<InvitedBy>,
    can_mint: bool,
    is_admin: bool,
    /// Codes that may still be created right now; `null` for admins.
    codes_left: Option<u32>,
    /// People that may still be brought in; `null` for admins.
    admits_left: Option<u32>,
    admits_used: u32,
    grant: Option<GrantView>,
    /// The signer's own codes, newest first, with who joined on each.
    codes: Vec<InviteView>,
}

#[derive(Serialize)]
struct InvitedBy {
    code: String,
    pubkey: String,
    npub: String,
    at: u64,
}

async fn handle_me(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    uri: OriginalUri,
) -> Response {
    let pk = match authenticate(&state, &headers, &uri, "GET") {
        Ok(pk) => pk,
        Err(r) => return r,
    };
    let now = unix_now();
    let is_admin = is_relay_admin(&state, &pk);
    let status = state.relay_invites.minter_status(&pk, is_admin, now);
    let invited_by = state
        .relay_invites
        .invited_by(&pk)
        .map(|(code, inviter, at)| InvitedBy {
            code,
            pubkey: inviter.to_hex(),
            npub: npub(&inviter),
            at,
        });
    let codes = state
        .relay_invites
        .invites_by(&pk)
        .iter()
        .map(|i| InviteView::from(i, now))
        .collect();
    Json(MeResponse {
        pubkey: pk.to_hex(),
        admitted: state
            .whitelist
            .admits(Some(&pk), |p| is_relay_admin(&state, p)),
        invited_by,
        can_mint: status.can_mint,
        is_admin: status.is_admin,
        codes_left: status.codes_left,
        admits_left: status.admits_left,
        admits_used: status.admits_used,
        grant: status.grant.as_ref().map(GrantView::from),
        codes,
    })
    .into_response()
}

async fn handle_my_invites(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    uri: OriginalUri,
) -> Response {
    let pk = match authenticate(&state, &headers, &uri, "GET") {
        Ok(pk) => pk,
        Err(r) => return r,
    };
    let now = unix_now();
    let codes: Vec<InviteView> = state
        .relay_invites
        .invites_by(&pk)
        .iter()
        .map(|i| InviteView::from(i, now))
        .collect();
    Json(codes).into_response()
}

#[derive(Deserialize, Default)]
struct MintRequest {
    label: Option<String>,
    max_uses: Option<u32>,
    /// Seconds.
    expires_in: Option<u64>,
}

async fn handle_mint(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    uri: OriginalUri,
    body: Option<Json<MintRequest>>,
) -> Response {
    let pk = match authenticate(&state, &headers, &uri, "POST") {
        Ok(pk) => pk,
        Err(r) => return r,
    };
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let now = unix_now();
    let is_admin = is_relay_admin(&state, &pk);
    match state.relay_invites.mint(
        pk,
        is_admin,
        MintOptions {
            label: req.label,
            max_uses: req.max_uses,
            expires_in: req.expires_in,
        },
        now,
    ) {
        Ok(invite) => {
            persist(&state);
            (StatusCode::CREATED, Json(InviteView::from(&invite, now))).into_response()
        }
        Err(e) => error(StatusCode::FORBIDDEN, e.message()),
    }
}

async fn handle_revoke_mine(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    uri: OriginalUri,
    Path(code): Path<String>,
) -> Response {
    let pk = match authenticate(&state, &headers, &uri, "DELETE") {
        Ok(pk) => pk,
        Err(r) => return r,
    };
    let Some(invite) = state.relay_invites.get(&code) else {
        return error(StatusCode::NOT_FOUND, "No such invite code");
    };
    if invite.created_by != pk && !is_relay_admin(&state, &pk) {
        return error(StatusCode::FORBIDDEN, "You can only revoke your own codes");
    }
    // A minter can stop their code; only an admin can take access away from
    // the people who already used it.
    state.relay_invites.revoke(&code, false, unix_now());
    persist(&state);
    StatusCode::NO_CONTENT.into_response()
}

// ---------------------------------------------------------------------------
// Admin console API (admin session)
// ---------------------------------------------------------------------------

pub fn admin_routes() -> Router<Arc<ServerState>> {
    Router::new()
        .route("/", get(handle_admin_list).post(handle_admin_mint))
        .route("/codes/{code}", delete(handle_admin_revoke))
        .route(
            "/minters",
            get(handle_admin_minters).post(handle_admin_grant),
        )
        .route("/minters/{pubkey}", delete(handle_admin_ungrant))
        .route("/referrals", get(handle_admin_referrals))
        .route("/invitees/{pubkey}", delete(handle_admin_evict))
}

fn require_admin(state: &ServerState, headers: &HeaderMap) -> Result<PublicKey, Response> {
    admin_session_pubkey(state, headers)
        .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "Unauthorized"))
}

#[derive(Serialize)]
struct AdminListResponse {
    invites: Vec<InviteView>,
    minters: Vec<GrantView>,
    active_codes: usize,
    total_admitted: usize,
}

async fn handle_admin_list(State(state): State<Arc<ServerState>>, headers: HeaderMap) -> Response {
    if let Err(r) = require_admin(&state, &headers) {
        return r;
    }
    let now = unix_now();
    let invites: Vec<InviteView> = state
        .relay_invites
        .list_invites()
        .iter()
        .map(|i| InviteView::from(i, now))
        .collect();
    let active_codes = invites.iter().filter(|i| i.status == "active").count();
    Json(AdminListResponse {
        active_codes,
        total_admitted: state.relay_invites.admitted_pubkeys().len(),
        minters: state
            .relay_invites
            .list_grants()
            .iter()
            .map(GrantView::from)
            .collect(),
        invites,
    })
    .into_response()
}

async fn handle_admin_mint(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    body: Option<Json<MintRequest>>,
) -> Response {
    let admin = match require_admin(&state, &headers) {
        Ok(pk) => pk,
        Err(r) => return r,
    };
    let req = body.map(|Json(b)| b).unwrap_or_default();
    let now = unix_now();
    match state.relay_invites.mint(
        admin,
        true,
        MintOptions {
            label: req.label,
            max_uses: req.max_uses,
            expires_in: req.expires_in,
        },
        now,
    ) {
        Ok(invite) => {
            persist(&state);
            (StatusCode::CREATED, Json(InviteView::from(&invite, now))).into_response()
        }
        Err(e) => error(StatusCode::BAD_REQUEST, e.message()),
    }
}

#[derive(Deserialize)]
struct RevokeQuery {
    /// Also withdraw invite admission from everyone who used the code.
    #[serde(default)]
    evict: bool,
}

async fn handle_admin_revoke(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(code): Path<String>,
    Query(q): Query<RevokeQuery>,
) -> Response {
    if let Err(r) = require_admin(&state, &headers) {
        return r;
    }
    let Some(evicted) = state.relay_invites.revoke(&code, q.evict, unix_now()) else {
        return error(StatusCode::NOT_FOUND, "No such invite code");
    };
    for pk in &evicted {
        // Only drop the hot-path entry if no other live redemption holds it.
        if state.relay_invites.invited_by(pk).is_none() {
            state.whitelist.remove_invited(pk);
        }
    }
    persist(&state);
    Json(serde_json::json!({ "revoked": code, "evicted": evicted.len() })).into_response()
}

async fn handle_admin_minters(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = require_admin(&state, &headers) {
        return r;
    }
    let now = unix_now();
    #[derive(Serialize)]
    struct MinterRow {
        #[serde(flatten)]
        grant: GrantView,
        active_codes: u32,
        admits_used: u32,
    }
    let rows: Vec<MinterRow> = state
        .relay_invites
        .list_grants()
        .iter()
        .map(|g| {
            let status = state.relay_invites.minter_status(&g.pubkey, false, now);
            MinterRow {
                grant: GrantView::from(g),
                active_codes: status.active_codes,
                admits_used: status.admits_used,
            }
        })
        .collect();
    Json(rows).into_response()
}

#[derive(Deserialize)]
struct GrantRequest {
    pubkey: String,
    max_codes: Option<u32>,
    max_uses_per_code: Option<u32>,
    max_total_admits: Option<u32>,
    max_ttl_days: Option<u64>,
}

async fn handle_admin_grant(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Json(req): Json<GrantRequest>,
) -> Response {
    let admin = match require_admin(&state, &headers) {
        Ok(pk) => pk,
        Err(r) => return r,
    };
    let Some(pk) = parse_pubkey(&req.pubkey) else {
        return error(StatusCode::BAD_REQUEST, "Not a valid npub or hex pubkey");
    };
    let existing = state.relay_invites.grant_of(&pk);
    let grant = MinterGrant {
        pubkey: pk,
        granted_by: existing.as_ref().map(|g| g.granted_by).unwrap_or(admin),
        granted_at: existing
            .as_ref()
            .map(|g| g.granted_at)
            .unwrap_or_else(unix_now),
        max_codes: req
            .max_codes
            .or(existing.as_ref().map(|g| g.max_codes))
            .unwrap_or(DEFAULT_GRANT_MAX_CODES)
            .max(1),
        max_uses_per_code: req
            .max_uses_per_code
            .or(existing.as_ref().map(|g| g.max_uses_per_code))
            .unwrap_or(DEFAULT_GRANT_USES_PER_CODE)
            .max(1),
        max_total_admits: req
            .max_total_admits
            .or(existing.as_ref().map(|g| g.max_total_admits))
            .unwrap_or(DEFAULT_GRANT_TOTAL_ADMITS)
            .max(1),
        max_ttl_secs: req
            .max_ttl_days
            .map(|d| d.max(1) * 86_400)
            .or(existing.as_ref().map(|g| g.max_ttl_secs))
            .unwrap_or(DEFAULT_GRANT_TTL_DAYS * 86_400),
    };
    state.relay_invites.set_grant(grant.clone());
    persist(&state);
    Json(GrantView::from(&grant)).into_response()
}

async fn handle_admin_ungrant(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(pubkey): Path<String>,
) -> Response {
    if let Err(r) = require_admin(&state, &headers) {
        return r;
    }
    let Some(pk) = parse_pubkey(&pubkey) else {
        return error(StatusCode::BAD_REQUEST, "Not a valid npub or hex pubkey");
    };
    if !state.relay_invites.remove_grant(&pk) {
        return error(StatusCode::NOT_FOUND, "That pubkey has no minting grant");
    }
    persist(&state);
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Serialize)]
struct ReferralView {
    inviter: String,
    inviter_npub: String,
    is_admin_code: bool,
    /// Whether the inviter currently holds a grant (admins never need one).
    has_grant: bool,
    /// Whether the inviter was themselves invited, and by whom: the next link
    /// up the chain.
    invited_by: Option<String>,
    total_admitted: u32,
    codes: Vec<InviteView>,
}

async fn handle_admin_referrals(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> Response {
    if let Err(r) = require_admin(&state, &headers) {
        return r;
    }
    let now = unix_now();
    let rows: Vec<ReferralView> = state
        .relay_invites
        .referrals()
        .iter()
        .map(|b| ReferralView {
            inviter: b.inviter.to_hex(),
            inviter_npub: npub(&b.inviter),
            is_admin_code: b.is_admin_code,
            has_grant: state.relay_invites.grant_of(&b.inviter).is_some(),
            invited_by: state
                .relay_invites
                .invited_by(&b.inviter)
                .map(|(_, by, _)| by.to_hex()),
            total_admitted: b.total_admitted,
            codes: b.codes.iter().map(|i| InviteView::from(i, now)).collect(),
        })
        .collect();
    Json(rows).into_response()
}

async fn handle_admin_evict(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Path(pubkey): Path<String>,
) -> Response {
    if let Err(r) = require_admin(&state, &headers) {
        return r;
    }
    let Some(pk) = parse_pubkey(&pubkey) else {
        return error(StatusCode::BAD_REQUEST, "Not a valid npub or hex pubkey");
    };
    if !state.relay_invites.evict(&pk, unix_now()) {
        return error(
            StatusCode::NOT_FOUND,
            "That pubkey was not admitted by an invite",
        );
    }
    state.whitelist.remove_invited(&pk);
    persist(&state);
    StatusCode::NO_CONTENT.into_response()
}
