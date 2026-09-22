import { useState, useEffect } from 'preact/hooks'
import {
  adminApi,
  type AccessSources,
  type StorageSettings,
  type StorageSample,
  type StorageStats,
} from '../../services/AdminApiClient'
import {
  AccessIcon,
  GroupsIcon,
  OverviewIcon,
  StorageIcon,
} from './icons'
import { TimeSeriesChart } from './TimeSeriesChart'

interface Stats {
  active_connections: number
  total_groups: number
  total_members: number
  whitelisted_count: number
  uptime_seconds: number
}

const formatUptime = (seconds: number): string => {
  const days = Math.floor(seconds / 86400)
  const hours = Math.floor((seconds % 86400) / 3600)
  const mins = Math.floor((seconds % 3600) / 60)
  if (days > 0) return `${days}d ${hours}h ${mins}m`
  if (hours > 0) return `${hours}h ${mins}m`
  return `${mins}m`
}

const formatNumber = (n: number) => n.toLocaleString()

/**
 * Who can actually connect, in one sentence.
 *
 * Every tier that admits has to appear, or the sentence is false. The web of
 * trust admits by reachability rather than by listing keys, so it contributes a
 * number that lives nowhere in the allowlist — the reason the old copy claimed
 * 234 when the real answer was closer to 140,000.
 *
 * `wot_admitted` is 0 until the follow graph has been built, which is a
 * genuinely different state from "trust admits nobody": the tier is armed but
 * cannot answer yet, so it is called out rather than silently counted as zero.
 */
const accessSummary = (access: AccessSources | null, whitelistedCount: number): string => {
  // Falls back to the old, narrower sentence only until the tiers have loaded.
  if (!access) {
    return whitelistedCount > 0
      ? `${formatNumber(whitelistedCount)} pubkeys on the allowlist.`
      : 'No allowlist, so any pubkey may connect and store events here.'
  }

  if (access.open_relay) {
    return 'Open relay: any pubkey may connect and store events here.'
  }

  const listed = access.manual + access.follow_derived
  const parts: string[] = []

  if (listed > 0) {
    const how = access.follow_derived > 0
      ? `${formatNumber(access.manual)} added by hand, ${formatNumber(access.follow_derived)} from follow sync`
      : 'added by hand'
    parts.push(`${formatNumber(listed)} on the allowlist (${how})`)
  }

  if (access.wot_enabled) {
    parts.push(
      access.wot_admitted > 0
        ? `about ${formatNumber(access.wot_admitted)} more within ${access.wot_max_hops} hops of the web of trust`
        : `the web of trust is on, but its follow graph is not built yet, so it is admitting nobody right now`
    )
  }

  const who = parts.length > 0 ? parts.join(', plus ') : 'nobody'
  const blocked = access.blacklisted > 0
    ? ` ${formatNumber(access.blacklisted)} blocked outright, which overrides every tier.`
    : ''

  return `${who[0].toUpperCase()}${who.slice(1)} may connect. Everyone else is refused.${blocked}`
}

const formatBytes = (bytes: number) => {
  if (!bytes) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const i = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1)
  return `${(bytes / Math.pow(1024, i)).toFixed(i === 0 ? 0 : 1)} ${units[i]}`
}

const relativeAge = (unix: number) => {
  if (!unix) return 'never'
  const secs = Math.max(0, Math.floor(Date.now() / 1000) - unix)
  if (secs < 60) return 'just now'
  if (secs < 3600) return `${Math.floor(secs / 60)}m ago`
  if (secs < 86400) return `${Math.floor(secs / 3600)}h ago`
  return `${Math.floor(secs / 86400)}d ago`
}

/** Secondary metric: same weight as its siblings, smaller than the health row. */
const StatCard = ({
  label,
  value,
  sub,
  icon: Icon,
}: {
  label: string
  value: string | number
  sub?: string
  icon: (p: { class?: string }) => preact.JSX.Element
}) => (
  <div class="lc-card p-4">
    <div class="flex items-center gap-2 mb-1.5" style={{ color: 'var(--color-text-secondary)' }}>
      <Icon class="w-4 h-4" />
      <span class="text-xs">{label}</span>
    </div>
    <div class="text-xl font-bold">{value}</div>
    {sub && <div class="text-xs mt-0.5" style={{ color: 'var(--color-text-secondary)' }}>{sub}</div>}
  </div>
)

export const Dashboard = () => {
  const [stats, setStats] = useState<Stats | null>(null)
  const [storage, setStorage] = useState<StorageSettings | null>(null)
  const [storageStats, setStorageStats] = useState<StorageStats | null>(null)
  // Admission is several tiers, not one list. Without this the Access card can
  // only see the explicit allowlist and reports it as the whole rule.
  const [access, setAccess] = useState<AccessSources | null>(null)
  // The hourly self-samples behind both charts. The Overview had no trend at
  // all -- every number on it was an instant, so "is this growing, and how
  // fast" could only be answered by opening the Storage screen.
  const [history, setHistory] = useState<StorageSample[]>([])
  const [error, setError] = useState<string | null>(null)

  const fetchStats = () => {
    adminApi.getStats()
      .then(setStats)
      .catch(e => setError(e.message))
  }

  useEffect(() => {
    fetchStats()
    const interval = setInterval(fetchStats, 30000)
    // Identity and storage change rarely; fetch once. The storage call is the
    // cached snapshot, never a fresh scan -- the Storage screen owns recounting.
    adminApi.getStorageSettings().then(setStorage).catch(() => undefined)
    adminApi.getStorageStats().then(r => setStorageStats(r.stats)).catch(() => undefined)
    adminApi.getAccessSources().then(setAccess).catch(() => undefined)
    // Advisory: a relay with no history file yet still has a usable Overview.
    adminApi.getStorageHistory().then(r => setHistory(r.samples)).catch(() => undefined)
    return () => clearInterval(interval)
  }, [])

  if (error) {
    return <div class="p-4 rounded-lg bg-red-500/10 text-red-400 border border-red-500/20">{error}</div>
  }

  if (!stats) {
    return (
      <div class="flex items-center gap-3" style={{ color: 'var(--color-text-secondary)' }}>
        <span class="lc-spinner" />
        Loading stats...
      </div>
    )
  }

  // Retention state is three states, not two, and the third is the one that
  // matters: armed in the config but not yet live, because policies are only
  // read at startup. That is the state this relay sat in while its database
  // grew to 5.2GB -- the config said pruning was on and nothing was running.
  const retention: { tone: string; label: string; detail: string; summary: string } = (() => {
    if (!storage)
      return {
        tone: '',
        label: 'Retention unknown',
        detail: 'Could not read storage settings.',
        summary: 'Unavailable',
      }
    if (storage.restart_required) {
      return {
        tone: 'admin-status-badge-warn',
        label: 'Restart to apply',
        detail: 'Retention is configured but not running. Policies are read only at startup.',
        summary: 'Configured, not yet running',
      }
    }
    if (!storage.configured_pruning_enabled) {
      return {
        tone: '',
        label: 'Keeping everything',
        detail: 'Nothing is deleted automatically. Storage grows until you act.',
        summary: 'Nothing deleted automatically',
      }
    }
    const windows = Object.entries(storage.policies_secs ?? {})
      .map(([kind, secs]) => `kind ${kind} after ${Math.round(secs / 86400)}d`)
      .join(', ')
    return {
      tone: 'admin-status-badge-ok',
      label: 'Retention on',
      detail: windows ? `Deleting ${windows}.` : 'Retention is enforced.',
      summary: `${Object.keys(storage.policies_secs ?? {}).length} kind(s) on a timer`,
    }
  })()

  return (
    <div class="space-y-6">
      {/* Trends lead: the shape of the last day answers "is anything wrong"
          faster than any single number can, and the numbers below it are then
          read as a point on a line rather than on their own. The identity card
          that used to sit here -- relay name, description, and a retention
          badge -- is gone: you already know which relay you opened, the name is
          in the sidebar, and its badge said the same thing as the Retention row
          a few inches below it. */}
      <section>
        <h3 class="text-sm font-semibold mb-3" style={{ color: 'var(--color-text-secondary)' }}>
          Trends
        </h3>
        <div class="grid grid-cols-1 lg:grid-cols-2 gap-4">
          <div class="lc-card p-5">
            <div class="admin-storage-chart-head">
              <h4>Active connections</h4>
              <p>
                Sampled hourly, so this is the shape of the day rather than a live
                gauge — the figure above is live. Hover for a value and a time.
              </p>
            </div>
            <TimeSeriesChart
              points={history
                .filter(h => h.connections != null)
                .map(h => ({ at: h.at, value: h.connections as number }))}
              format={n => `${formatNumber(n)} ${n === 1 ? 'connection' : 'connections'}`}
              label="Connections"
              zeroBased={false}
              emptyHint="Collecting — connection counts appear over the next few hours."
            />
          </div>
          <div class="lc-card p-5">
            <div class="admin-storage-chart-head">
              <h4>Disk used</h4>
              <p>
                The database file, sampled hourly. It never shrinks on its own:
                LMDB reuses freed pages internally, so deleting events flattens
                this line rather than lowering it.
              </p>
            </div>
            <TimeSeriesChart
              points={history.map(h => ({ at: h.at, value: h.db_bytes }))}
              format={formatBytes}
              label="Database size"
            />
          </div>
        </div>
      </section>

      {/* The numbers that change minute to minute, under the curves they are
          the latest point of. */}
      <section>
        <h3 class="text-sm font-semibold mb-3" style={{ color: 'var(--color-text-secondary)' }}>
          Right now
        </h3>
        <div class="grid grid-cols-1 sm:grid-cols-3 gap-4">
          <div class="lc-card p-5">
            <div class="flex items-center gap-2 mb-1" style={{ color: 'var(--color-text-secondary)' }}>
              <OverviewIcon class="w-4 h-4" />
              <span class="text-xs">Active connections</span>
            </div>
            <div class="text-3xl font-bold" style={{ color: 'var(--color-accent)' }}>
              {formatNumber(stats.active_connections)}
            </div>
          </div>
          <div class="lc-card p-5">
            <div class="flex items-center gap-2 mb-1" style={{ color: 'var(--color-text-secondary)' }}>
              <OverviewIcon class="w-4 h-4" />
              <span class="text-xs">Uptime</span>
            </div>
            <div class="text-3xl font-bold">{formatUptime(stats.uptime_seconds)}</div>
          </div>
          <div class="lc-card p-5">
            <div class="flex items-center gap-2 mb-1" style={{ color: 'var(--color-text-secondary)' }}>
              <StorageIcon class="w-4 h-4" />
              <span class="text-xs">Last event received</span>
            </div>
            <div class="text-3xl font-bold">
              {storageStats?.newest_event_unix ? relativeAge(storageStats.newest_event_unix) : '—'}
            </div>
          </div>
        </div>
      </section>

      {/*
        * What is configured, and whether it is actually in force. The relay ran
        * for weeks with a config that said pruning was on while nothing was
        * deleting, and with a channel-list query that took 29 seconds -- none
        * of which was visible anywhere. A status is only worth showing if it
        * says whether the thing is working, so each row carries its own verdict
        * rather than a raw value the operator has to interpret.
        */}
      <section>
        <h3 class="text-sm font-semibold mb-3" style={{ color: 'var(--color-text-secondary)' }}>
          Configured
        </h3>
        <div class="admin-status-list">
          {/* The explanation moved into `title`. These rows carried a
              paragraph each, so the four verdicts an operator scans for were
              separated by prose they had already read. Summary on the row,
              detail on hover. */}
          <div class="admin-status-item" title={retention.detail}>
            <div>
              <strong>Retention</strong>
              <p class="admin-status-summary">{retention.summary}</p>
            </div>
            <span class={`admin-status-badge ${retention.tone}`}>{retention.label}</span>
          </div>

          <div
            class="admin-status-item"
            title={
              'Deleting events frees space inside the LMDB file but never returns it to the '
              + 'filesystem, so a file that stays large after a big delete is expected. '
              + 'Storage → Reclaim disk space hands it back.'
            }
          >
            <div>
              <strong>Database</strong>
              <p class="admin-status-summary">
                {storage ? formatBytes(storage.db_size_bytes) : '—'} on disk
                {storage && storage.runs > 0 && (
                  <> · {formatNumber(storage.total_pruned)} deleted</>
                )}
              </p>
            </div>
            <span class="admin-status-badge">
              {storage && storage.runs > 0 ? `${formatNumber(storage.runs)} prune runs` : 'No prune yet'}
            </span>
          </div>

          {/* Admission is a ladder: an explicit allowlist, keys from follow
              sync, and the web of trust. Reporting only the first understated
              this relay by two orders of magnitude -- 234 listed keys next to
              ~140,000 admitted by trust -- and read as "everyone else is
              refused", which was untrue. The full sentence is the hover; the
              row shows the tier counts. */}
          <div class="admin-status-item" title={accessSummary(access, stats.whitelisted_count)}>
            <div>
              <strong>Access</strong>
              <p class="admin-status-summary">
                {access
                  ? [
                      `Tier 1 · ${formatNumber(access.manual + access.follow_derived)}`,
                      access.wot_enabled ? `trust · ${formatNumber(access.wot_admitted)}` : null,
                      access.blacklisted > 0 ? `blocked · ${formatNumber(access.blacklisted)}` : null,
                    ]
                      .filter(Boolean)
                      .join('   ')
                  : `${formatNumber(stats.whitelisted_count)} allowed`}
              </p>
            </div>
            <span class={`admin-status-badge ${access?.open_relay === false || stats.whitelisted_count > 0 ? 'admin-status-badge-ok' : ''}`}>
              {access
                ? access.open_relay
                  ? 'Open relay'
                  : access.wot_enabled ? 'Allowlist + trust' : 'Allowlist'
                : stats.whitelisted_count > 0 ? 'Restricted' : 'Open relay'}
            </span>
          </div>
        </div>
      </section>

      {/* Standing totals -- demoted, compact. */}
      <section>
        <h3 class="text-sm font-semibold mb-3" style={{ color: 'var(--color-text-secondary)' }}>
          This relay holds
        </h3>
        <div class="grid grid-cols-2 lg:grid-cols-4 gap-4">
          <StatCard label="Groups" value={formatNumber(stats.total_groups)} icon={GroupsIcon} />
          <StatCard label="Members" value={formatNumber(stats.total_members)} icon={GroupsIcon} />
          <StatCard
            label="Allowed pubkeys"
            value={stats.whitelisted_count === 0 ? 'Open' : formatNumber(stats.whitelisted_count)}
            sub={stats.whitelisted_count === 0 ? 'no whitelist' : undefined}
            icon={AccessIcon}
          />
          <StatCard
            label="Database"
            value={storage ? formatBytes(storage.db_size_bytes) : '—'}
            sub={storageStats
              ? `${formatNumber(storageStats.sampled_events)} events ${storageStats.sample_is_complete ? 'stored' : 'sampled'}`
              : undefined}
            icon={StorageIcon}
          />
        </div>
      </section>
    </div>
  )
}
