use crate::obelisk_index::{MessageScopeResolution, ObeliskReadContext};
use crate::server::ServerState;
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    Json,
};
use nostr_sdk::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const NIP98_AUTH_KIND: u16 = 27235;
const NIP98_FRESHNESS_SECS: u64 = 60;
const RATE_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, Default)]
pub struct ObeliskHttpLimiter {
    requests: Mutex<HashMap<(String, ObeliskQuotaKind), VecDeque<Instant>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObeliskQuotaKind {
    Bootstrap,
    Messages,
}

#[derive(Debug, Deserialize)]
pub struct BootstrapQuery {
    limit_per_group: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct MessagesQuery {
    before: Option<u64>,
    limit: Option<usize>,
    scope: Option<String>,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

impl ObeliskHttpLimiter {
    pub fn check(
        &self,
        pubkey: &PublicKey,
        kind: ObeliskQuotaKind,
        per_minute: u32,
    ) -> Option<u64> {
        if per_minute == 0 {
            return None;
        }

        let now = Instant::now();
        let key = (pubkey.to_hex(), kind);
        let mut requests = self.requests.lock().ok()?;
        let bucket = requests.entry(key).or_default();

        while bucket
            .front()
            .is_some_and(|instant| now.duration_since(*instant) > RATE_WINDOW)
        {
            bucket.pop_front();
        }

        if bucket.len() >= per_minute as usize {
            let retry_after = bucket
                .front()
                .map(|instant| {
                    RATE_WINDOW
                        .saturating_sub(now.duration_since(*instant))
                        .as_secs()
                        .max(1)
                })
                .unwrap_or(1);
            return Some(retry_after);
        }

        bucket.push_back(now);
        None
    }
}

pub async fn handle_bootstrap(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    uri: Uri,
    Query(query): Query<BootstrapQuery>,
) -> Response {
    let Some(index) = state.obelisk_index.as_ref() else {
        return error(StatusCode::NOT_FOUND, "Obelisk index is disabled");
    };

    let context = match authenticate_nip98(&state, &headers, &uri, "GET") {
        Ok(context) => context,
        Err(response) => return response,
    };

    if let Some(retry_after) = state.obelisk_http_limiter.check(
        &context.pubkey,
        ObeliskQuotaKind::Bootstrap,
        index.settings().bootstrap_requests_per_minute,
    ) {
        return rate_limited(retry_after);
    }

    Json(index.bootstrap(&state.relay_url, &context, query.limit_per_group)).into_response()
}

pub async fn handle_messages(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    uri: Uri,
    Path(group_id): Path<String>,
    Query(query): Query<MessagesQuery>,
) -> Response {
    let Some(index) = state.obelisk_index.as_ref() else {
        return error(StatusCode::NOT_FOUND, "Obelisk index is disabled");
    };

    let context = match authenticate_nip98(&state, &headers, &uri, "GET") {
        Ok(context) => context,
        Err(response) => return response,
    };

    if let Some(retry_after) = state.obelisk_http_limiter.check(
        &context.pubkey,
        ObeliskQuotaKind::Messages,
        index.settings().message_requests_per_minute,
    ) {
        return rate_limited(retry_after);
    }

    let scope = match index.resolve_message_scope(&group_id, query.scope.as_deref()) {
        MessageScopeResolution::Found(scope) => scope,
        MessageScopeResolution::NotFound => return error(StatusCode::NOT_FOUND, "Group not found"),
        MessageScopeResolution::Ambiguous => {
            return error(
                StatusCode::CONFLICT,
                "Group id exists in multiple scopes; pass ?scope=<scope>",
            )
        }
    };

    match index
        .messages_for_group(&scope, &group_id, &context, query.before, query.limit)
        .await
    {
        Ok(Some(response)) => Json(response).into_response(),
        Ok(None) => error(StatusCode::FORBIDDEN, "Access denied"),
        Err(err) => error(StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
    }
}

fn authenticate_nip98(
    state: &ServerState,
    headers: &HeaderMap,
    uri: &Uri,
    method: &str,
) -> Result<ObeliskReadContext, Response> {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| {
            error(
                StatusCode::UNAUTHORIZED,
                "Missing NIP-98 Authorization header",
            )
        })?;
    let encoded = auth
        .split_once(' ')
        .and_then(|(scheme, value)| scheme.eq_ignore_ascii_case("Nostr").then_some(value.trim()))
        .ok_or_else(|| {
            error(
                StatusCode::UNAUTHORIZED,
                "Expected Authorization: Nostr <event>",
            )
        })?;
    let auth_bytes = decode_base64(encoded)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "Invalid NIP-98 auth encoding"))?;
    let event_json = String::from_utf8(auth_bytes)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "Invalid NIP-98 auth encoding"))?;
    let event = Event::from_json(&event_json)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "Invalid NIP-98 auth event"))?;

    if event.kind != Kind::from(NIP98_AUTH_KIND) {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "NIP-98 event must be kind 27235",
        ));
    }
    if event.verify().is_err() {
        return Err(error(StatusCode::BAD_REQUEST, "Invalid NIP-98 signature"));
    }

    let now = unix_now();
    let created_at = event.created_at.as_secs();
    if now.abs_diff(created_at) > NIP98_FRESHNESS_SECS {
        return Err(error(
            StatusCode::UNAUTHORIZED,
            "NIP-98 auth event is stale",
        ));
    }

    let expected_url = expected_http_url(state, uri);
    if tag_value(&event, "u") != Some(expected_url.as_str()) {
        return Err(error(StatusCode::UNAUTHORIZED, "NIP-98 URL tag mismatch"));
    }
    if tag_value(&event, "method") != Some(method) {
        return Err(error(
            StatusCode::UNAUTHORIZED,
            "NIP-98 method tag mismatch",
        ));
    }

    if !state.whitelist.is_empty()
        && !state.whitelist.contains(&event.pubkey)
        && event.pubkey != state.relay_public_key
        && !state.admin_pubkeys.contains(&event.pubkey)
    {
        return Err(error(
            StatusCode::FORBIDDEN,
            "Pubkey is not whitelisted on this relay",
        ));
    }

    Ok(ObeliskReadContext {
        pubkey: event.pubkey,
        relay_pubkey: state.relay_public_key,
        admin_pubkeys: state.admin_pubkeys.clone(),
    })
}

fn expected_http_url(state: &ServerState, uri: &Uri) -> String {
    let base = state
        .relay_url
        .trim_end_matches('/')
        .replacen("wss://", "https://", 1)
        .replacen("ws://", "http://", 1);
    let path_and_query = uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or("/");
    format!("{base}{path_and_query}")
}

fn tag_value<'a>(event: &'a Event, name: &str) -> Option<&'a str> {
    event
        .tags
        .iter()
        .find(|tag| tag.as_slice().first().map(String::as_str) == Some(name))
        .and_then(|tag| tag.as_slice().get(1).map(String::as_str))
}

fn decode_base64(input: &str) -> Result<Vec<u8>, ()> {
    let mut output = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0u8;

    for byte in input.bytes() {
        if byte == b'=' {
            break;
        }
        let Some(value) = base64_value(byte) else {
            return Err(());
        };
        buffer = (buffer << 6) | value as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((buffer >> bits) as u8);
            buffer &= (1u32 << bits) - 1;
        }
    }

    Ok(output)
}

fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' | b'-' => Some(62),
        b'/' | b'_' => Some(63),
        _ => None,
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        Json(ErrorResponse {
            error: message.into(),
        }),
    )
        .into_response()
}

fn rate_limited(retry_after: u64) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, retry_after.to_string())],
        Json(ErrorResponse {
            error: "Rate limited".to_string(),
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_standard_base64() {
        let decoded = decode_base64("eyJraW5kIjoyNzIzNX0=").unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), "{\"kind\":27235}");
    }

    #[test]
    fn limiter_returns_retry_after_when_exhausted() {
        let limiter = ObeliskHttpLimiter::default();
        let pubkey = Keys::generate().public_key();

        assert_eq!(limiter.check(&pubkey, ObeliskQuotaKind::Bootstrap, 1), None);
        assert!(limiter
            .check(&pubkey, ObeliskQuotaKind::Bootstrap, 1)
            .is_some());
    }
}
