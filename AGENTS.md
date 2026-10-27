# AGENTS.md — Relay Internal Agents & Processors

This document explains the internal "agents" (processors, middleware, and systems) that make up the relay's event processing pipeline. Each agent has a specific responsibility in the chain.

## Agent Pipeline Overview

```
Nostr Client (WebSocket)
    │
    ▼
┌─────────────────────────┐
│  WebSocket Handler      │  Accepts connection, upgrades HTTP → WS
│  (server.rs)            │  Handles NIP-42 auth challenge/response
└─────────┬───────────────┘
          │
          ▼
┌─────────────────────────┐
│  Validation Middleware   │  Structural validation: does the event
│  (validation_middleware) │  have the right tags? Is it well-formed?
└─────────┬───────────────┘
          │
          ▼
┌─────────────────────────┐
│  Groups Event Processor  │  Business logic: whitelist, permissions,
│  (groups_event_processor)│  group operations, content routing
└─────────┬───────────────┘
          │
          ▼
┌─────────────────────────┐
│  Groups State Manager    │  In-memory group state (DashMap),
│  (groups.rs + group.rs)  │  membership, roles, invites
└─────────┬───────────────┘
          │
          ▼
┌─────────────────────────┐
│  Storage (nostr-lmdb)    │  Persistent storage with scoped
│                          │  multi-tenant support
└─────────────────────────┘
```

---

## 1. WebSocket Handler Agent

**File:** `src/server.rs`
**Role:** Connection manager and protocol gateway

### What It Does
- Accepts incoming WebSocket connections
- Handles the Nostr relay protocol (EVENT, REQ, CLOSE, AUTH)
- Initiates NIP-42 authentication challenges
- Routes messages to the processing pipeline
- Manages subscription state per connection
- Serves the Preact frontend on HTTP GET

### Key Behaviors
- Max 300 concurrent connections (configurable)
- 24h max connection duration, 30m idle timeout
- WebSocket upgrade with Connection/Upgrade header forwarding
- Health endpoint at `/health` for Docker healthchecks

---

## 2. Validation Middleware Agent

**File:** `src/validation_middleware.rs` (131 lines)
**Role:** Structural gatekeeper — rejects malformed events before they reach business logic

### What It Does
- Validates that group events have the required `h` tag (group identifier)
- Allows relay-pubkey events with `d` tag (managed group metadata like 39000-39003)
- Passes through `NON_GROUP_ALLOWED_KINDS` without group tag requirement

### Rules
```
IF event.pubkey == relay_pubkey AND has d-tag → ALLOW (relay metadata)
IF event has h-tag → ALLOW (group event, will be validated by processor)
IF event.kind in NON_GROUP_ALLOWED_KINDS → ALLOW (e.g. NIP-42 auth, wallet events)
ELSE → REJECT ("missing h tag")
```

### Why It Exists
Separates structural validation from business logic. The processor doesn't need to worry about malformed events — they're already filtered out.

---

## 3. Groups Event Processor Agent (The Brain)

**File:** `src/groups_event_processor.rs` (422 lines)
**Role:** Core business logic — the decision maker for all relay operations

### What It Does
This is the most important agent. It implements the `EventProcessor` trait with three methods:

#### `verify_filters()` — Read Access Control
Called when a client sends a REQ (subscription request).
1. **Whitelist check** — is this pubkey allowed on the relay at all?
2. **Group access check** — for private groups, is the user a member?
3. Not admitted: returns `auth-required` if the socket has not authenticated yet
   (clients AUTH and retry on it), `restricted` if it has (AUTH won't help)

#### `can_see_event()` — Event Visibility
Called before delivering each event to a subscriber.
1. Finds the group the event belongs to (via h-tag)
2. Delegates to `Group.can_see_event()` for per-group visibility rules
3. Non-group events are always visible

#### `handle_event()` — Write Processing
Called when a client sends an EVENT.
1. **Blacklist** — the event's author, whatever the session
2. **Exemptions** — NIP-46 signer traffic (24133), and relay invite claims
   (28934, `handle_relay_claim`; see `docs/relay-invites.md`), which by design
   come from keys that are not admitted yet
3. **Admission** — `is_allowed()`: `auth-required:` if not authenticated,
   `restricted:` if authenticated but not admitted
4. **Unmanaged groups** — allows events for groups not yet created (NIP-29 spec)
5. **Kind routing** — routes to the appropriate handler:

| Kind | Handler | What Happens |
|------|---------|-------------|
| 9007 | `handle_group_create` | Creates group, sets creator as admin |
| 9002 | `handle_edit_metadata` | Updates group name/description/settings |
| 9021 | `handle_join_request` | Auto-accepts (open) or queues (closed) |
| 9022 | `handle_leave_request` | Removes member from group |
| 9006 | `handle_set_roles` | Assigns roles to members |
| 9000 | `handle_put_user` | Admin adds user to group |
| 9001 | `handle_remove_user` | Admin removes user from group |
| 9008 | `handle_delete_group` | Deletes group and all its data |
| 9005 | `handle_delete_event` | Moderator deletes specific content |
| 9009 | `handle_create_invite` | Creates a group invite code |
| 28934 | `handle_relay_claim` | Redeems a *relay* invite code (before admission) |
| other+h-tag | `handle_group_content` | Stores chat messages, etc. |
| other | pass-through | Non-group events stored directly |

### Whitelist Logic
Admission is one rule, `Whitelist::admits` (`src/whitelist.rs`), which
`is_allowed()` and the unindexed-query budget both call:

```rust
fn is_allowed(&self, pubkey: &Option<PublicKey>) -> bool {
    self.whitelist.admits(pubkey.as_ref(), |pk| self.is_relay_admin(pk))
}
// admits: blacklist → open relay (nothing restricts) → contains(pk) || admin
// contains: manual → follow-derived → invite → web-of-trust
```

The invite tier (`AccessTier::Invite`) never makes an open relay closed:
`is_empty()` ignores it. Its contents are owned and persisted by
`RelayInviteStore` (`config/relay_invites.json`).

---

## 4. Groups State Manager Agent

**File:** `src/groups.rs` (2,078 lines)
**Role:** In-memory group state with database persistence

### What It Does
- Maintains a `DashMap<(Scope, String), Group>` of all groups
- Loads all groups from database on startup (`load_groups()`)
- Provides thread-safe concurrent access to group state
- Each group operation validates permissions, mutates state, and returns `StoreCommand`s

### Scoped Storage
Groups are keyed by `(Scope, group_id)` for multi-tenant subdomain support:
```rust
// Different subdomains can have groups with the same ID
groups.get(&(Scope::Named("sub1".into()), "general".into()))
groups.get(&(Scope::Default, "general".into()))
```

### Key Methods
| Method | Called By | Purpose |
|--------|----------|---------|
| `load_groups()` | Startup | Rebuilds state from database |
| `get_group()` | Processor | Lookup group by scope + ID |
| `find_group_from_event()` | Processor | Find group from event's h-tag |
| `handle_group_create()` | Kind 9007 | Create new group |
| `handle_edit_metadata()` | Kind 9002 | Update group settings |
| `handle_join_request()` | Kind 9021 | Process join request |
| `handle_leave_request()` | Kind 9022 | Process leave |
| `handle_put_user()` | Kind 9000 | Add member |
| `handle_remove_user()` | Kind 9001 | Remove member |
| `handle_set_roles()` | Kind 9006 | Assign roles |
| `handle_delete_group()` | Kind 9008 | Delete group |
| `handle_delete_event()` | Kind 9005 | Delete content |
| `handle_create_invite()` | Kind 9009 | Create invite code |
| `handle_group_content()` | Content events | Store group messages |

---

## 5. Individual Group Agent

**File:** `src/group.rs` (3,189 lines)
**Role:** Per-group state and permission logic

### What It Does
Each `Group` struct manages:
- **Metadata** — name, description, picture, public/private, open/closed, broadcast
- **Members** — pubkey → roles mapping
- **Roles** — admin, moderator, member, custom roles with permissions
- **Invites** — code → invite details (expiration, max uses)
- **Join requests** — pending requests for closed groups

### Permission Model
```
Admin       → Full control (create/delete groups, manage members, set roles, moderate)
Moderator   → Delete events, manage content
Member      → Post content, leave group
Non-member  → Read public groups, submit join requests
```

### Key Methods
| Method | Purpose |
|--------|---------|
| `is_member(pubkey)` | Check membership |
| `has_role(pubkey, role)` | Check specific role |
| `can_see_event(pubkey, event)` | Visibility check for private groups |
| `apply_tags(tags)` | Update metadata from event tags |
| `is_group_management_kind(kind)` | Identify management events (9000-9009) |

---

## 6. Storage Agent (nostr-lmdb)

**Role:** Persistent event storage

### What It Does
- Stores Nostr events in LMDB (Lightning Memory-Mapped Database)
- Supports scoped storage for multi-tenant subdomains
- Handles `StoreCommand`s from the processor:
  - `SaveSignedEvent` — store an event as-is
  - `SaveUnsignedEvent` — relay signs and stores (for metadata events 39000-39003)
  - `DeleteEvents` — remove events by filter

### Utilities
- `nostr-lmdb-dump` — export all events
- `nostr-lmdb-integrity` — verify database consistency
- `export_import` — backup/restore
- `negentropy_sync` — sync with another relay

---

## 7. Metrics Agent

**File:** `src/metrics.rs` (239 lines)
**Role:** Observability

### What It Does
- Collects Prometheus metrics on relay operations
- Tracks connection counts, event processing times, subscription counts
- Exposed at `/metrics` endpoint
- Sampled metrics handler reduces overhead

---

## Agent Interaction Example: User Sends a Chat Message

```
1. User's Nostr client sends EVENT (kind 11, h-tag: "general")
2. WebSocket Handler receives the raw message
3. Validation Middleware checks: has h-tag? ✓
4. GroupsRelayProcessor.handle_event():
   a. is_allowed(user_pubkey)? → checks whitelist → ✓
   b. find_group_from_event() → finds "general" group
   c. It's not a management kind, has h-tag → handle_group_content()
5. Groups State Manager.handle_group_content():
   a. Is user a member of "general"? ✓
   b. Is group in broadcast mode? No → members can post
   c. Returns StoreCommand::SaveSignedEvent
6. Storage Agent persists the event
7. WebSocket Handler broadcasts to all subscribers of "general"
   (filtered by can_see_event for each subscriber)
```

## Agent Interaction Example: Whitelist Rejection

```
1. Unknown user connects and sends AUTH
2. WebSocket Handler initiates NIP-42 challenge
3. User responds with signed challenge
4. User sends REQ for group events
5. GroupsRelayProcessor.verify_filters():
   a. is_allowed(unknown_pubkey)? → NOT in whitelist → ✗
   b. Authenticated but not admitted → Error::restricted
      (before AUTH it would be Error::auth_required, prompting the client to AUTH)
6. User receives: ["CLOSED", sub_id, "restricted: Access denied: your pubkey is not whitelisted on this relay"]
```
