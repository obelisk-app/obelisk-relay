import { useEffect, useState } from 'preact/hooks'
import {
  AccessIcon,
  DeployIcon,
  FlowIcon,
  OverviewIcon,
  RelayIcon,
  ReportsIcon,
  SettingsIcon,
  StorageIcon,
} from './admin/icons'

interface RelayInfo {
  name: string
  description: string
  group_count: number
  supported_nips: number[]
  /** NIP-11 icon, when the operator has set one. */
  icon?: string
}

/* The console's own glyphs, so a section means the same thing in both places:
   whatever Access looks like in the sidebar of the admin panel is what Access
   looks like here. */
const sections = [
  { id: 'overview', label: 'Overview', icon: OverviewIcon },
  { id: 'architecture', label: 'Architecture', icon: RelayIcon },
  { id: 'events', label: 'Event Flow', icon: FlowIcon },
  { id: 'access', label: 'Access', icon: AccessIcon },
  { id: 'admin', label: 'Admin', icon: SettingsIcon },
  { id: 'storage', label: 'Storage', icon: StorageIcon },
  { id: 'deployment', label: 'Deployment', icon: DeployIcon },
  { id: 'operations', label: 'Operations', icon: ReportsIcon },
]

const kindRows = [
  ['9007', 'Create group', 'Creates group state and assigns creator as admin.'],
  ['9002', 'Edit metadata', 'Updates name, description, privacy, broadcast, and channel settings.'],
  ['9021', 'Join request', 'Auto-accepts open groups or queues closed-group requests.'],
  ['9022', 'Leave request', 'Removes the requester from membership state.'],
  ['9000', 'Put user', 'Admin adds a user to a group.'],
  ['9001', 'Remove user', 'Admin removes a user from a group.'],
  ['9006', 'Set roles', 'Admin assigns roles and permissions.'],
  ['9005', 'Delete event', 'Moderator/admin deletes group content.'],
  ['9008', 'Delete group', 'Deletes group data and related events.'],
  ['9009', 'Create invite', 'Creates invite codes with expiration and use limits.'],
  ['other + h tag', 'Group content', 'Stores chat messages and other group-scoped content.'],
]

export const DocsPage = (_props: { path?: string }) => {
  const [info, setInfo] = useState<RelayInfo | null>(null)
  const [iconBroken, setIconBroken] = useState(false)
  const wsUrl = `${window.location.protocol === 'https:' ? 'wss:' : 'ws:'}//${window.location.host}`

  useEffect(() => {
    fetch('/api/relay-info')
      .then(res => res.json())
      .then(setInfo)
      .catch(() => undefined)
  }, [])

  return (
    <div class="docs-page">
      <aside class="docs-sidebar">
        {/* The same banner as the console's sidebar: the relay's own icon and
            name, then what this page is. It read "Obelisk Relay" on every
            deployment regardless of which relay was serving it. */}
        <a href="/" class="docs-brand">
          <span class="docs-brand-icon">
            {info?.icon && !iconBroken ? (
              <img src={info.icon} alt="" onError={() => setIconBroken(true)} />
            ) : (
              <RelayIcon class="w-7 h-7" />
            )}
          </span>
          <span class="docs-brand-text">
            <strong>{info?.name || 'Obelisk Relay'}</strong>
            <small>Documentation</small>
          </span>
        </a>
        <nav>
          {sections.map(section => (
            <a href={`#${section.id}`} key={section.id}>
              <section.icon class="docs-nav-icon" />
              <span>{section.label}</span>
            </a>
          ))}
        </nav>
        <div class="docs-sidebar-actions">
          <a href="/admin">Admin</a>
          {/* The client is obelisk.ar's, with this relay as a parameter --
              window.location.host so each deployment points at itself. */}
          <a
            href={`https://obelisk.ar/app?relay=${encodeURIComponent(window.location.host)}`}
            target="_blank"
            rel="noopener noreferrer"
          >
            App
          </a>
        </div>
      </aside>

      <main class="docs-main">
        {/* Title and blurb only, so this is the same height as the console's
            page header and the two rules line up when you cross from /admin.
            It sits outside the scroll region below, which is what stops it
            riding up the page -- the same arrangement the console uses, and for
            the same reason: an element can only stick against a scrollport
            that actually scrolls, and the window was the scrollport here. */}
        <div class="docs-header">
          <div class="docs-kicker">Relay documentation</div>
          <h1>{info?.name || 'Obelisk Groups Relay'}</h1>
          <p>{info?.description || 'NIP-29 relay with server-side groups, whitelist access, admin tooling, and LMDB storage.'}</p>
        </div>

        <div class="docs-scroll">
        <div id="overview" class="docs-facts">
          <div class="docs-hero-grid">
            <div>
              <span>WebSocket URL</span>
              <strong>{wsUrl}</strong>
            </div>
            <div>
              <span>Groups</span>
              <strong>{info?.group_count ?? '-'}</strong>
            </div>
            <div>
              <span>Supported NIPs</span>
              <strong>{info?.supported_nips?.join(', ') || '1, 9, 11, 29, 40, 42, 70'}</strong>
            </div>
          </div>
        </div>

        <section id="architecture" class="docs-section">
          <h2>Architecture</h2>
          <p>The relay is a Rust Axum server with a Preact admin/client frontend. WebSocket traffic enters the Nostr protocol handler, passes validation middleware, then hits the NIP-29 group processor before events are persisted to LMDB.</p>
          <div class="docs-flow">
            <span>Nostr client</span>
            <span>WebSocket handler</span>
            <span>Validation middleware</span>
            <span>Groups processor</span>
            <span>Groups state</span>
            <span>nostr-lmdb</span>
          </div>
          <div class="docs-card-grid">
            <div class="docs-card">
              <h3>WebSocket handler</h3>
              <p>Accepts Nostr EVENT, REQ, CLOSE, and AUTH messages, serves NIP-42 challenges, tracks subscriptions, and serves the frontend for HTTP clients.</p>
            </div>
            <div class="docs-card">
              <h3>Validation middleware</h3>
              <p>Rejects malformed group events before business logic. Group events need an h tag, relay metadata needs d tags, and allowed non-group kinds pass through.</p>
            </div>
            <div class="docs-card">
              <h3>Groups processor</h3>
              <p>Central write/read policy layer. It checks whitelist access, private-group visibility, management permissions, and routes events by kind.</p>
            </div>
            <div class="docs-card">
              <h3>State and storage</h3>
              <p>In-memory group state is keyed by scope and group id. Persistent events live in LMDB and are restored into group state on startup.</p>
            </div>
          </div>
        </section>

        <section id="events" class="docs-section">
          <h2>Event Flow</h2>
          <p>A typical group message is validated structurally, checked against relay access policy, matched to a group, checked for membership/post permission, stored, and then delivered only to subscribers that can see it.</p>
          <div class="docs-table">
            <table>
              <thead>
                <tr>
                  <th>Kind</th>
                  <th>Operation</th>
                  <th>Behavior</th>
                </tr>
              </thead>
              <tbody>
                {kindRows.map(row => (
                  <tr key={row[0]}>
                    <td>{row[0]}</td>
                    <td>{row[1]}</td>
                    <td>{row[2]}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </section>

        <section id="access" class="docs-section">
          <h2>Access and Permissions</h2>
          <p>Admission is three tiers and a block list, decided in that order. Each tier publishes on a share of the rate budget, because distance from your reference accounts is how much evidence you have that someone should be trusted with your disk.</p>
          <div class="docs-card-grid">
            <div class="docs-card">
              <h3>Tier 1 — full budget</h3>
              <p>Pubkeys added by hand, the reference accounts themselves, and everyone those accounts follow. Follow-derived entries are refreshed by follow sync, so removing one without blocking it brings it back on the next run.</p>
            </div>
            <div class="docs-card">
              <h3>Tier 2 and Tier 3 — half and a quarter</h3>
              <p>Reachable in two or three hops of the follow graph, which is built from the reference accounts outward. Membership is the graph's answer rather than a list, so nothing is added or removed here directly. The outermost hop is the one the graph's fetch budget truncates first, so its count can be a floor rather than a total.</p>
            </div>
            <div class="docs-card">
              <h3>Blocked overrides everything</h3>
              <p>A blocked pubkey is refused however it would otherwise qualify, including Tier 1. It is checked against the event's signature rather than the authenticated session, so declining to authenticate is not a way around it.</p>
            </div>
            <div class="docs-card">
              <h3>Enforcement and group roles</h3>
              <p>With enforcement off, any pubkey can publish and the rate limits are the only brake. With it on, unauthenticated clients get auth-required and unadmitted ones are denied reads and writes. Inside a group: admins manage roles, moderators manage content, members post, and non-members read public groups or request access.</p>
            </div>
          </div>
        </section>

        <section id="admin" class="docs-section">
          <h2>Admin Panel</h2>
          <p>The admin panel is Nostr-authenticated. Admin keys are stored as runtime config and can manage access, groups, moderation reports, storage, relay identity, backups, restart, and recovery.</p>
          <div class="docs-card-grid">
            <div class="docs-card">
              <h3>Access</h3>
              <p>One screen: search any npub, hex key or NIP-05 address to see which tier admits it and why, browse Tier 1, 2 and 3 and the blocked list, and select rows to add or block in bulk. Reference accounts are configured at the top of Tier 1, since seeding it is what they do.</p>
            </div>
            <div class="docs-card">
              <h3>Policies</h3>
              <p>Inside Access: whether admission is enforced at all, the rate budget each tier gets, and how the follow graph is built — locally from contact lists the relay holds, or from an external oracle.</p>
            </div>
            <div class="docs-card">
              <h3>Groups</h3>
              <p>Inspect group metadata, members, event streams, delete groups, delete events, and remove problematic user content.</p>
            </div>
            <div class="docs-card">
              <h3>Reports</h3>
              <p>NIP-56 kind-1984 reports, grouped by what was reported rather than one row per reporter. Readable by relay admins only — a public moderation queue is a retaliation channel. The relay snapshots reported content when the report arrives, so evidence survives the message being deleted.</p>
            </div>
            <div class="docs-card">
              <h3>Settings</h3>
              <p>Manage relay identity, admin pubkeys, backups, key rotation, restart, and recovery reset.</p>
            </div>
          </div>
        </section>

        <section id="storage" class="docs-section">
          <h2>Storage and Pruning</h2>
          <p>Events are stored in LMDB. The Storage panel shows database size and pruner status. Pruning is disabled by default and only starts after config is saved and the relay restarts.</p>
          <div class="docs-callout">
            Protected NIP-29 management and state kinds are never pruned. This prevents accidental deletion of group identity, membership, roles, and metadata.
          </div>
        </section>

        <section id="deployment" class="docs-section">
          <h2>Deployment</h2>
          <p>The standard deployment is Docker Compose. The relay container exposes port 8080 and can be published through Cloudflare Tunnel or another reverse proxy.</p>
          <div class="docs-code">
            <code>docker compose build groups_relay</code>
            <code>docker compose up -d --no-deps groups_relay</code>
            <code>curl -fsS http://127.0.0.1:8080/health</code>
          </div>
          <p>Release builds use the Dockerfile to compile Rust binaries, build the Preact frontend, copy LMDB utilities, and publish an image to GHCR.</p>
        </section>

        <section id="operations" class="docs-section">
          <h2>Operations Checklist</h2>
          <div class="docs-card-grid">
            <div class="docs-card">
              <h3>Health</h3>
              <p>Use /health for uptime checks and /metrics for Prometheus metrics.</p>
            </div>
            <div class="docs-card">
              <h3>Backups</h3>
              <p>Config reset and restore create timestamped backups under config/backups. Download and restore them from Settings.</p>
            </div>
            <div class="docs-card">
              <h3>Restart</h3>
              <p>Some changes are startup-only: pruning activation, relay URL/name changes, and relay key rotation. Use Settings to restart.</p>
            </div>
            <div class="docs-card">
              <h3>Recovery reset</h3>
              <p>Reset clears access runtime state and reopens setup while keeping event history, group data, database contents, and relay secret key.</p>
            </div>
          </div>
        </section>
        </div>
      </main>
    </div>
  )
}
