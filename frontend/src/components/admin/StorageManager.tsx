import { useEffect, useState } from 'preact/hooks'
import { SearchIcon } from './SearchIcon'
import { TimeSeriesChart } from './TimeSeriesChart'
import {
  adminApi,
  type Attribution,
  type StorageAuthorStat,
  type StorageKindAuthors,
  type PruneResult,
  type StorageSettings,
  type StorageStats,
  type StorageSample,
  type CompactionStatus,
} from '../../services/AdminApiClient'
import { confirmMatches } from './confirmPhrase'
import { useDirtySection } from './settingsDirty'
import { fetchProfiles, getDisplayName, type NostrProfile } from '../../services/ProfileFetcher'
import { ProfileCard, CopyNpubButton } from './ProfileCard'

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

/**
 * Reclaiming the disk that pruning does not.
 *
 * Deleting events frees LMDB pages for reuse but never returns them to the
 * filesystem, so the file stays at its high-water mark and the graph above it
 * never goes down. This is the control that fixes that. It is its own card
 * rather than a line in the stats grid because it costs a restart, and because
 * "how much would this get back" is the number an operator needs before
 * agreeing to one.
 *
 * Self-contained state: measuring walks the free list in a child process on the
 * relay, so it must not be tied to the stats polling loop above.
 */
const CompactionCard = () => {
  const [status, setStatus] = useState<CompactionStatus | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState('')
  const [confirm, setConfirm] = useState('')
  const [busy, setBusy] = useState(false)
  /** Set once the relay has been told to restart, so the card can wait for it. */
  const [restarting, setRestarting] = useState(false)

  const load = async (refresh = false) => {
    setLoading(true)
    setError('')
    try {
      setStatus(await adminApi.getCompactionStatus(refresh))
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Could not read compaction status')
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => { void load() }, [])

  /**
   * Wait out the restart, then show what the compaction actually did.
   *
   * The failures here are the expected middle of the operation, not errors: the
   * relay exits, Docker restarts it, and the compaction runs before it starts
   * listening. Anything that treated a refused connection as a failure would
   * report every successful compaction as broken.
   */
  const waitForRelay = async () => {
    const deadline = Date.now() + 5 * 60 * 1000
    while (Date.now() < deadline) {
      await new Promise(resolve => setTimeout(resolve, 3000))
      try {
        const fresh = await adminApi.getCompactionStatus(true)
        setStatus(fresh)
        setRestarting(false)
        return
      } catch {
        // Still down, or still compacting. Keep waiting.
      }
    }
    setRestarting(false)
    setError('The relay did not come back within five minutes. Check the container logs.')
  }

  const compact = async () => {
    setBusy(true)
    setError('')
    try {
      await adminApi.compactDatabase(confirm)
      setConfirm('')
      setRestarting(true)
      void waitForRelay()
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Could not start the compaction')
    } finally {
      setBusy(false)
    }
  }

  const reclaimable = status?.reclaimable_bytes ?? null
  const lastRun = status?.history.length ? status.history[status.history.length - 1] : null
  const ready = confirmMatches(confirm, 'COMPACT')

  return (
    <div class="admin-storage-chart-block">
      <div class="admin-storage-chart-head">
        <h4>Reclaim disk space</h4>
        <p>
          Rebuilds the database file without the pages that deleted events left
          behind, and hands that space back to the filesystem. The relay
          restarts to do it — the copy is taken from a snapshot, so it cannot
          run while the relay is writing. Events, groups and membership are
          carried across unchanged.
        </p>
      </div>

      {loading && !status && <div class="lc-skeleton h-24 w-full" />}

      {status && (
        <>
          <div class="admin-storage-stats mt-3">
            <div
              class="admin-stat-card"
              title="The LMDB file as the filesystem sees it."
            >
              <span>File on disk</span>
              <strong>{formatBytes(status.db_file_bytes)}</strong>
            </div>
            <div
              class="admin-stat-card"
              title="Pages holding data the relay still serves. A compaction leaves the file at about this size."
            >
              <span>Actually in use</span>
              <strong>{status.live_bytes === null ? '—' : formatBytes(status.live_bytes)}</strong>
            </div>
            <div
              class="admin-stat-card"
              title="Space freed by deletion that LMDB is holding on to. This is what a compaction returns."
            >
              <span>Reclaimable</span>
              <strong>{reclaimable === null ? '—' : formatBytes(reclaimable)}</strong>
            </div>
            <div
              class="admin-stat-card"
              title="A compaction writes a second copy of the live data before replacing the original, so it needs room for it."
            >
              <span>Free disk</span>
              <strong>
                {status.free_disk_bytes === null ? '—' : formatBytes(status.free_disk_bytes)}
              </strong>
            </div>
          </div>

          <p class="text-sm mt-2" style={{ color: 'var(--color-text-secondary)' }}>
            {status.measure_error
              ? `Measured ${relativeAge(status.measured_at)} — ${status.measure_error}`
              : `Measured ${relativeAge(status.measured_at)}. Needs ${formatBytes(status.required_free_bytes)} free to run.`}
            {' '}
            <button
              type="button"
              onClick={() => load(true)}
              disabled={loading || restarting}
              class="underline"
              style={{ color: 'var(--color-text-secondary)' }}
            >
              {loading ? 'Measuring...' : 'Measure again'}
            </button>
          </p>

          {lastRun && (
            <p class="text-sm mt-1" style={{ color: 'var(--color-text-secondary)' }}>
              {lastRun.status === 'ok'
                ? `Last compaction ${relativeAge(lastRun.at)}: ${formatBytes(lastRun.before_bytes)} → ${formatBytes(lastRun.after_bytes)} in ${(lastRun.duration_ms / 1000).toFixed(1)}s.`
                : `Last compaction ${relativeAge(lastRun.at)} ${lastRun.status}: ${lastRun.detail ?? 'no detail recorded'}`}
            </p>
          )}

          {restarting ? (
            <div class="mt-3 p-3 rounded-lg text-sm bg-amber-500/10 text-amber-300 border border-amber-500/20">
              Compacting and restarting. The console will be unreachable for a
              moment — this page is waiting and will report the result.
            </div>
          ) : status.pending ? (
            <div class="mt-3 p-3 rounded-lg text-sm bg-amber-500/10 text-amber-300 border border-amber-500/20">
              A compaction is staged and will run the next time the relay
              starts.
            </div>
          ) : (
            <div class="admin-danger-card mt-3">
              {status.blocked_reason ? (
                <p class="text-sm">{status.blocked_reason}</p>
              ) : (
                <p class="text-sm">
                  This will reclaim about {formatBytes(reclaimable ?? 0)} and
                  restart the relay. Clients reconnect on their own.
                </p>
              )}
              <div class="flex gap-2 mt-2">
                <input
                  type="text"
                  value={confirm}
                  onInput={e => setConfirm((e.target as HTMLInputElement).value)}
                  placeholder="Type COMPACT to confirm"
                  disabled={!status.can_compact || busy}
                  class="lc-input text-sm"
                />
                <button
                  type="button"
                  onClick={compact}
                  disabled={!status.can_compact || !ready || busy}
                  class="admin-action-btn"
                  style={{ borderRadius: '8px', padding: '7px 14px' }}
                >
                  {busy ? 'Starting...' : 'Compact and restart'}
                </button>
              </div>
            </div>
          )}
        </>
      )}

      {error && (
        <div class="mt-3 p-3 rounded-lg text-sm bg-red-500/10 text-red-400 border border-red-500/20">
          {error}
        </div>
      )}
    </div>
  )
}

/**
 * Says whose data a row is, in the operator's terms.
 *
 * "Sender" and "recipient" are not decoration: for kind 1059 the only pubkey
 * available is the addressee, and blocking them stops them *receiving* mail
 * rather than stopping a spammer. Labelling both cases "pubkey" would invite
 * exactly the wrong action.
 */
const ATTRIBUTION_LABEL: Record<Attribution, string> = {
  author: 'Sender',
  recipient: 'Recipient',
}

const ATTRIBUTION_HINT: Record<Attribution, string> = {
  author: 'Charged to the pubkey that signed these events.',
  recipient:
    'Charged to the pubkey these events are addressed to. Gift wraps are signed by a throwaway key per message, so the sender cannot be identified — blocking this key stops them receiving, not someone else sending.',
}

/** Appears only when rows are ticked, so the table is unchanged until then. */
const BulkBar = ({
  rows,
  selected,
  onClear,
  onDelete,
}: {
  rows: StorageAuthorStat[]
  selected: Set<string>
  onClear: () => void
  onDelete: () => void
}) => {
  const count = rows.filter(r => selected.has(r.pubkey)).length
  if (count === 0) return null
  return (
    <div class="admin-bulk-bar">
      <span>
        <strong>{count}</strong> selected
      </span>
      <span class="flex-1" />
      <button type="button" class="admin-bulk-clear" onClick={onClear}>
        Clear
      </button>
      <button type="button" class="admin-bulk-delete" onClick={onDelete}>
        Delete events…
      </button>
    </div>
  )
}

/** Shared table body for both the per-kind drilldown and the relay-wide list. */
const AuthorTable = (props: {
  rows: StorageAuthorStat[]
  /** Denominator for the share column — bytes of the kind, or of the sample. */
  totalBytes: number
  approximate: boolean
  busyPubkey: string | null
  profiles: Map<string, NostrProfile>
  onOpenProfile: (row: StorageAuthorStat) => void
  onToggleBlacklist: (row: StorageAuthorStat) => void
  onPrune: (row: StorageAuthorStat) => void
  /** Selected pubkeys, for the bulk action. */
  selected: Set<string>
  onToggleSelect: (pubkey: string) => void
  onToggleSelectAll: () => void
  /** Restrict the prune dialog to one kind when opened from a drilldown. */
  kind?: number
}) => (
  <div class="overflow-x-auto">
    <table class="admin-table">
      <thead>
        <tr>
          <th style={{ width: '32px' }}>
            <input
              type="checkbox"
              aria-label="Select all rows"
              checked={props.rows.length > 0 && props.rows.every(r => props.selected.has(r.pubkey))}
              onChange={props.onToggleSelectAll}
            />
          </th>
          <th>Pubkey</th>
          <th class="is-numeric">Events</th>
          <th class="is-numeric">Est. size</th>
          <th class="is-numeric">Share</th>
          <th />
        </tr>
      </thead>
      <tbody>
        {props.rows.map(row => {
          const pct = props.totalBytes > 0 ? (row.sampled_bytes / props.totalBytes) * 100 : 0
          const profile = props.profiles.get(row.pubkey)
          return (
            <tr
              key={row.pubkey}
              style={{
                borderTop: '1px solid var(--color-border)',
                background: props.selected.has(row.pubkey)
                  ? 'rgba(var(--color-accent-rgb), 0.07)'
                  : undefined,
              }}
            >
              <td>
                <input
                  type="checkbox"
                  aria-label={`Select ${row.npub || row.pubkey}`}
                  checked={props.selected.has(row.pubkey)}
                  onChange={() => props.onToggleSelect(row.pubkey)}
                />
              </td>
              <td>
                <div class="flex items-center gap-2.5">
                  {/* Identity is clickable: a raw npub answers "which key" but
                      never "who". Opening the profile is how an operator
                      decides whether a heavy pubkey is a person or a spammer. */}
                  <button
                    type="button"
                    class="admin-author-identity"
                    onClick={() => props.onOpenProfile(row)}
                    disabled={!row.npub}
                    title={row.npub ? 'Show profile' : row.pubkey}
                  >
                    {profile?.picture ? (
                      <img
                        src={profile.picture}
                        alt=""
                        class="w-7 h-7 rounded-full object-cover flex-shrink-0"
                        style={{ border: '1px solid var(--color-border)' }}
                        onError={e => { (e.target as HTMLImageElement).style.display = 'none' }}
                      />
                    ) : (
                      <span
                        class="w-7 h-7 rounded-full flex-shrink-0 flex items-center justify-center text-[10px] font-bold"
                        style={{ background: 'rgba(var(--color-accent-rgb), 0.1)', color: 'var(--color-accent)' }}
                      >
                        {(profile?.name || row.npub.slice(5, 7) || '??').slice(0, 2).toUpperCase()}
                      </span>
                    )}
                    <span class="min-w-0 text-left">
                      <span class="block text-sm truncate">
                        {profile ? getDisplayName(profile, row.npub) : (row.npub ? `${row.npub.slice(0, 14)}…` : `${row.pubkey.slice(0, 16)}…`)}
                      </span>
                      <span class="block text-[11px] font-mono" style={{ color: 'var(--color-text-secondary)' }}>
                        {row.npub ? `${row.npub.slice(0, 16)}…` : 'unparseable pubkey'}
                      </span>
                    </span>
                  </button>
                  {row.npub && <CopyNpubButton npub={row.npub} />}
                  <span
                    class="admin-status-badge"
                    title={ATTRIBUTION_HINT[row.attributed_by]}
                  >
                    {ATTRIBUTION_LABEL[row.attributed_by]}
                  </span>
                  {row.blacklisted && (
                    <span class="admin-status-badge admin-status-badge-danger">blocked</span>
                  )}
                </div>
              </td>
              <td class="is-numeric">{formatNumber(row.count)}</td>
              <td class="is-numeric">
                {props.approximate ? '≈' : ''}
                {formatBytes(row.sampled_bytes)}
              </td>
              <td class="is-numeric" style={{ color: 'var(--color-text-secondary)' }}>
                {pct < 0.1 && pct > 0 ? '<0.1' : pct.toFixed(1)}%
              </td>
              <td class="is-numeric whitespace-nowrap">
                {/* Blocking stops them connecting; it reclaims no disk. Delete
                    is the other half, so both live here. */}
                <button
                  type="button"
                  onClick={() => props.onPrune(row)}
                  class="text-xs mr-3"
                  style={{ color: '#f87171' }}
                  title="Delete stored events for this pubkey"
                >
                  Delete…
                </button>
                <button
                  type="button"
                  disabled={props.busyPubkey === row.pubkey || !row.npub}
                  onClick={() => props.onToggleBlacklist(row)}
                  class="text-xs"
                  style={{
                    color: row.blacklisted ? 'var(--color-text-secondary)' : '#f87171',
                    opacity: row.npub ? 1 : 0.4,
                  }}
                  title={
                    row.npub
                      ? undefined
                      : 'This pubkey came from a tag and is not valid hex, so it cannot be blocked.'
                  }
                >
                  {props.busyPubkey === row.pubkey ? '…' : row.blacklisted ? 'Unblock' : 'Block'}
                </button>
              </td>
            </tr>
          )
        })}
      </tbody>
    </table>
  </div>
)

/** What the prune dialog is currently aimed at — one row, or a selection. */
interface PruneAim {
  rows: StorageAuthorStat[]
  /** Kinds offered. One when opened from a drilldown, all seen kinds otherwise. */
  kinds: number[]
}

/**
 * Delete a pubkey's stored events, narrowed by kind and date.
 *
 * Always previews first, and the count shown is the count deleted — server-side
 * both run off the same filter. Protected NIP-29 kinds are refused regardless
 * of what is selected here, so pruning a group's creator cannot orphan it.
 */
const PruneDialog = ({
  aim,
  onClose,
  onDone,
}: {
  aim: PruneAim
  onClose: () => void
  onDone: (deleted: number) => void
}) => {
  const [kinds, setKinds] = useState<number[]>(aim.kinds.slice(0, 1))
  const [sinceDate, setSinceDate] = useState('')
  const [untilDate, setUntilDate] = useState('')
  const [preview, setPreview] = useState<PruneResult | null>(null)
  const [confirm, setConfirm] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const toUnix = (d: string, endOfDay = false) => {
    if (!d) return undefined
    const ms = new Date(`${d}T${endOfDay ? '23:59:59' : '00:00:00'}`).getTime()
    return Number.isNaN(ms) ? undefined : Math.floor(ms / 1000)
  }

  const request = {
    // Attribution travels per pubkey: a selection can mix gift-wrap recipients
    // with ordinary authors, and one rule for both would hit the wrong people.
    targets: aim.rows.map(r => ({ pubkey: r.pubkey, attributed_by: r.attributed_by })),
    kinds,
    since: toUnix(sinceDate),
    until: toUnix(untilDate, true),
  }

  // Any change to the filter invalidates a previous count.
  useEffect(() => {
    setPreview(null)
    setConfirm('')
  }, [kinds.join(','), sinceDate, untilDate])

  const runPreview = async () => {
    setBusy(true)
    setError(null)
    try {
      setPreview(await adminApi.pruneEvents({ ...request, dry_run: true }))
    } catch (e) {
      setError((e as Error).message)
    } finally {
      setBusy(false)
    }
  }

  const runDelete = async () => {
    setBusy(true)
    setError(null)
    try {
      const result = await adminApi.pruneEvents({ ...request, confirm })
      onDone(result.deleted)
    } catch (e) {
      setError((e as Error).message)
    } finally {
      setBusy(false)
    }
  }

  const single = aim.rows.length === 1 ? aim.rows[0] : null
  const label = single
    ? (single.npub ? `${single.npub.slice(0, 18)}…` : single.pubkey.slice(0, 16))
    : `${aim.rows.length} pubkeys`
  // Mixed selections get both explanations, because the rules differ.
  const modes = [...new Set(aim.rows.map(r => r.attributed_by))]

  return (
    <div class="admin-modal-backdrop" onClick={onClose}>
      <div class="admin-modal" onClick={e => e.stopPropagation()}>
        <h3 class="admin-modal-title">Delete stored events</h3>
        <p class="admin-modal-copy">
          {single ? (
            <>{ATTRIBUTION_LABEL[single.attributed_by]} <code>{label}</code>.</>
          ) : (
            <><code>{label}</code> selected.</>
          )}{' '}
          {modes.map(m => ATTRIBUTION_HINT[m]).join(' ')}
        </p>

        <div class="admin-modal-field">
          <span>Kinds</span>
          <div class="admin-kind-chips">
            {aim.kinds.map(k => (
              <label key={k} class={`admin-kind-chip ${kinds.includes(k) ? 'is-on' : ''}`}>
                <input
                  type="checkbox"
                  checked={kinds.includes(k)}
                  onChange={e =>
                    setKinds(prev =>
                      (e.target as HTMLInputElement).checked
                        ? [...prev, k]
                        : prev.filter(x => x !== k),
                    )
                  }
                />
                {k} · {kindLabel(k)}
              </label>
            ))}
          </div>
          {kinds.some(k => NEVER_PRUNE_KINDS.includes(k)) && (
            <p class="admin-modal-note">
              Protected kinds stay selected but are refused server-side — group
              identity and membership are never deleted.
            </p>
          )}
        </div>

        <div class="admin-rate-grid">
          <label>
            <span>From (optional)</span>
            <input type="date" value={sinceDate} onInput={e => setSinceDate((e.target as HTMLInputElement).value)} />
          </label>
          <label>
            <span>Until (optional)</span>
            <input type="date" value={untilDate} onInput={e => setUntilDate((e.target as HTMLInputElement).value)} />
          </label>
        </div>
        <p class="admin-modal-note">
          Leave both empty to delete every matching event regardless of age.
        </p>

        {error && <p class="admin-modal-error">{error}</p>}

        {preview && (
          <div class="admin-modal-preview">
            {preview.matched === 0 ? (
              <strong>Nothing matches this filter.</strong>
            ) : (
              <>
                <strong>{formatNumber(preview.matched)} event{preview.matched === 1 ? '' : 's'} will be deleted.</strong>
                <span>
                  This is exact, not sampled, and cannot be undone. The relay keeps
                  no other copy.
                </span>
              </>
            )}
          </div>
        )}

        {preview && preview.matched > 0 && (
          <div class="admin-modal-field">
            <span>Type DELETE to confirm</span>
            <input
              type="text"
              class={`admin-confirm-input ${confirmMatches(confirm, 'DELETE') ? 'is-valid' : ''}`}
              value={confirm}
              placeholder="DELETE"
              onInput={e => setConfirm((e.target as HTMLInputElement).value)}
            />
          </div>
        )}

        <div class="admin-modal-actions">
          <button type="button" class="admin-save-bar-discard" onClick={onClose} disabled={busy}>
            Cancel
          </button>
          {!preview ? (
            <button
              type="button"
              class="admin-save-bar-save"
              onClick={runPreview}
              disabled={busy || kinds.length === 0}
            >
              {busy ? 'Counting…' : 'Preview'}
            </button>
          ) : (
            <button
              type="button"
              class="admin-modal-danger"
              onClick={runDelete}
              disabled={busy || preview.matched === 0 || !confirmMatches(confirm, 'DELETE')}
            >
              {busy ? 'Deleting…' : `Delete ${formatNumber(preview.matched)}`}
            </button>
          )}
        </div>
      </div>
    </div>
  )
}

export const StorageManager = () => {
  const [settings, setSettings] = useState<StorageSettings | null>(null)
  const [stats, setStats] = useState<StorageStats | null>(null)
  /**
   * Filter for the stored-by-kind table.
   *
   * The table lists every kind the relay holds, which on a busy relay is long
   * enough that finding one means scrolling. Matches the kind number and its
   * label, so both "1059" and "gift" get you there.
   */
  const [kindFilter, setKindFilter] = useState('')
  // Chosen kinds, as a set rather than one selection: "show me gift wraps and
  // group messages" is a normal question and a single-select cannot ask it.
  // Empty means no kind restriction, which is how the table starts.
  const [pickedKinds, setPickedKinds] = useState<Set<number>>(new Set())
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
  // Per-kind drilldown: which row is open, and the breakdowns fetched so far.
  // Cached by kind so re-opening a row is free — the server reads the same
  // snapshot either way.
  const [expandedKind, setExpandedKind] = useState<number | null>(null)
  const [kindAuthors, setKindAuthors] = useState<Record<number, StorageKindAuthors>>({})
  const [kindAuthorsLoading, setKindAuthorsLoading] = useState<number | null>(null)
  const [kindAuthorsError, setKindAuthorsError] = useState<string | null>(null)
  const [blacklistBusy, setBlacklistBusy] = useState<string | null>(null)
  // Profiles for the attribution tables, keyed by hex. Fetched lazily and
  // merged across the relay-wide table and every drilldown, so switching
  // between them never refetches a face already on screen.
  const [authorProfiles, setAuthorProfiles] = useState<Map<string, NostrProfile>>(new Map())
  const [profileTarget, setProfileTarget] = useState<{ hex: string; npub: string } | null>(null)

  /** Fetch any pubkeys we do not have a profile for yet. */
  const loadAuthorProfiles = (rows: StorageAuthorStat[]) => {
    const missing = rows
      .filter(r => r.npub && !authorProfiles.has(r.pubkey))
      .map(r => r.pubkey)
    if (missing.length === 0) return
    fetchProfiles(missing)
      .then(fetched => {
        setAuthorProfiles(prev => {
          const next = new Map(prev)
          for (const [hex, profile] of fetched) next.set(hex, profile)
          return next
        })
      })
      // Profiles are decoration; a relay that will not answer must not break
      // the storage screen.
      .catch(() => undefined)
  }

  const toggleKind = (kind: number) => {
    if (expandedKind === kind) {
      setExpandedKind(null)
      return
    }
    setExpandedKind(kind)
    setKindAuthorsError(null)
    if (kindAuthors[kind]) return

    setKindAuthorsLoading(kind)
    adminApi.getStorageKindAuthors(kind)
      .then(data => {
        setKindAuthors(prev => ({ ...prev, [kind]: data }))
        loadAuthorProfiles(data.authors)
      })
      .catch(e => setKindAuthorsError(e.message))
      .finally(() => setKindAuthorsLoading(null))
  }

  /**
   * Block or unblock from any attribution table.
   *
   * Patches the row in place rather than refetching: the storage sample is
   * cached for minutes, so a reload would show the old state and make the
   * button look broken.
   */
  const [pruneAim, setPruneAim] = useState<PruneAim | null>(null)
  /** Rows ticked for a bulk action, by hex. */
  const [selectedRows, setSelectedRows] = useState<Set<string>>(new Set())

  const toggleSelect = (pubkey: string) =>
    setSelectedRows(prev => {
      const next = new Set(prev)
      if (next.has(pubkey)) next.delete(pubkey)
      else next.add(pubkey)
      return next
    })

  const toggleSelectAll = (rows: StorageAuthorStat[]) =>
    setSelectedRows(prev => {
      const all = rows.every(r => prev.has(r.pubkey))
      const next = new Set(prev)
      for (const r of rows) {
        if (all) next.delete(r.pubkey)
        else next.add(r.pubkey)
      }
      return next
    })

  /**
   * Open the prune dialog. From a kind drilldown only that kind is offered;
   * from the relay-wide table, every kind in the sample is, so an operator can
   * clear a spammer's whole footprint in one pass.
   */
  const kindsFor = (kind?: number) => {
    const kinds = kind !== undefined
      ? [kind]
      : (stats?.kinds ?? []).map(k => k.kind).filter(k => !NEVER_PRUNE_KINDS.includes(k))
    return kinds.length > 0 ? kinds : [1]
  }

  const openPrune = (row: StorageAuthorStat, kind?: number) =>
    setPruneAim({ rows: [row], kinds: kindsFor(kind) })

  /** Clear every ticked row in one action. */
  const openBulkPrune = (rows: StorageAuthorStat[], kind?: number) => {
    const chosen = rows.filter(r => selectedRows.has(r.pubkey))
    if (chosen.length === 0) return
    setPruneAim({ rows: chosen, kinds: kindsFor(kind) })
  }

  const openAuthorProfile = (row: StorageAuthorStat) => {
    if (!row.npub) return
    setProfileTarget({ hex: row.pubkey, npub: row.npub })
  }

  const toggleBlacklist = async (row: StorageAuthorStat) => {
    if (!row.npub) return
    setBlacklistBusy(row.pubkey)
    try {
      if (row.blacklisted) {
        await adminApi.removeFromBlacklist(row.pubkey)
      } else {
        await adminApi.addToBlacklist(row.pubkey)
      }
      const nowBlocked = !row.blacklisted
      const patch = (rows: StorageAuthorStat[]) =>
        rows.map(r => (r.pubkey === row.pubkey ? { ...r, blacklisted: nowBlocked } : r))

      setStats(prev => (prev ? { ...prev, top_authors: patch(prev.top_authors) } : prev))
      setKindAuthors(prev => {
        const next: Record<number, StorageKindAuthors> = {}
        for (const [kind, entry] of Object.entries(prev)) {
          next[Number(kind)] = { ...entry, authors: patch(entry.authors) }
        }
        return next
      })
      setToast(nowBlocked ? 'Pubkey blocked.' : 'Pubkey unblocked.')
    } catch (e) {
      setError((e as Error).message)
    } finally {
      setBlacklistBusy(null)
    }
  }

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

  // Relay-wide table: fetch faces whenever the snapshot changes.
  useEffect(() => {
    if (stats?.top_authors?.length) loadAuthorProfiles(stats.top_authors)
  }, [stats?.top_authors])

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
      // Rethrown so the save bar can name this section in its failure list.
      setError(e instanceof Error ? e.message : 'Failed to save storage settings')
      throw e
    } finally {
      setSaving(false)
    }
  }

  // Compare the draft against what is configured on the server. The draft is
  // deliberately blank on a relay that never enabled pruning, so "blank and
  // disarmed" has to read as clean rather than as a pending change.
  const savedPolicyDays: Record<number, string> = {}
  for (const [kind, secs] of Object.entries(settings?.policies_secs ?? {})) {
    savedPolicyDays[Number(kind)] = String(Math.round(Number(secs) / 86400))
  }
  const savedArmed = settings?.configured_pruning_enabled ?? false
  const policiesChanged =
    JSON.stringify(Object.fromEntries(activePolicies)) !==
    JSON.stringify(
      Object.fromEntries(
        Object.entries(savedPolicyDays)
          .map(([k, v]) => [Number(k), Number(v)] as const)
          .filter(([, days]) => Number.isInteger(days) && days >= 1),
      ),
    )
  const intervalChanged =
    savedArmed && armed && intervalMinutes !== String(settings?.prune_interval_minutes ?? '')
  const storageDirty = armed !== savedArmed || policiesChanged || intervalChanged

  useDirtySection(
    {
      id: 'storage',
      label: 'Storage',
      dirty: storageDirty,
      // Keeps this section's own gates: an unconfirmed destructive change is
      // an unsaved change that cannot be committed, not a clean one.
      blocked: !storageDirty
        ? null
        : !formValid
          ? 'set an interval of at least 1 minute and at least one retention window'
          : !confirmed
            ? 'type DELETE to confirm arming automatic deletion'
            : null,
      consequence:
        storageDirty && armed
          ? `Arming deletion permanently removes events older than the window, across ${activePolicies.length} kind${activePolicies.length === 1 ? '' : 's'}. There is no undo and no backup.`
          : null,
    },
    {
      save,
      discard: () => {
        setArmed(savedArmed)
        setIntervalMinutes(savedArmed ? String(settings?.prune_interval_minutes ?? '') : '')
        setPolicyDays(savedArmed ? savedPolicyDays : {})
        setConfirmText('')
      },
    },
  )

  /**
   * Kinds after the filter. Matches both the numeric kind and the human label,
   * so "gift" finds 1059 and "1059" finds it too -- an operator hunting for a
   * kind knows one or the other, rarely both.
   */
  const visibleKinds = (() => {
    const q = kindFilter.trim().toLowerCase()
    const all = stats?.kinds ?? []
    return all.filter(k => {
      if (pickedKinds.size > 0 && !pickedKinds.has(k.kind)) return false
      if (!q) return true
      return String(k.kind).includes(q) || kindLabel(k.kind).toLowerCase().includes(q)
    })
  })()

  const togglePickedKind = (kind: number) =>
    setPickedKinds(prev => {
      const next = new Set(prev)
      if (!next.delete(kind)) next.add(kind)
      return next
    })

  // What the current filter actually covers. Without this the table shows a
  // subset and every total on the screen still describes the whole database,
  // so "how much of my disk is this kind" needs arithmetic by hand.
  const filteredTotals = visibleKinds.reduce(
    (acc, k) => ({ events: acc.events + k.count, bytes: acc.bytes + k.sampled_bytes }),
    { events: 0, bytes: 0 },
  )
  const kindsFiltered = pickedKinds.size > 0 || kindFilter.trim() !== ''

  return (
    <div>
      <p class="text-sm mb-6" style={{ color: 'var(--color-text-secondary)' }}>
        Automatic pruning is off unless explicitly armed below.
      </p>

      {toast && (
        <div class="mb-4 p-3 rounded-lg text-sm border" style={{ background: 'rgba(var(--color-accent-rgb), 0.08)', color: 'var(--color-accent)', borderColor: 'rgba(var(--color-accent-rgb), 0.2)' }}>
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
                class="admin-action-btn"
              >
                {counting ? 'Counting...' : 'Recount'}
              </button>
            </div>

            <div class="admin-storage-stats mt-4">
              <div
                class="admin-stat-card"
                title="The size of the LMDB file on disk. Deleting events frees pages for reuse inside this file but does not shrink it — only a compaction does."
              >
                <span>File on disk</span>
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
              {/* Two different deleters. Showing only the pruner's total under
                  a bare "Events deleted" meant an operator deleting by hand
                  watched a number that could not move. */}
              <div
                class="admin-stat-card"
                title="Events you deleted from this console since the relay last started. Resets on restart."
              >
                <span>Deleted by you</span>
                <strong>{formatNumber(settings.admin_deleted_total ?? 0)}</strong>
              </div>
              <div
                class="admin-stat-card"
                title="Events removed by the automatic retention sweep since the relay last started. Resets on restart."
              >
                <span>Deleted by auto-prune</span>
                <strong>{formatNumber(settings.total_pruned)}</strong>
              </div>
            </div>

            {/* Connections share the disk series' hourly tick, so they are a
                shape-of-the-day trend rather than a live gauge. Rendered only
                once enough samples carry the field -- older samples predate it
                and charting their absence as zero would invent an outage. */}
            {history.filter(h => h.connections != null).length >= 2 && (
              <div class="admin-storage-chart-block">
                <div class="admin-storage-chart-head">
                  <h4>Connections over time</h4>
                  <p>
                    Live WebSocket connections, sampled on the same hourly tick as the
                    file size. For the current figure see the Overview.
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
            )}

            <div class="admin-storage-chart-block">
              <div class="admin-storage-chart-head">
                <h4>Disk used over time</h4>
                <p style={{ color: '#fcd34d' }}>
                  Deleting events will not make this line go down. LMDB reuses
                  freed pages inside the file and never returns them to the
                  filesystem, so the file only ever grows. Pruning stops it
                  growing further; shrinking it needs a compaction, which is the
                  card below.
                </p>
                <p>
                  The file on disk, sampled hourly. Includes space freed by
                  deletion but not yet returned to the filesystem — LMDB reuses
                  it internally and never shrinks the file, so a flat event count
                  beside a rising line means the database wants compacting rather
                  than pruning.
                </p>
              </div>
              <TimeSeriesChart
                  points={history.map(h => ({ at: h.at, value: h.db_bytes }))}
                  format={formatBytes}
                  label="Database size"
                />
            </div>

            <CompactionCard />

            {statsError && (
              <div class="mt-4 p-3 rounded-lg text-sm bg-red-500/10 text-red-400 border border-red-500/20">
                {statsError}
              </div>
            )}

            {counting && !stats && <div class="lc-skeleton h-40 w-full mt-4" />}

            {stats && stats.kinds.length > 0 && (
              <div class="mt-4">
                <div class="admin-search-field mb-2">
                  <SearchIcon class="admin-search-icon" />
                  <input
                    class="admin-search-input"
                    type="text"
                    value={kindFilter}
                    onInput={e => setKindFilter((e.target as HTMLInputElement).value)}
                    placeholder="Filter by kind number or name — 1059, gift wrap, report…"
                    aria-label="Filter stored events by kind"
                  />
                </div>

                {/* Every kind this relay actually holds, heaviest first, as
                    toggles. The text box needs you to already know what you are
                    looking for; these show you what there is. */}
                <div class="admin-chip-row">
                  {[...stats.kinds]
                    .sort((a, b) => b.count - a.count)
                    .map(k => (
                      <button
                        key={k.kind}
                        type="button"
                        class={`admin-chip ${pickedKinds.has(k.kind) ? 'is-on' : ''}`}
                        aria-pressed={pickedKinds.has(k.kind)}
                        onClick={() => togglePickedKind(k.kind)}
                        title={`${kindLabel(k.kind)} · kind ${k.kind}`}
                      >
                        <span>{kindLabel(k.kind)}</span>
                        <span class="admin-chip-count">{formatNumber(k.count)}</span>
                      </button>
                    ))}
                  {pickedKinds.size > 0 && (
                    <button
                      type="button"
                      class="admin-chip"
                      onClick={() => setPickedKinds(new Set())}
                    >
                      Clear {pickedKinds.size} selected
                    </button>
                  )}
                </div>

                {kindsFiltered && visibleKinds.length > 0 && (
                  <p class="text-xs mb-2" style={{ color: 'var(--color-text-secondary)' }}>
                    {visibleKinds.length} of {stats.kinds.length} kinds ·{' '}
                    <strong style={{ color: 'var(--color-text-primary)' }}>
                      {formatNumber(filteredTotals.events)}
                    </strong>{' '}
                    events · {formatBytes(filteredTotals.bytes)} estimated ·{' '}
                    {stats.sampled_events > 0
                      ? `${((filteredTotals.events / stats.sampled_events) * 100).toFixed(1)}%`
                      : '0%'}{' '}
                    of what was {stats.sample_is_complete ? 'stored' : 'sampled'}
                  </p>
                )}

                {visibleKinds.length === 0 && (
                  <p class="text-sm" style={{ color: 'var(--color-text-secondary)' }}>
                    No kind matches{kindFilter ? ` “${kindFilter}”` : ' the selected kinds'}.
                  </p>
                )}
                <div class="overflow-x-auto admin-table-scroll">
                <table class="admin-table">
                  <thead>
                    <tr>
                      <th>Kind</th>
                      <th>Type</th>
                      <th class="is-numeric">Events</th>
                      <th class="is-numeric">Est. size</th>
                      <th class="is-numeric">Share</th>
                    </tr>
                  </thead>
                  <tbody>
                    {visibleKinds.map(k => {
                      const pct = stats.sampled_events > 0
                        ? (k.count / stats.sampled_events) * 100
                        : 0
                      const protectedKind = NEVER_PRUNE_KINDS.includes(k.kind)
                      const open = expandedKind === k.kind
                      const breakdown = kindAuthors[k.kind]
                      return (
                        <>
                          <tr
                            key={k.kind}
                            class="admin-kind-row"
                            style={{ borderTop: '1px solid var(--color-border)' }}
                            onClick={() => toggleKind(k.kind)}
                            role="button"
                            tabIndex={0}
                            aria-expanded={open}
                            title="Show which pubkeys this kind belongs to"
                            onKeyDown={e => {
                              if (e.key === 'Enter' || e.key === ' ') {
                                e.preventDefault()
                                toggleKind(k.kind)
                              }
                            }}
                          >
                            <td style={{ fontFamily: 'var(--font-mono, monospace)' }}>
                              <span class={`admin-disclosure ${open ? 'is-open' : ''}`} aria-hidden="true">
                                ▸
                              </span>
                              {k.kind}
                            </td>
                            <td>
                              {kindLabel(k.kind)}
                              {protectedKind && (
                                <span class="admin-status-badge ml-2" title="Never pruned">
                                  protected
                                </span>
                              )}
                            </td>
                            <td class="is-numeric">{formatNumber(k.count)}</td>
                            <td
                              class="is-numeric"
                              title={`${formatBytes(k.avg_bytes)} average per event across the sample. Content and tags only — index overhead is not counted, so these do not sum to the file on disk.`}
                            >
                              {stats.sample_is_complete ? '' : '≈'}{formatBytes(k.sampled_bytes)}
                            </td>
                            <td class="is-numeric" style={{ color: 'var(--color-text-secondary)' }}>
                              {pct < 0.1 && pct > 0 ? '<0.1' : pct.toFixed(1)}%
                            </td>
                          </tr>
                          {open && (
                            <tr key={`${k.kind}-authors`}>
                              <td colSpan={5} class="admin-kind-drilldown">
                                {kindAuthorsLoading === k.kind && (
                                  <div class="lc-skeleton h-24 w-full" />
                                )}
                                {kindAuthorsError && kindAuthorsLoading !== k.kind && (
                                  <p class="text-sm" style={{ color: '#fca5a5' }}>
                                    {kindAuthorsError}
                                  </p>
                                )}
                                {breakdown && (
                                  breakdown.authors.length > 0 ? (
                                    <>
                                      <p class="text-xs mb-2" style={{ color: 'var(--color-text-secondary)' }}>
                                        {ATTRIBUTION_HINT[breakdown.attributed_by]}
                                        {' '}Top {breakdown.authors.length} of kind {k.kind} in the sample.
                                      </p>
                                      <BulkBar
                                        rows={breakdown.authors}
                                        selected={selectedRows}
                                        onClear={() => setSelectedRows(new Set())}
                                        onDelete={() => openBulkPrune(breakdown.authors, k.kind)}
                                      />
                                      <AuthorTable
                                        rows={breakdown.authors}
                                        totalBytes={breakdown.kind_sampled_bytes}
                                        approximate={!stats.sample_is_complete}
                                        busyPubkey={blacklistBusy}
                                        profiles={authorProfiles}
                                        onOpenProfile={openAuthorProfile}
                                        onToggleBlacklist={toggleBlacklist}
                                        onPrune={row => openPrune(row, k.kind)}
                                        selected={selectedRows}
                                        onToggleSelect={toggleSelect}
                                        onToggleSelectAll={() => toggleSelectAll(breakdown.authors)}
                                      />
                                    </>
                                  ) : (
                                    <p class="text-sm" style={{ color: 'var(--color-text-secondary)' }}>
                                      No attributable pubkeys for this kind in the sample.
                                    </p>
                                  )
                                )}
                              </td>
                            </tr>
                          )}
                        </>
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
                    <table class="admin-table">
                      <thead>
                        <tr>
                          <th>Kind</th>
                          <th>Type</th>
                          <th class="is-numeric">Stored</th>
                          <th>Delete after (days)</th>
                          <th class="is-numeric">Would delete now</th>
                        </tr>
                      </thead>
                      <tbody>
                        {PRUNABLE_KINDS.map(k => {
                          const sampled = stats?.kinds.find(s => s.kind === k.kind)
                          const ex = exact[k.kind]
                          const deletedSoFar = settings.deleted_by_kind?.[k.kind]
                          return (
                            <tr key={k.kind} style={{ borderTop: '1px solid var(--color-border)' }}>
                              <td style={{ fontFamily: 'var(--font-mono, monospace)' }}>{k.kind}</td>
                              <td>
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
                              <td class="is-numeric">
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
                              <td>
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
                              <td class="is-numeric">
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

            {/* Committing moved to the shared save bar, which keeps this
                section's DELETE gate and reports it as a blocker. */}
            <div class="admin-settings-actions mt-4">
              <div class="text-sm" style={{ color: 'var(--color-text-secondary)' }}>
                Last run: {formatUnix(settings.last_run_unix)} · Runs: {settings.runs}
                {saving && ' · Saving…'}
              </div>
            </div>
          </section>

          {/* The kinds table says what is filling the disk. This says whose it
              is — the only way to aim the blacklist at whoever is responsible. */}
          {stats && stats.top_authors.length > 0 && (
            <section class="admin-settings-card">
              <div class="admin-settings-card-header">
                <div>
                  <h3>Events by pubkey</h3>
                  <p>
                    Who this relay is storing data for, heaviest first. Rows are
                    labelled Sender or Recipient because gift wraps can only be
                    attributed to the person receiving them.
                  </p>
                </div>
              </div>

              <div class="mt-4">
                <BulkBar
                  rows={stats.top_authors}
                  selected={selectedRows}
                  onClear={() => setSelectedRows(new Set())}
                  onDelete={() => openBulkPrune(stats.top_authors)}
                />
                <AuthorTable
                  rows={stats.top_authors}
                  totalBytes={stats.kinds.reduce((sum, k) => sum + k.sampled_bytes, 0)}
                  approximate={!stats.sample_is_complete}
                  busyPubkey={blacklistBusy}
                  profiles={authorProfiles}
                  onOpenProfile={openAuthorProfile}
                  onToggleBlacklist={toggleBlacklist}
                  onPrune={row => openPrune(row)}
                  selected={selectedRows}
                  onToggleSelect={toggleSelect}
                  onToggleSelectAll={() => toggleSelectAll(stats.top_authors)}
                />
              </div>

              <p class="text-xs mt-3" style={{ color: 'var(--color-text-secondary)' }}>
                {stats.sample_is_complete
                  ? 'Covers every stored event.'
                  : `Based on the ${formatNumber(stats.sampled_events)} most recent events (back to ${formatUnix(stats.oldest_sampled_unix)}), not the whole database. Shares are of the sample.`}
                {' '}Blocking a pubkey stops it connecting; it does not delete
                anything already stored.
              </p>
            </section>
          )}

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
                <table class="admin-table">
                  <thead>
                    <tr>
                      <th>Recipient</th>
                      <th class="is-numeric">Wraps</th>
                      <th class="is-numeric">Share</th>
                      <th />
                    </tr>
                  </thead>
                  <tbody>
                    {stats.top_recipients.map(r => {
                      const wraps = stats.kinds.find(k => k.kind === 1059)?.count ?? 0
                      const pct = wraps > 0 ? (r.count / wraps) * 100 : 0
                      return (
                        <tr key={r.pubkey} style={{ borderTop: '1px solid var(--color-border)' }}>
                          <td class="text-xs" style={{ fontFamily: 'var(--font-mono, monospace)' }} title={r.pubkey}>
                            {r.pubkey.slice(0, 16)}…
                          </td>
                          <td class="is-numeric">{formatNumber(r.count)}</td>
                          <td class="is-numeric" style={{ color: 'var(--color-text-secondary)' }}>
                            {pct.toFixed(1)}%
                          </td>
                          <td class="is-numeric">
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

      {pruneAim && (
        <PruneDialog
          aim={pruneAim}
          onClose={() => setPruneAim(null)}
          onDone={deleted => {
            setPruneAim(null)
            setSelectedRows(new Set())
            setToast(`Deleted ${formatNumber(deleted)} event${deleted === 1 ? '' : 's'}.`)
            // Everything on screen now describes events that no longer exist.
            // Drop the per-kind drilldown cache too -- it is keyed by kind and
            // would otherwise keep serving deleted rows until a page reload --
            // and force a rescan rather than waiting for the cache to expire.
            setKindAuthors({})
            const reopen = expandedKind
            setExpandedKind(null)
            loadStats(true)
            if (reopen !== null) {
              // Re-open the row the operator was looking at, against fresh data.
              setTimeout(() => toggleKind(reopen), 0)
            }
          }}
        />
      )}

      {profileTarget && (
        <ProfileCard
          hex={profileTarget.hex}
          npub={profileTarget.npub}
          profile={authorProfiles.get(profileTarget.hex)}
          onClose={() => setProfileTarget(null)}
        />
      )}
    </div>
  )
}
