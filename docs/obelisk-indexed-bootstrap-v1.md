# Obelisk Indexed Bootstrap v1

This document is the client-facing protocol for Obelisk-compatible relays that
support optimized startup reads. The feature is additive: normal Nostr
WebSocket `REQ`, `EVENT`, `CLOSE`, and `AUTH` behavior remains unchanged.

## Discovery

Fetch NIP-11 relay information with:

```http
GET /
Accept: application/nostr+json
```

An Obelisk-compatible relay advertises:

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

Clients must use this optimized path only when `version` is `1`. If the field
is missing or unsupported, use normal Nostr subscriptions and queries.

## Authentication

Optimized HTTP reads use NIP-98.

The client signs a kind `27235` event:

```json
{
  "kind": 27235,
  "content": "",
  "tags": [
    ["u", "https://relay.example.com/api/obelisk/v1/bootstrap?limit_per_group=50"],
    ["method", "GET"]
  ]
}
```

Send the signed event as:

```http
Authorization: Nostr <base64-encoded-event-json>
```

The relay verifies signature, freshness, exact URL, method, whitelist/admin
access, and per-group visibility. Clients should sign every optimized HTTP
read, including public bootstrap reads, so the same flow works on whitelisted
and private relays.

## Bootstrap

Request:

```http
GET /api/obelisk/v1/bootstrap?limit_per_group=50
Authorization: Nostr <auth>
```

Response:

```json
{
  "version": 1,
  "relay": "wss://relay.example.com",
  "generated_at": 1780617600,
  "cursor": {
    "since": 1780617590
  },
  "scopes": [
    {
      "scope": "default",
      "groups": [
        {
          "id": "general",
          "events": [
            {
              "id": "...",
              "pubkey": "...",
              "created_at": 1780617000,
              "kind": 39000,
              "tags": [],
              "content": "",
              "sig": "..."
            }
          ],
          "next_before": 1780617000
        }
      ]
    }
  ]
}
```

`events` are complete raw Nostr events. Clients should ingest them through the
same code path used for events received from normal relay subscriptions.

The bootstrap includes visible group state events and recent visible content:

- Group state and creation: `39000`, `39001`, `39002`, `39003`, `9007`
- Recent messages: `9`, `11`
- Reactions and deletes: `7`, `5`, `9005`
- Invites visible to the authenticated user: `9009`

`limit_per_group` is capped by relay configuration. `next_before` is the cursor
to use when loading older messages for that group.

## Live Catch-Up

After bootstrap ingestion, open one standard WebSocket subscription:

```json
[
  "REQ",
  "obelisk-live",
  {
    "since": 1780617590,
    "kinds": [7, 9, 11, 39000, 39001, 39002, 39003, 9005, 9007, 9009]
  }
]
```

Use the `cursor.since` value from the bootstrap response. Clients must tolerate
duplicate events because Nostr `since` matching may include events already
returned by the bootstrap.

## Message Pagination

Each chat scrolls independently. Loading older messages does not require a new
live subscription.

Request:

```http
GET /api/obelisk/v1/groups/general/messages?before=1780617000&limit=50
Authorization: Nostr <auth>
```

For scoped deployments, pass `scope` when the same group id can exist in more
than one scope:

```http
GET /api/obelisk/v1/groups/general/messages?scope=team-a&before=1780617000&limit=50
```

Response:

```json
{
  "version": 1,
  "scope": "default",
  "group_id": "general",
  "events": [],
  "next_before": 1780616400
}
```

Messages are newest-first. `before` is exclusive. `limit` is capped by relay
configuration. If a group id is ambiguous and no `scope` is provided, the relay
returns HTTP `409`.

## Rate Limits

Optimized HTTP reads do not count against WebSocket connection, subscription,
or normal Nostr `REQ` limits. They have separate HTTP limits:

- Bootstrap default: `30` requests per pubkey per minute.
- Message pages default: `120` requests per pubkey per minute.
- Hard response caps are always enforced.

When limited, the relay returns:

```http
429 Too Many Requests
Retry-After: <seconds>
```

## Fallback

Clients must fall back to normal Nostr behavior when:

- The NIP-11 capability is missing.
- The advertised version is not `1`.
- NIP-98 signing is unavailable.
- The HTTP request fails or returns an invalid response.
- The relay returns `401`, `403`, `404`, `409`, or `429` and the client cannot
  recover by re-authenticating, adding `scope`, or waiting for `Retry-After`.
