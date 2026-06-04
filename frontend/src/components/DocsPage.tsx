import { useEffect, useState } from 'preact/hooks'

interface RelayInfo {
  name: string
  description: string
  group_count: number
  supported_nips: number[]
}

const sections = [
  { id: 'overview', label: 'Overview' },
  { id: 'architecture', label: 'Architecture' },
  { id: 'events', label: 'Event Flow' },
  { id: 'access', label: 'Access' },
  { id: 'admin', label: 'Admin' },
  { id: 'storage', label: 'Storage' },
  { id: 'deployment', label: 'Deployment' },
  { id: 'operations', label: 'Operations' },
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
        <a href="/" class="docs-brand">Obelisk Relay</a>
        <nav>
          {sections.map(section => (
            <a href={`#${section.id}`} key={section.id}>{section.label}</a>
          ))}
        </nav>
        <div class="docs-sidebar-actions">
          <a href="/admin">Admin</a>
          <a href="/app">App</a>
        </div>
      </aside>

      <main class="docs-main">
        <section id="overview" class="docs-hero">
          <div class="docs-kicker">Relay documentation</div>
          <h1>{info?.name || 'Obelisk Groups Relay'}</h1>
          <p>{info?.description || 'NIP-29 relay with server-side groups, whitelist access, admin tooling, and LMDB storage.'}</p>
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
        </section>

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
          <div class="docs-card-grid">
            <div class="docs-card">
              <h3>Open relay</h3>
              <p>An empty whitelist means any authenticated pubkey can use the relay. Open mode must use per-pubkey, per-connection, and global rate limits.</p>
            </div>
            <div class="docs-card">
              <h3>Whitelist enforced</h3>
              <p>When whitelist entries exist, unauthenticated users receive auth-required errors and non-whitelisted users are denied reads and writes.</p>
            </div>
            <div class="docs-card">
              <h3>Blacklist override</h3>
              <p>Blacklisted pubkeys are denied even if they appear in manual or follow-derived whitelist entries.</p>
            </div>
            <div class="docs-card">
              <h3>Group permissions</h3>
              <p>Admins manage groups and roles, moderators manage content, members can post, and non-members can only read public groups or request access.</p>
            </div>
          </div>
        </section>

        <section id="admin" class="docs-section">
          <h2>Admin Panel</h2>
          <p>The admin panel is Nostr-authenticated. Admin keys are stored as runtime config and can manage access, reference accounts, groups, storage, relay identity, backups, restart, and recovery.</p>
          <div class="docs-card-grid">
            <div class="docs-card">
              <h3>Access</h3>
              <p>Choose open relay or whitelist enforcement, configure rate limits, add allowed pubkeys, and block pubkeys.</p>
            </div>
            <div class="docs-card">
              <h3>References</h3>
              <p>Reference accounts are follow-sync sources. Their follows can populate follow-derived whitelist entries.</p>
            </div>
            <div class="docs-card">
              <h3>Groups</h3>
              <p>Inspect group metadata, members, event streams, delete groups, delete events, and remove problematic user content.</p>
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
      </main>
    </div>
  )
}
