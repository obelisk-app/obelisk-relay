import { useState, useEffect } from 'preact/hooks'
import {
  adminApi,
  type PublicRelayInfo,
  type StorageSettings,
  type StorageStats,
} from '../../services/AdminApiClient'
import {
  AccessIcon,
  GroupsIcon,
  OverviewIcon,
  RelayIcon,
  StorageIcon,
} from './icons'

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
  const [relayInfo, setRelayInfo] = useState<PublicRelayInfo | null>(null)
  const [storage, setStorage] = useState<StorageSettings | null>(null)
  const [storageStats, setStorageStats] = useState<StorageStats | null>(null)
  const [iconBroken, setIconBroken] = useState(false)
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
    adminApi.getRelayInfo().then(setRelayInfo).catch(() => undefined)
    adminApi.getStorageSettings().then(setStorage).catch(() => undefined)
    adminApi.getStorageStats().then(r => setStorageStats(r.stats)).catch(() => undefined)
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
  const retention: { tone: string; label: string; detail: string } = (() => {
    if (!storage) return { tone: '', label: 'Retention unknown', detail: 'Could not read storage settings.' }
    if (storage.restart_required) {
      return {
        tone: 'admin-status-badge-warn',
        label: 'Restart to apply',
        detail: 'Retention is configured but not running. Policies are read only at startup.',
      }
    }
    if (!storage.configured_pruning_enabled) {
      return {
        tone: '',
        label: 'Keeping everything',
        detail: 'Nothing is deleted automatically. Storage grows until you act.',
      }
    }
    const windows = Object.entries(storage.policies_secs ?? {})
      .map(([kind, secs]) => `kind ${kind} after ${Math.round(secs / 86400)}d`)
      .join(', ')
    return {
      tone: 'admin-status-badge-ok',
      label: 'Retention on',
      detail: windows ? `Deleting ${windows}.` : 'Retention is enforced.',
    }
  })()

  return (
    <div class="space-y-6">
      {/* Which relay am I looking at? Matters when running several. */}
      <section class="lc-card p-5">
        <div class="flex items-start gap-4">
          <div
            class="flex-shrink-0 flex items-center justify-center overflow-hidden"
            style={{
              width: '48px', height: '48px', borderRadius: '10px',
              border: '1px solid var(--color-border)', background: 'var(--color-bg-primary)',
            }}
          >
            {relayInfo?.icon && !iconBroken
              ? <img src={relayInfo.icon} alt="" class="w-full h-full object-cover" onError={() => setIconBroken(true)} />
              : <RelayIcon class="w-6 h-6" />}
          </div>
          <div class="min-w-0 flex-1">
            <h2 class="text-xl font-bold truncate">{relayInfo?.name || 'Relay'}</h2>
            {relayInfo?.description && (
              <p class="text-sm mt-0.5" style={{ color: 'var(--color-text-secondary)' }}>
                {relayInfo.description}
              </p>
            )}
          </div>
          {/* Was a red "Deleting old events" whenever retention was on. It
              read as an action stuck in progress rather than a steady state,
              and coloured the healthy configuration as a danger -- retention
              being on is what stops the disk filling. */}
          <span class={`admin-status-badge ${retention.tone}`} title={retention.detail}>
            {retention.label}
          </span>
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
          <div class="admin-status-item">
            <div>
              <strong>Retention</strong>
              <p>{retention.detail}</p>
            </div>
            <span class={`admin-status-badge ${retention.tone}`}>{retention.label}</span>
          </div>

          <div class="admin-status-item">
            <div>
              <strong>Database</strong>
              <p>
                {storage ? formatBytes(storage.db_size_bytes) : '—'} on disk
                {storage && storage.runs > 0 && (
                  <> · {formatNumber(storage.total_pruned)} events deleted since start</>
                )}
                {/* LMDB reuses freed pages internally and never returns them,
                    so a file that stays large after a big delete is expected
                    and needs an export/import rebuild, not more pruning. */}
                . Deleting events frees space inside the file, not on the disk.
              </p>
            </div>
            <span class="admin-status-badge">
              {storage && storage.runs > 0 ? `${formatNumber(storage.runs)} prune runs` : 'No prune yet'}
            </span>
          </div>

          <div class="admin-status-item">
            <div>
              <strong>Access</strong>
              <p>
                {stats.whitelisted_count > 0
                  ? `${formatNumber(stats.whitelisted_count)} pubkeys may connect. Everyone else is refused.`
                  : 'No allowlist, so any pubkey may connect and store events here.'}
              </p>
            </div>
            <span class={`admin-status-badge ${stats.whitelisted_count > 0 ? 'admin-status-badge-ok' : ''}`}>
              {stats.whitelisted_count > 0 ? 'Restricted' : 'Open relay'}
            </span>
          </div>
        </div>
      </section>

      {/* Live health first -- the numbers that change minute to minute. */}
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
            <div class="text-3xl font-bold" style={{ color: '#b4f953' }}>
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
