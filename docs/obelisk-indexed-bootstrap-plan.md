# Obelisk Indexed Bootstrap Plan

## Summary

Obelisk should keep full standard Nostr relay compatibility while adding an
Obelisk-specific optimized read path for clients that know how to use it.

The relay remains the canonical event store: it accepts, validates, stores, and
serves normal Nostr/NIP-29 events over WebSocket for all standard clients. In
parallel, `obelisk-relay` maintains a grouped, time-ordered, rebuildable index
derived from those same events. Obelisk clients detect the optimized capability
through NIP-11 and fetch a single authenticated bootstrap snapshot instead of
opening many per-group subscriptions during startup.

Implementation starts on May 30, 2026.

## Goals

- Preserve normal relay behavior for standard Nostr clients.
- Preserve Obelisk client fallback support for normal relays.
- Reduce startup fan-out from many WebSocket `REQ`s to one optimized bootstrap
  read when connected to an Obelisk relay.
- Keep LMDB as the durable source of truth; the optimized index must be
  rebuildable.
- Use raw Nostr events in optimized responses so existing client ingest logic
  remains the compatibility layer.

## Public Interface

### NIP-11 Capability

`obelisk-relay` should advertise an Obelisk capability in the existing NIP-11
relay information response:

```json
{
  "obelisk": {
    "indexed_bootstrap": {
      "version": 1,
      "url": "/api/obelisk/v1/bootstrap",
      "auth": "nip98"
    }
  }
}
```

The relay should keep standard `supported_nips` behavior intact. If the
authenticated HTTP API is enabled, the relay can also advertise NIP-98 support.

### HTTP APIs

Add authenticated HTTP endpoints:

```http
GET /api/obelisk/v1/bootstrap?limit_per_group=50
GET /api/obelisk/v1/groups/{group_id}/messages?before=<unix>&limit=50
```

The bootstrap response should group raw Nostr events by scope and group. The
first version should include:

- Group metadata events: `39000`
- Group admin/member events: `39001`, `39002`
- Recent group messages: kind `9`
- Reactions: kind `7`
- Author deletions and moderation deletions: kinds `5`, `9005`
- Active call announcements currently used by the client
- Cursors/high-water timestamps for follow-up live subscriptions

The message pagination endpoint should return older raw message events for one
group, newest-first, using `before` and `limit`.

### Authentication

Use NIP-98 HTTP authentication:

- Obelisk clients sign kind `27235` auth events.
- The auth event must include `u` and `method` tags matching the request.
- The relay verifies signature, timestamp freshness, URL, method, whitelist
  membership, and per-group visibility.
- Logged-in Obelisk clients should send NIP-98 even when reading public groups.
- Public-only deployments may allow unauthenticated public bootstrap data, but
  private or whitelisted data must require NIP-98.

## Relay Implementation

Add an `ObeliskIndex` subsystem to `obelisk-relay`.

The index should be keyed by `(Scope, group_id)` and maintain:

- Latest state events for group metadata, admins, and members.
- Recent message windows sorted by `(created_at, id)`.
- Recent reactions and deletion/moderation events.
- Active-call events needed by Obelisk startup.
- A high-water cursor for live catch-up after bootstrap.

Build the index from LMDB at startup across all scopes. Update it whenever the
accepted event path emits store commands, including relay-generated unsigned
state events. Periodically reconcile the index from LMDB so it can recover from
rare drift, restarts, and pruning.

Suggested config defaults:

```yaml
relay:
  obelisk_index:
    enabled: true
    recent_per_group: 50
    max_bootstrap_groups: 500
    reconcile_interval: "5m"
```

The index is an additive optimization. It must not become the authority for
validation, persistence, or standard Nostr query semantics.

## Client Implementation

Extend `obelisk` NIP-11 parsing to read the custom Obelisk capability.

During `BridgeImpl.connect()`:

1. Fetch relay information.
2. Detect `obelisk.indexed_bootstrap.version === 1`.
3. Sign a NIP-98 request with the active login method.
4. Call the bootstrap endpoint.
5. Feed returned raw events through existing ingest functions.
6. Mark metadata, membership, and message readiness from the snapshot.
7. Open a small live standard Nostr subscription from the bootstrap cursor.

If capability detection, auth, HTTP, validation, or ingestion fails, the client
must fall back to the current `SimplePool` subscription and `querySync` behavior.

`loadMoreMessages(groupId)` should use the optimized message pagination endpoint
when available and fall back to the existing Nostr `querySync` path otherwise.

## Implementation Order

1. Add relay capability config and NIP-11 advertisement.
2. Add NIP-98 verification helpers and tests in the relay.
3. Build `ObeliskIndex` from LMDB at startup.
4. Wire live index updates from accepted store commands.
5. Expose bootstrap and message pagination endpoints.
6. Extend Obelisk relay-info parsing and capability storage.
7. Add client bootstrap fetch/auth/ingest flow with fallback.
8. Replace optimized `loadMoreMessages` path.
9. Add integration tests and run a local relay/client cold-login check.

## Test Plan

Relay tests:

- NIP-11 advertises the capability only when enabled.
- NIP-98 accepts valid requests and rejects stale, wrong-method, wrong-URL, and
  invalid-signature requests.
- Bootstrap filters private and whitelisted group data correctly.
- Index rebuild from LMDB matches live index updates.
- Create/edit/member/message/reaction/delete events update the index.
- Pagination returns older messages in stable newest-first order.

Client tests:

- Capability detection chooses optimized bootstrap when advertised.
- Missing or invalid capability falls back to normal Nostr subscriptions.
- NIP-98 auth headers are signed using nsec, NIP-07, and NIP-46 paths where
  practical to cover.
- Bootstrap raw events populate the existing stores without duplicates.
- `loadMoreMessages` uses HTTP pagination when available and `querySync`
  otherwise.

Manual validation:

- Obelisk relay plus Obelisk client cold login uses one active relay WebSocket
  and avoids per-group message fan-out.
- A standard Nostr client can still subscribe and publish normally.
- Obelisk can still connect to non-Obelisk relays.

## Assumptions

- First optimized scope is chat bootstrap, not a full all-events export.
- HTTP snapshot and pagination are the first optimized transport.
- NIP-98 is the authentication model for optimized HTTP reads.
- Optimized responses carry raw Nostr events, not client-specific DTOs.
- LMDB remains the durable source of truth.
