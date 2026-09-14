import { useEffect, useState } from 'preact/hooks'
import {
  adminApi,
  type StorageSettings,
  type StorageStats,
} from '../../services/AdminApiClient'
import { StorageIcon } from './icons'

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
  const [retentionDays, setRetentionDays] = useState<string>('')
  const [intervalMinutes, setIntervalMinutes] = useState<string>('')
  const [selectedKinds, setSelectedKinds] = useState<number[]>([])
  const [confirmText, setConfirmText] = useState('')

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
          setRetentionDays(String(data.retention_days))
          setIntervalMinutes(String(data.prune_interval_minutes))
          setSelectedKinds(data.prune_kinds)
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

  useEffect(() => { loadSettings(); loadStats() }, [])

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

  const toggleKind = (kind: number) => {
    setSelectedKinds(prev =>
      prev.includes(kind) ? prev.filter(k => k !== kind) : [...prev, kind],
    )
  }

  const isEnabling = armed && !settings?.configured_pruning_enabled
  const confirmed = !isEnabling || confirmText.trim() === 'DELETE'

  const retentionNum = Number(retentionDays)
  const intervalNum = Number(intervalMinutes)
  const formValid = !armed || (
    Number.isInteger(retentionNum) && retentionNum >= 1 &&
    Number.isInteger(intervalNum) && intervalNum >= 1 &&
    selectedKinds.length > 0
  )

  const save = async () => {
    if (!settings) return
    setSaving(true)
    setError(null)
    try {
      const next = await adminApi.updateStorageSettings({
        pruning_enabled: armed,
        retention_days: armed ? retentionNum : (settings.retention_days || 30),
        prune_interval_minutes: armed ? intervalNum : (settings.prune_interval_minutes || 60),
        prune_kinds: armed ? selectedKinds : (settings.prune_kinds.length ? settings.prune_kinds : [9, 11, 12]),
      })
      setSettings(next)
      setConfirmText('')
      setToast(
        armed
          ? 'Pruning armed. It takes effect on the next relay restart.'
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
      <div class="flex items-center gap-2 mb-2">
        <StorageIcon class="w-5 h-5" />
        <h2 class="text-xl font-bold">Storage</h2>
      </div>
      <p class="text-sm mb-6" style={{ color: 'var(--color-text-secondary)' }}>
        What this relay has stored, and whether anything is being deleted.
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
                    on a database this size. Shares are of the sample.
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
                    setRetentionDays('')
                    setIntervalMinutes('60')
                    setSelectedKinds([])
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
                <div class="admin-rate-grid">
                  <label>
                    <span>Delete events older than (days)</span>
                    <input
                      type="number"
                      min="1"
                      placeholder="e.g. 90"
                      value={retentionDays}
                      onInput={e => setRetentionDays((e.target as HTMLInputElement).value)}
                    />
                  </label>
                  <label>
                    <span>Check every (minutes)</span>
                    <input
                      type="number"
                      min="1"
                      placeholder="60"
                      value={intervalMinutes}
                      onInput={e => setIntervalMinutes((e.target as HTMLInputElement).value)}
                    />
                  </label>
                </div>

                <div>
                  <span class="block text-sm font-semibold mb-2">Which events to delete</span>
                  <div class="grid grid-cols-1 sm:grid-cols-2 gap-2">
                    {PRUNABLE_KINDS.map(k => {
                      const stat = stats?.kinds.find(s => s.kind === k.kind)
                      return (
                        <label key={k.kind} class="admin-toggle-row">
                          <input
                            type="checkbox"
                            checked={selectedKinds.includes(k.kind)}
                            onChange={() => toggleKind(k.kind)}
                          />
                          <span>
                            <strong>{k.label} <span class="font-mono opacity-60">({k.kind})</span></strong>
                            <small>
                              {stat ? `${formatNumber(stat.count)} stored` : 'none stored'}
                              {k.hint ? ` · ${k.hint}` : ''}
                            </small>
                          </span>
                        </label>
                      )
                    })}
                  </div>
                  <p class="text-xs mt-2" style={{ color: 'var(--color-text-secondary)' }}>
                    Protected kinds ({NEVER_PRUNE_KINDS[0]}–{NEVER_PRUNE_KINDS[11]},{' '}
                    {NEVER_PRUNE_KINDS[12]}–{NEVER_PRUNE_KINDS[15]}) are not
                    listed because the relay refuses to delete them.
                  </p>
                </div>

                {/* Blast radius, computed against the saved config. */}
                {stats && settings.configured_pruning_enabled && stats.prune_preview > 0 && (
                  <div class="p-3 rounded-lg text-sm border" style={{ background: 'rgba(248,113,113,0.08)', color: '#fca5a5', borderColor: 'rgba(248,113,113,0.25)' }}>
                    With the currently saved settings
                    ({stats.prune_preview_retention_days} days, kinds{' '}
                    {stats.prune_preview_kinds.join(', ')}),{' '}
                    <strong>{formatNumber(stats.prune_preview)}</strong> stored
                    events are already older than the window and would be
                    deleted on the next prune run.
                  </div>
                )}

                {isEnabling && (
                  <div class="p-4 rounded-lg border" style={{ background: 'rgba(248,113,113,0.06)', borderColor: 'rgba(248,113,113,0.25)' }}>
                    <p class="text-sm mb-3" style={{ color: '#fca5a5' }}>
                      This permanently deletes stored events on every run. There
                      is no undo and no backup is taken. Type <strong>DELETE</strong> to confirm.
                    </p>
                    <input
                      type="text"
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
