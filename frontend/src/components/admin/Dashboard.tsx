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

  const pruning = storage?.pruning_enabled ?? false

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
          <span
            class={`admin-status-badge ${pruning ? 'admin-status-badge-danger' : 'admin-status-badge-ok'}`}
            title={pruning
              ? 'Automatic retention pruning is running on this relay'
              : 'No automatic deletion is configured'}
          >
            {pruning ? 'Deleting old events' : 'Nothing is deleted'}
          </span>
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
