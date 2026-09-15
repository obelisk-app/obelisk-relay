import { useEffect, useState } from 'preact/hooks'
import {
  adminApi,
  type StorageSettings,
  type StorageStats,
  type StorageSample,
} from '../../services/AdminApiClient'
import { confirmMatches } from './confirmPhrase'

/**
 * Kinds the relay refuses to prune under any configuration — mirrors
 * NEVER_PRUNE_KINDS in src/pruner.rs. Group identity, membership, roles and
 * metadata are reconstructed from these, so pruning them makes groups vanish
 * or lose their admins. Shown read-only rather than hidden, so the guarantee
 * is visible instead of implicit.
 */
const NEVER_PRUNE_KINDS = [
  9000, 9001, 9002, 9003, 9004, 9005, 9006, 9007, 9008, 9009, 9010, 9011,
  39000, 39001, 39002, 39003,
]

/** Kinds an operator can reasonably choose to prune, with plain-language names. */
const PRUNABLE_KINDS: { kind: number; label: string; hint?: string }[] = [
  { kind: 9, label: 'Group chat message', hint: 'NIP-29 chat' },
  { kind: 11, label: 'Group thread', hint: 'NIP-29 thread root' },
  { kind: 12, label: 'Group thread reply' },
  { kind: 1, label: 'Note', hint: 'Public short note' },
  { kind: 7, label: 'Reaction' },
  { kind: 5, label: 'Deletion request' },
  { kind: 1984, label: 'Report', hint: 'NIP-56 moderation report' },
  { kind: 9735, label: 'Zap receipt' },
  { kind: 9321, label: 'Nutzap' },
  { kind: 2390, label: 'Obelisk game event', hint: 'High volume — game moves' },
  { kind: 1059, label: 'Gift wrap', hint: 'NIP-59 — deleting these loses private messages' },
]

const KIND_LABELS: Record<number, string> = {
  0: 'Profile metadata',
  1: 'Note',
  3: 'Contact list',
  4: 'Encrypted DM (legacy)',
  5: 'Deletion request',
  7: 'Reaction',
  9: 'Group chat message',
  10: 'Group chat reply',
  11: 'Group thread',
  12: 'Group thread reply',
  6: 'Repost',
  16: 'Generic repost',
  20: 'Picture',
  1059: 'Gift wrap',
  1111: 'Comment',
  1984: 'Report',
  2390: 'Obelisk game event',
  9021: 'Join request',
  9022: 'Leave request',
  9321: 'Nutzap',
  9735: 'Zap receipt',
  10002: 'Relay list',
  10009: 'Group list',
  10050: 'DM relay list',
  24133: 'Signer (NIP-46)',
  30000: 'Follow set',
  30023: 'Long-form article',
  30311: 'Live event',
  31313: 'Obelisk app event',
  31923: 'Calendar event',
  34550: 'Community definition',
  39000: 'Group metadata',
  39001: 'Group admins',
  39002: 'Group members',
  39003: 'Group roles',
}

const kindLabel = (kind: number) => {
  if (KIND_LABELS[kind]) return KIND_LABELS[kind]
  if (kind >= 9000 && kind <= 9011) return 'Group management'
  return `Kind ${kind}`
}

const formatBytes = (bytes: number) => {
  if (bytes === 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const index = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1)
  return `${(bytes / Math.pow(1024, index)).toFixed(index === 0 ? 0 : 1)} ${units[index]}`
}

const formatNumber = (n: number) => n.toLocaleString()

/**
 * Disk usage over time, as an inline SVG area chart.
 *
 * Hand-drawn rather than pulled from a charting library: the relay serves its
 * frontend under a strict CSP with no external origins, the bundle is already
 * 1.1MB, and this is one series of at most 720 points. An SVG path is a few
 * lines and has no supply chain.
 *
 * Reads the LMDB file size, so it includes reclaimable free-list slack. A flat
 * event count beside a rising line is a database that wants compacting rather
 * than pruning -- which is exactly what happened here, and what event counts
 * alone could never have shown.
 */
const StorageChart = ({ samples }: { samples: StorageSample[] }) => {
  if (samples.length < 2) {
    return (
      <p class="text-xs" style={{ color: 'var(--color-text-secondary)' }}>
        Collecting — the relay samples its own size hourly, so the first points
        appear over the next few hours.
      </p>
    )
  }

  const W = 640
  const H = 140
  const PAD_L = 8
  const PAD_B = 18

  const values = samples.map(s => s.db_bytes)
  const peak = Math.max(...values)
  const floor = 0 // anchor at zero: a truncated axis exaggerates growth
  const span = peak - floor || 1

  const first = samples[0].at
  const last = samples[samples.length - 1].at
  const timeSpan = last - first || 1

  const x = (at: number) => PAD_L + ((at - first) / timeSpan) * (W - PAD_L * 2)
  const y = (b: number) => (H - PAD_B) - ((b - floor) / span) * (H - PAD_B - 8)

  const line = samples.map((s, i) => `${i === 0 ? 'M' : 'L'}${x(s.at).toFixed(1)},${y(s.db_bytes).toFixed(1)}`).join(' ')
  const area = `${line} L${x(last).toFixed(1)},${H - PAD_B} L${x(first).toFixed(1)},${H - PAD_B} Z`

  const current = values[values.length - 1]
  const earliest = values[0]
  const delta = current - earliest

  return (
    <div>
      <svg
        class="admin-storage-chart"
        viewBox={`0 0 ${W} ${H}`}
        preserveAspectRatio="none"
        role="img"
        aria-label={`Database size over time, currently ${formatBytes(current)}`}
      >
        <defs>
          <linearGradient id="storageFill" x1="0" y1="0" x2="0" y2="1">
            <stop offset="0%" stop-color="var(--color-accent)" stop-opacity="0.22" />
            <stop offset="100%" stop-color="var(--color-accent)" stop-opacity="0" />
          </linearGradient>
        </defs>
        <line x1={PAD_L} y1={H - PAD_B} x2={W - PAD_L} y2={H - PAD_B} class="admin-storage-chart-axis" />
        <path d={area} fill="url(#storageFill)" />
        <path d={line} class="admin-storage-chart-line" />
      </svg>
      <div class="admin-storage-chart-legend">
        <span>{formatUnix(first)}</span>
        <span>
          {formatBytes(current)} now
          {delta !== 0 && (
            <span style={{ color: delta > 0 ? '#eab308' : 'var(--color-accent)' }}>
              {' '}({delta > 0 ? '+' : '−'}{formatBytes(Math.abs(delta))} over this window)
            </span>
          )}
        </span>
      </div>
    </div>
  )
}

const formatUnix = (unix: number) => {
  if (!unix) return 'Never'
  return new Date(unix * 1000).toLocaleString()
}

const relativeAge = (unix: number) => {
  if (!unix) return ''
  const secs = Math.max(0, Math.floor(Date.now() / 1000) - unix)
  if (secs < 60) return 'just now'
  if (secs < 3600) return `${Math.floor(secs / 60)}m ago`
  if (secs < 86400) return `${Math.floor(secs / 3600)}h ago`
  return `${Math.floor(secs / 86400)}d ago`
}

export const StorageManager = () => {
  const [settings, setSettings] = useState<StorageSettings | null>(null)
  const [stats, setStats] = useState<StorageStats | null>(null)
  const [counting, setCounting] = useState(false)
  const [statsError, setStatsError] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [toast, setToast] = useState<string | null>(null)

  // Local draft of the pruning form. Kept separate from `settings` so the
  // server's fallback display values never masquerade as configured policy.
  const [armed, setArmed] = useState(false)
  const [intervalMinutes, setIntervalMinutes] = useState<string>('')
  // Retention per kind, in days, as strings so a half-typed value does not
  // momentarily read as 0 days.
  const [policyDays, setPolicyDays] = useState<Record<number, string>>({})
  const [confirmText, setConfirmText] = useState('')
  // Exact counts fetched on demand, keyed by kind.
  const [exact, setExact] = useState<Record<number, { total: number; olderThan?: number }>>({})
  const [countingKind, setCountingKind] = useState<number | null>(null)
  const [recipientTarget, setRecipientTarget] = useState<string | null>(null)
  const [recipientConfirm, setRecipientConfirm] = useState('')
  const [recipientBusy, setRecipientBusy] = useState(false)
  const [history, setHistory] = useState<StorageSample[]>([])

  const loadHistory = () => {
    adminApi.getStorageHistory()
      .then(r => setHistory(r.samples))
      // Advisory: a missing history must not blank the screen.
      .catch(() => setHistory([]))
  }

  const loadSettings = () => {
    setLoading(true)
    adminApi.getStorageSettings()
      .then(data => {
        setSettings(data)
        setArmed(data.configured_pruning_enabled)
        // Only prefill the destructive numbers when pruning is genuinely
        // configured. On a relay that never enabled it, these stay blank so
        // the operator has to choose a window deliberately.
        if (data.configured_pruning_enabled) {
          setIntervalMinutes(String(data.prune_interval_minutes))
          const secs = data.policies_secs ?? {}
          const asDays: Record<number, string> = {}
          for (const [kind, s] of Object.entries(secs)) {
            asDays[Number(kind)] = String(Math.round(Number(s) / 86400))
          }
          setPolicyDays(asDays)
        }
        setError(null)
      })
      .catch(e => setError(e.message))
      .finally(() => setLoading(false))
  }

  // Counting a multi-GB database outlives an HTTP request, so the server
  // scans in the background and we poll until a fresh snapshot lands.
  const loadStats = (refresh = false) => {
    setCounting(true)
    adminApi.getStorageStats(refresh)
      .then(data => {
        setStats(data.stats)
        setStatsError(null)
        setCounting(data.computing)
      })
      .catch(e => { setStatsError(e.message); setCounting(false) })
  }

  useEffect(() => { loadSettings(); loadStats(); loadHistory() }, [])

  useEffect(() => {
    if (!counting) return
    const id = setInterval(() => {
      adminApi.getStorageStats()
        .then(data => {
          setStats(data.stats)
          setCounting(data.computing)
        })
        .catch(() => undefined)
    }, 3000)
    return () => clearInterval(id)
  }, [counting])

  const activePolicies = Object.entries(policyDays)
    .map(([kind, days]) => [Number(kind), Number(days)] as const)
    .filter(([, days]) => Number.isInteger(days) && days >= 1)

  const isEnabling = armed && !settings?.configured_pruning_enabled
  const confirmed = !isEnabling || confirmMatches(confirmText, 'DELETE')
  const intervalNum = Number(intervalMinutes)
  const formValid = !armed || (
    Number.isInteger(intervalNum) && intervalNum >= 1 && activePolicies.length > 0
  )

  // Counting one kind takes tens of seconds on a multi-GB database, so the
  // server computes in the background and we poll for the result.
  const countExactly = async (kind: number) => {
    setCountingKind(kind)
    const days = Number(policyDays[kind])
    const olderThan = Number.isInteger(days) && days >= 1 ? days : undefined
    try {
      // Measured at 434s for kind 1059 on the 4.2 GB production database --
      // far longer than the ~12s a smaller kind takes -- so allow 15 minutes.
      // The server caches the result either way, so giving up early only
      // costs the operator a second click.
      for (let attempt = 0; attempt < 300; attempt += 1) {
        const res = await adminApi.getExactKindCount(kind, olderThan)
        if (res.result) {
          setExact(prev => ({
            ...prev,
            [kind]: { total: res.result!.total, olderThan: res.result!.older_than },
          }))
          return
        }
        await new Promise(r => setTimeout(r, 3000))
      }
      setError('Still counting. The result is cached when it lands — click "count exactly" again in a few minutes.')
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Exact count failed')
    } finally {
      setCountingKind(null)
    }
  }

  const deleteWrapsFor = async (pubkey: string) => {
    setRecipientBusy(true)
    setError(null)
    try {
      const res = await adminApi.bulkDeleteByRecipient([pubkey], [1059])
      setToast(
        res.failed === 0
          ? `Deleted ${res.deleted} gift wrap${res.deleted !== 1 ? 's' : ''} addressed to that pubkey.`
          : `Deleted ${res.deleted}; the request reported ${res.failed} failure(s).`,
      )
      setTimeout(() => setToast(null), 6000)
      setRecipientTarget(null)
      setRecipientConfirm('')
      loadStats(true)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to delete gift wraps')
    } finally {
      setRecipientBusy(false)
    }
  }

  const save = async () => {
    if (!settings) return
    setSaving(true)
    setError(null)
    try {
      const next = await adminApi.updateStorageSettings({
        pruning_enabled: armed,
        prune_interval_minutes: armed ? intervalNum : (settings.prune_interval_minutes || 60),
        retention_days_by_kind: Object.fromEntries(activePolicies),
      })
      setSettings(next)
      setConfirmText('')
      setToast(
        armed
          ? `Pruning armed for ${activePolicies.length} kind${activePolicies.length !== 1 ? 's' : ''}. It takes effect on the next relay restart.`
          : 'Pruning disabled. No events will be deleted.',
      )
      setTimeout(() => setToast(null), 6000)
      loadStats(true)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to save storage settings')
    } finally {
      setSaving(false)
    }
  }

  return (
    <div>
      <p class="text-sm mb-6" style={{ color: 'var(--color-text-secondary)' }}>
        Automatic pruning is off unless explicitly armed below.
      </p>

      {toast && (
        <div class="mb-4 p-3 rounded-lg text-sm border" style={{ background: 'rgba(180,249,83,0.08)', color: '#b4f953', borderColor: 'rgba(180,249,83,0.2)' }}>
          {toast}
        </div>
      )}

      {error && (
        <div class="mb-4 p-3 rounded-lg text-sm bg-red-500/10 text-red-400 border border-red-500/20">
          {error}
        </div>
      )}

      {loading || !settings ? (
        <div class="space-y-3">
          <div class="lc-skeleton h-28 w-full" />
          <div class="lc-skeleton h-56 w-full" />
        </div>
      ) : (
        <div class="space-y-5">
          {/* ---------- What is stored ---------- */}
          <section class="admin-settings-card">
            <div class="admin-settings-card-header">
              <div>
                <h3>What is stored</h3>
                <p>
                  {counting
                    ? stats
                      ? `Recounting in the background — showing figures from ${relativeAge(stats.computed_at)}.`
                      : 'Counting events by kind. This can take a few minutes on a large database.'
                    : stats
                      ? `Counted ${relativeAge(stats.computed_at)}.`
                      : 'No counts yet.'}
                </p>
              </div>
              <button
                type="button"
                onClick={() => loadStats(true)}
                disabled={counting}
                class="lc-pill text-sm"
                style={{ borderRadius: '8px', padding: '7px 14px' }}
              >
                {counting ? 'Counting...' : 'Recount'}
              </button>
            </div>

            <div class="admin-storage-stats mt-4">
              <div class="admin-stat-card">
                <span>Database size</span>
                <strong>{formatBytes(settings.db_size_bytes)}</strong>
              </div>
              <div class="admin-stat-card">
                <span>{stats?.sample_is_complete ? 'Events stored' : 'Events sampled'}</span>
                <strong>{stats ? formatNumber(stats.sampled_events) : '—'}</strong>
              </div>
              <div class="admin-stat-card">
                <span>Newest event</span>
                <strong>{stats?.newest_event_unix ? relativeAge(stats.newest_event_unix) : '—'}</strong>
              </div>
              <div class="admin-stat-card">
                <span>Events deleted</span>
                <strong>{formatNumber(settings.total_pruned)}</strong>
              </div>
            </div>

            <div class="admin-storage-chart-block">
              <div class="admin-storage-chart-head">
                <h4>Disk used over time</h4>
                <p>
                  The file on disk, sampled hourly. Includes space freed by
                  deletion but not yet returned to the filesystem — LMDB reuses
                  it internally and never shrinks the file, so a flat event count
                  beside a rising line means the database wants compacting rather
                  than pruning.
                </p>
              </div>
              <StorageChart samples={history} />
            </div>

            {statsError && (
              <div class="mt-4 p-3 rounded-lg text-sm bg-red-500/10 text-red-400 border border-red-500/20">
                {statsError}
              </div>
            )}

            {counting && !stats && <div class="lc-skeleton h-40 w-full mt-4" />}

            {stats && stats.kinds.length > 0 && (
              <div class="mt-4 overflow-x-auto">
                <table class="w-full text-sm">
                  <thead>
                    <tr style={{ color: 'var(--color-text-secondary)' }}>
                      <th class="text-left font-medium p-2">Kind</th>
                      <th class="text-left font-medium p-2">Type</th>
                      <th class="text-right font-medium p-2">Events</th>
                      <th class="text-right font-medium p-2">Est. size</th>
                      <th class="text-right font-medium p-2">Share</th>
                    </tr>
                  </thead>
                  <tbody>
                    {stats.kinds.map(k => {
                      const pct = stats.sampled_events > 0
                        ? (k.count / stats.sampled_events) * 100
                        : 0
                      const protectedKind = NEVER_PRUNE_KINDS.includes(k.kind)
                      return (
                        <tr key={k.kind} style={{ borderTop: '1px solid var(--color-border)' }}>
                          <td class="p-2 font-mono">{k.kind}</td>
                          <td class="p-2">
                            {kindLabel(k.kind)}
                            {protectedKind && (
                              <span class="admin-status-badge ml-2" title="Never pruned">
                                protected
                              </span>
                            )}
                          </td>
                          <td class="p-2 text-right font-mono">{formatNumber(k.count)}</td>
                          <td
                            class="p-2 text-right font-mono"
                            title={`${formatBytes(k.avg_bytes)} average per event across the sample. Content and tags only — index overhead is not counted, so these do not sum to the file on disk.`}
                          >
                            {stats.sample_is_complete ? '' : '≈'}{formatBytes(k.sampled_bytes)}
                          </td>
                          <td class="p-2 text-right" style={{ color: 'var(--color-text-secondary)' }}>
                            {pct < 0.1 && pct > 0 ? '<0.1' : pct.toFixed(1)}%
                          </td>
                        </tr>
                      )
                    })}
                  </tbody>
                </table>
                {!stats.sample_is_complete && (
                  <p class="text-xs mt-3" style={{ color: 'var(--color-text-secondary)' }}>
                    Based on the {formatNumber(stats.sampled_events)} most recent
                    events (back to {formatUnix(stats.oldest_sampled_unix)}), not the
                    whole database — exact per-kind totals are too slow to compute
                    on a database this size. Shares are of the sample. Sizes count
                    content and tags only, so they will not add up to the file on
                    disk: index entries and reclaimable free space are excluded.
                  </p>
                )}
              </div>
            )}
          </section>

          {/* ---------- Pruning ---------- */}
          <section class="admin-settings-card">
            <div class="admin-settings-card-header">
              <div>
                <h3>Automatic deletion</h3>
                <p>
                  Permanently deletes stored events older than a retention
                  window. Off by default and never required.
                </p>
              </div>
              <span class={`admin-status-badge ${settings.pruning_enabled ? 'admin-status-badge-danger' : 'admin-status-badge-ok'}`}>
                {settings.pruning_enabled ? 'Deleting' : 'Nothing is deleted'}
              </span>
            </div>

            {settings.configured_pruning_enabled && !settings.pruning_enabled && (
              <div class="mt-3 p-3 rounded-lg text-sm border" style={{ background: 'rgba(234,179,8,0.08)', color: '#eab308', borderColor: 'rgba(234,179,8,0.25)' }}>
                Pruning is armed in the config but not running — the relay has
                not been restarted since it was enabled. It will start deleting
                on the next restart.
              </div>
            )}

            <label class="admin-toggle-row mt-4">
              <input
                type="checkbox"
                checked={armed}
                onChange={e => {
                  const on = (e.target as HTMLInputElement).checked
                  setArmed(on)
                  setConfirmText('')
                  if (!on) return
                  // Start from an empty, deliberate choice rather than a
                  // prefilled window the operator never picked.
                  if (!settings.configured_pruning_enabled) {
                    setIntervalMinutes('60')
                    setPolicyDays({})
                  }
                }}
              />
              <span>
                <strong>Delete old events automatically</strong>
                <small>
                  Group metadata, membership, roles and all NIP-29 management
                  events are never deleted, whatever is selected.
                </small>
              </span>
            </label>

            {armed && (
              <div class="mt-4 space-y-4">
                <label class="block" style={{ maxWidth: '260px' }}>
                  <span class="block text-sm font-semibold mb-1">Check every (minutes)</span>
                  <input
                    type="number"
                    min="1"
                    placeholder="60"
                    value={intervalMinutes}
                    onInput={e => setIntervalMinutes((e.target as HTMLInputElement).value)}
                  />
                </label>

                <div>
                  <span class="block text-sm font-semibold mb-1">Retention per kind</span>
                  <p class="text-xs mb-3" style={{ color: 'var(--color-text-secondary)' }}>
                    Leave a window blank to keep that kind forever. Each kind has its
                    own window, so short-lived traffic can go without touching
                    conversations.
                  </p>
                  <div class="overflow-x-auto">
                    <table class="w-full text-sm">
                      <thead>
                        <tr style={{ color: 'var(--color-text-secondary)' }}>
                          <th class="text-left font-medium p-2">Kind</th>
                          <th class="text-left font-medium p-2">Type</th>
                          <th class="text-right font-medium p-2">Stored</th>
                          <th class="text-left font-medium p-2">Delete after (days)</th>
                          <th class="text-right font-medium p-2">Would delete now</th>
                        </tr>
                      </thead>
                      <tbody>
                        {PRUNABLE_KINDS.map(k => {
                          const sampled = stats?.kinds.find(s => s.kind === k.kind)
                          const ex = exact[k.kind]
                          const deletedSoFar = settings.deleted_by_kind?.[k.kind]
                          return (
                            <tr key={k.kind} style={{ borderTop: '1px solid var(--color-border)' }}>
                              <td class="p-2 font-mono">{k.kind}</td>
                              <td class="p-2">
                                {k.label}
                                {k.hint && (
                                  <span class="block text-xs" style={{ color: 'var(--color-text-secondary)' }}>
                                    {k.hint}
                                  </span>
                                )}
                                {deletedSoFar ? (
                                  <span class="block text-xs" style={{ color: '#fca5a5' }}>
                                    {formatNumber(deletedSoFar)} deleted so far
                                  </span>
                                ) : null}
                              </td>
                              <td class="p-2 text-right font-mono">
                                {ex ? (
                                  <span title="Exact count">{formatNumber(ex.total)}</span>
                                ) : sampled ? (
                                  <span style={{ color: 'var(--color-text-secondary)' }} title="From the sample, not a total">
                                    ~{formatNumber(sampled.count)}
                                  </span>
                                ) : (
                                  <span style={{ color: 'var(--color-text-secondary)' }}>—</span>
                                )}
                                <button
                                  type="button"
                                  class="block ml-auto text-xs underline"
                                  style={{ color: 'var(--color-text-secondary)' }}
                                  disabled={countingKind !== null}
                                  onClick={() => countExactly(k.kind)}
                                >
                                  {countingKind === k.kind ? 'counting… (minutes)' : 'count exactly'}
                                </button>
                              </td>
                              <td class="p-2">
                                <input
                                  type="number"
                                  min="1"
                                  style={{ width: '90px' }}
                                  placeholder="keep"
                                  value={policyDays[k.kind] ?? ''}
                                  onInput={e => {
                                    const v = (e.target as HTMLInputElement).value
                                    setPolicyDays(prev => {
                                      const next = { ...prev }
                                      if (v === '') delete next[k.kind]
                                      else next[k.kind] = v
                                      return next
                                    })
                                  }}
                                />
                              </td>
                              <td class="p-2 text-right font-mono">
                                {ex?.olderThan !== undefined ? (
                                  <span style={{ color: ex.olderThan > 0 ? '#fca5a5' : undefined }}>
                                    {formatNumber(ex.olderThan)}
                                  </span>
                                ) : (
                                  <span style={{ color: 'var(--color-text-secondary)' }}>
                                    count to see
                                  </span>
                                )}
                              </td>
                            </tr>
                          )
                        })}
                      </tbody>
                    </table>
                  </div>
                  <p class="text-xs mt-2" style={{ color: 'var(--color-text-secondary)' }}>
                    Protected kinds ({NEVER_PRUNE_KINDS[0]}–{NEVER_PRUNE_KINDS[11]},{' '}
                    {NEVER_PRUNE_KINDS[12]}–{NEVER_PRUNE_KINDS[15]}) are not listed
                    because the relay refuses to delete them.
                  </p>
                </div>

                {isEnabling && (
                  <div class="p-4 rounded-lg border" style={{ background: 'rgba(248,113,113,0.06)', borderColor: 'rgba(248,113,113,0.25)' }}>
                    <p class="text-sm mb-3" style={{ color: '#fca5a5' }}>
                      This permanently deletes stored events on every run. There is no
                      undo and no backup is taken. Gift wraps (1059) are private
                      messages — the relay holds no other copy, so deleting them
                      destroys them for the recipients too. Type <strong>DELETE</strong> to confirm.
                    </p>
                    <input
                      type="text"
                      class={`admin-confirm-input ${confirmMatches(confirmText, 'DELETE') ? 'is-valid' : ''}`}
                      value={confirmText}
                      placeholder="DELETE"
                      aria-label="Type DELETE to confirm enabling automatic deletion"
                      onInput={e => setConfirmText((e.target as HTMLInputElement).value)}
                    />
                  </div>
                )}
              </div>
            )}

            <div class="admin-settings-actions mt-4">
              <div class="text-sm" style={{ color: 'var(--color-text-secondary)' }}>
                Last run: {formatUnix(settings.last_run_unix)} · Runs: {settings.runs}
              </div>
              <button
                type="button"
                onClick={save}
                disabled={saving || !confirmed || !formValid}
                class="lc-pill-primary text-sm"
                style={{ borderRadius: '8px', padding: '9px 18px' }}
              >
                {saving ? 'Saving...' : 'Save storage settings'}
              </button>
            </div>
          </section>

          {/* Gift wraps are signed by one-time keys, so the p tag is the only
              per-user handle on what is usually the bulk of a relay's storage. */}
          {stats && stats.top_recipients.length > 0 && (
            <section class="admin-settings-card">
              <div class="admin-settings-card-header">
                <div>
                  <h3>Private message recipients</h3>
                  <p>
                    Busiest gift-wrap (kind 1059) recipients in the sample. Senders
                    cannot be shown — NIP-59 signs every wrap with a throwaway key,
                    by design.
                  </p>
                </div>
              </div>

              <div class="mt-4 overflow-x-auto">
                <table class="w-full text-sm">
                  <thead>
                    <tr style={{ color: 'var(--color-text-secondary)' }}>
                      <th class="text-left font-medium p-2">Recipient</th>
                      <th class="text-right font-medium p-2">Wraps</th>
                      <th class="text-right font-medium p-2">Share</th>
                      <th class="p-2" />
                    </tr>
                  </thead>
                  <tbody>
                    {stats.top_recipients.map(r => {
                      const wraps = stats.kinds.find(k => k.kind === 1059)?.count ?? 0
                      const pct = wraps > 0 ? (r.count / wraps) * 100 : 0
                      return (
                        <tr key={r.pubkey} style={{ borderTop: '1px solid var(--color-border)' }}>
                          <td class="p-2 font-mono text-xs" title={r.pubkey}>
                            {r.pubkey.slice(0, 16)}…
                          </td>
                          <td class="p-2 text-right font-mono">{formatNumber(r.count)}</td>
                          <td class="p-2 text-right" style={{ color: 'var(--color-text-secondary)' }}>
                            {pct.toFixed(1)}%
                          </td>
                          <td class="p-2 text-right">
                            {recipientTarget === r.pubkey ? (
                              <span class="flex items-center justify-end gap-2">
                                <input
                                  type="text"
                                  class={`admin-confirm-input ${confirmMatches(recipientConfirm, 'DELETE') ? 'is-valid' : ''}`}
                                  style={{ maxWidth: '130px' }}
                                  value={recipientConfirm}
                                  placeholder="DELETE"
                                  aria-label="Type DELETE to confirm"
                                  onInput={e => setRecipientConfirm((e.target as HTMLInputElement).value)}
                                />
                                <button
                                  type="button"
                                  disabled={recipientBusy || !confirmMatches(recipientConfirm, 'DELETE')}
                                  onClick={() => deleteWrapsFor(r.pubkey)}
                                  class="text-xs px-2 py-1 rounded"
                                  style={{ background: 'rgba(239,68,68,0.2)', color: '#f87171', border: '1px solid rgba(239,68,68,0.4)' }}
                                >
                                  {recipientBusy ? '…' : 'Confirm'}
                                </button>
                                <button
                                  type="button"
                                  onClick={() => { setRecipientTarget(null); setRecipientConfirm('') }}
                                  class="text-xs"
                                  style={{ color: 'var(--color-text-secondary)' }}
                                >
                                  Cancel
                                </button>
                              </span>
                            ) : (
                              <button
                                type="button"
                                onClick={() => { setRecipientTarget(r.pubkey); setRecipientConfirm('') }}
                                class="text-xs text-red-400 hover:text-red-300 opacity-60 hover:opacity-100"
                              >
                                Delete their wraps
                              </button>
                            )}
                          </td>
                        </tr>
                      )
                    })}
                  </tbody>
                </table>
              </div>

              <p class="text-xs mt-3" style={{ color: '#fca5a5' }}>
                Deleting a recipient's gift wraps destroys those private messages.
                The relay holds no other copy and clients fetch DM history from it.
              </p>
            </section>
          )}

          <section class="admin-settings-card">
            <div class="admin-settings-card-header">
              <div>
                <h3>Database</h3>
                <p class="font-mono break-all">{settings.db_path}</p>
              </div>
              <span class="text-sm" style={{ color: 'var(--color-text-secondary)' }}>
                {settings.db_file_count} files
                {stats && stats.scope_count > 1 ? ` · ${stats.scope_count} scopes` : ''}
              </span>
            </div>
          </section>
        </div>
      )}
    </div>
  )
}
