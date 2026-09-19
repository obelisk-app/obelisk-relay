import { useEffect, useState } from 'preact/hooks'
import { nip19 } from 'nostr-tools'
import { adminApi, type ReportCase } from '../../services/AdminApiClient'
import { fetchProfiles, type NostrProfile } from '../../services/ProfileFetcher'
import { AdminEmptyState } from './AdminEmptyState'
import { ProfileCard } from './ProfileCard'

type StatusFilter = 'open' | 'resolved' | 'all'

const shortHex = (hex: string) => `${hex.slice(0, 8)}…${hex.slice(-4)}`

/** Hex -> npub, tolerating anything that is not a valid key. */
const toNpub = (hex: string) => {
  try {
    return nip19.npubEncode(hex)
  } catch {
    return ''
  }
}

const displayName = (hex: string, profiles: Map<string, NostrProfile>) => {
  const p = profiles.get(hex)
  return p?.display_name || p?.name || p?.nip05 || null
}

const when = (unix: number) => {
  const seconds = Math.floor(Date.now() / 1000) - unix
  if (seconds < 60) return 'just now'
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`
  if (seconds < 86400) return `${Math.floor(seconds / 3600)}h ago`
  return `${Math.floor(seconds / 86400)}d ago`
}

/**
 * The moderation queue for NIP-56 reports.
 *
 * Two decisions shape this screen:
 *
 * One row per *target*, not per report. Ten people reporting one message is one
 * decision; a queue that listed them separately would make a brigade look like
 * ten problems and would take ten clicks to clear.
 *
 * The reported content is shown inline. An admin cannot tell abuse from a
 * grudge without reading what was actually said, and a queue that makes you go
 * and look it up by event id is a queue that does not get worked.
 *
 * Actions are offered only where they mean something: there is nothing to
 * delete when a report names a person rather than a message, and nothing to
 * remove someone from when the reported event is not in a group.
 */
/**
 * A pubkey you can actually act on: a name when we have one, the npub when we
 * don't, and a click to see the rest.
 *
 * The queue previously printed eight hex characters with no affordance. That is
 * not enough to tell a person apart, and there was nothing to click.
 */
const IdentityChip = ({
  hex,
  label,
  profiles,
  onInspect,
}: {
  hex: string
  /** Stated explicitly, because a report always involves two parties. */
  label: string
  profiles: Map<string, NostrProfile>
  onInspect: (v: { hex: string; npub: string }) => void
}) => {
  const npub = toNpub(hex)
  const name = displayName(hex, profiles)
  return (
    <span class="inline-flex items-center gap-1.5 flex-wrap">
      <span class="text-[var(--color-text-tertiary)]">{label}</span>
      <button
        onClick={() => onInspect({ hex, npub })}
        class="inline-flex items-center gap-1 px-1.5 py-0.5 rounded text-xs
               border border-[var(--color-border)] hover:border-[var(--color-border-hover)]
               hover:text-[var(--color-text-primary)] transition-colors"
        title={npub || hex}
      >
        {profiles.get(hex)?.picture && (
          <img
            src={profiles.get(hex)!.picture}
            alt=""
            referrerpolicy="no-referrer"
            class="w-4 h-4 rounded-full object-cover"
          />
        )}
        <span>{name ?? (npub ? `${npub.slice(0, 12)}…` : shortHex(hex))}</span>
      </button>
    </span>
  )
}

export const ReportsManager = () => {
  const [cases, setCases] = useState<ReportCase[]>([])
  const [resolvedTotal, setResolvedTotal] = useState(0)
  const [status, setStatus] = useState<StatusFilter>('open')
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [busyKey, setBusyKey] = useState<string | null>(null)
  const [confirming, setConfirming] = useState<{ key: string; action: string } | null>(null)
  const [notes, setNotes] = useState<Record<string, string>>({})
  const [profiles, setProfiles] = useState<Map<string, NostrProfile>>(new Map())
  /** Pubkey whose profile card is open. */
  const [inspecting, setInspecting] = useState<{ hex: string; npub: string } | null>(null)

  /**
   * Look up everyone involved -- reporters and reported alike.
   *
   * A truncated hex is not an identity you can act on. Deciding whether a report
   * is a real problem or a grudge usually means knowing who is on each side, and
   * the console was showing eight characters and no way to see more.
   */
  const loadProfiles = (list: ReportCase[]) => {
    const hexes = new Set<string>()
    for (const c of list) {
      if (c.reported_pubkey) hexes.add(c.reported_pubkey)
      if (c.target.kind === 'pubkey') hexes.add(c.target.hex)
      for (const r of c.reports) hexes.add(r.reporter)
    }
    if (hexes.size === 0) return
    fetchProfiles([...hexes])
      .then(found => setProfiles(prev => new Map([...prev, ...found])))
      // Decoration: a slow profile relay must not blank the queue.
      .catch(() => undefined)
  }

  const load = async (next: StatusFilter) => {
    setLoading(true)
    setError(null)
    try {
      const data = await adminApi.getReports(next)
      setCases(data.cases)
      setResolvedTotal(data.resolved_total)
      loadProfiles(data.cases)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to load reports')
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    let cancelled = false
    const run = async () => {
      setLoading(true)
      setError(null)
      try {
        const data = await adminApi.getReports(status)
        // The tab may have changed while this was in flight.
        if (cancelled) return
        setCases(data.cases)
        setResolvedTotal(data.resolved_total)
        loadProfiles(data.cases)
      } catch (e) {
        if (cancelled) return
        setError(e instanceof Error ? e.message : 'Failed to load reports')
      } finally {
        if (!cancelled) setLoading(false)
      }
    }
    void run()
    return () => {
      cancelled = true
    }
  }, [status])

  const resolve = async (c: ReportCase, action: 'dismiss' | 'delete_event' | 'remove_from_group' | 'blacklist') => {
    setBusyKey(c.key)
    setError(null)
    try {
      await adminApi.resolveReport({
        key: c.key,
        action,
        note: notes[c.key] ?? '',
        group_id: c.group_id ?? undefined,
      })
      setConfirming(null)
      await load(status)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to resolve report')
    } finally {
      setBusyKey(null)
    }
  }

  return (
    <div>
      <div class="admin-settings-card-header">
        <div>
          <h3>Reports</h3>
          <p>
            Moderation reports filed by users. One row per reported thing, however many
            people reported it. Only relay admins can read these.
          </p>
        </div>
        <div class="admin-row-actions">
          {(['open', 'resolved', 'all'] as StatusFilter[]).map(s => (
            <button
              key={s}
              onClick={() => setStatus(s)}
              class={`admin-status-badge ${status === s ? 'admin-status-badge-ok' : ''}`}
              aria-pressed={status === s}
            >
              {s === 'open' ? 'Needs review' : s === 'resolved' ? 'Handled' : 'All'}
            </button>
          ))}
        </div>
      </div>

      {error && (
        <p class="admin-access-hint" role="alert">{error}</p>
      )}

      {inspecting && (
        <ProfileCard
          profile={profiles.get(inspecting.hex)}
          hex={inspecting.hex}
          npub={inspecting.npub}
          onClose={() => setInspecting(null)}
        />
      )}

      {loading ? (
        <p class="admin-access-hint">Loading reports…</p>
      ) : cases.length === 0 ? (
        <AdminEmptyState
          headline={status === 'open' ? 'Nothing to review' : 'Nothing here'}
          stat={resolvedTotal > 0 && status === 'open' ? `${resolvedTotal} handled` : undefined}
        >
          {status === 'open'
            ? 'No reports are waiting for a decision.'
            : 'No reports match this filter.'}
        </AdminEmptyState>
      ) : (
        <div class="space-y-3 mt-4">
          {cases.map(c => {
            const isEvent = c.target.kind === 'event'
            const busy = busyKey === c.key
            // The account the actions apply to: the reported event's author, or
            // the pubkey itself when the report names a person.
            const subjectHex =
              c.target.kind === 'pubkey' ? c.target.hex : c.reported_pubkey
            const reporters = [...new Set(c.reports.map(r => r.reporter))]
            return (
              <section
                key={c.key}
                class="p-4 rounded-lg border border-[var(--color-border)] bg-[var(--color-bg-primary)]"
              >
                <div class="flex items-start justify-between gap-3 flex-wrap">
                  <div class="min-w-0">
                    <div class="flex items-center gap-2 flex-wrap">
                      <strong class="text-sm">
                        {isEvent ? 'Message' : 'Account'}
                      </strong>
                      {c.types.map(t => (
                        <span key={t} class="admin-status-badge admin-status-badge-warn">{t}</span>
                      ))}
                      <span class="text-xs text-[var(--color-text-tertiary)]">
                        {c.reporter_count} {c.reporter_count === 1 ? 'reporter' : 'reporters'}
                        {c.reports.length !== c.reporter_count && ` · ${c.reports.length} reports`}
                        {' · '}{when(c.last_reported_at)}
                      </span>
                    </div>
                    {/* Who is who, stated rather than implied. Every action
                        below acts on the reported account, never the reporter,
                        and that has to be legible before anyone clicks. */}
                    <div class="text-xs text-[var(--color-text-secondary)] mt-2 space-y-1">
                      {subjectHex ? (
                        <div>
                          <IdentityChip
                            hex={subjectHex}
                            label="Reported:"
                            profiles={profiles}
                            onInspect={setInspecting}
                          />
                        </div>
                      ) : c.claimed_pubkey ? (
                        // Shown as the accusation it is. The relay could not
                        // confirm it -- the message is gone -- so nothing here
                        // acts on this key.
                        <div class="flex items-center gap-1.5 flex-wrap">
                          <span class="text-[var(--color-text-tertiary)]">
                            Reporter claims it was:
                          </span>
                          <IdentityChip
                            hex={c.claimed_pubkey}
                            label=""
                            profiles={profiles}
                            onInspect={setInspecting}
                          />
                          <span class="text-yellow-300/80">unverified</span>
                        </div>
                      ) : null}
                      <div class="flex items-center gap-1.5 flex-wrap">
                        <span class="text-[var(--color-text-tertiary)]">Reported by:</span>
                        {reporters.map(r => (
                          <IdentityChip
                            key={r}
                            hex={r}
                            label=""
                            profiles={profiles}
                            onInspect={setInspecting}
                          />
                        ))}
                      </div>
                      <div class="text-[var(--color-text-tertiary)] break-all">
                        {isEvent && c.target.kind === 'event' ? `message ${shortHex(c.target.id)}` : null}
                        {c.group_id && ` · in group ${c.group_id}`}
                      </div>
                    </div>
                  </div>
                  {c.resolution && (
                    <span class="admin-status-badge admin-status-badge-ok">
                      {c.resolution.action.replace(/_/g, ' ')}
                    </span>
                  )}
                </div>

                {/* What was reported. Rendered as a text child, so a hostile
                    message cannot inject markup into the console. */}
                {c.reported_content !== null && (
                  <blockquote class="mt-3 p-3 rounded bg-[var(--color-bg-secondary)] text-sm whitespace-pre-wrap break-words">
                    {c.reported_content || <em class="text-[var(--color-text-tertiary)]">(no text content)</em>}
                  </blockquote>
                )}
                {isEvent && c.reported_content === null && (
                  <p class="admin-access-hint mt-2">
                    The reported message is no longer stored, so the relay cannot confirm who
                    wrote it. Blocking is unavailable for that reason — only the reporter's
                    unverified claim remains, and a ban should not rest on that.
                  </p>
                )}

                {/* Why they reported it. */}
                <ul class="mt-3 space-y-1">
                  {c.reports.slice(0, 5).map(r => (
                    <li key={r.report_id} class="text-xs text-[var(--color-text-secondary)]">
                      <span class="text-[var(--color-text-tertiary)]">{shortHex(r.reporter)}</span>
                      {' · '}{r.report_type}
                      {r.content && <>{' · '}<span class="break-words">{r.content}</span></>}
                    </li>
                  ))}
                  {c.reports.length > 5 && (
                    <li class="text-xs text-[var(--color-text-tertiary)]">
                      and {c.reports.length - 5} more
                    </li>
                  )}
                </ul>

                {!c.resolution && (
                  <div class="mt-3">
                    <label class="block">
                      <span class="sr-only">Note for the moderation log</span>
                      <input
                        type="text"
                        value={notes[c.key] ?? ''}
                        onInput={e =>
                          setNotes(prev => ({
                            ...prev,
                            [c.key]: (e.target as HTMLInputElement).value,
                          }))
                        }
                        placeholder="Optional note — recorded with your decision"
                        class="w-full text-sm"
                      />
                    </label>

                    <div class="flex items-center gap-2 mt-2 flex-wrap">
                      <button
                        onClick={() => void resolve(c, 'dismiss')}
                        disabled={busy}
                        class="px-3 py-1.5 rounded text-xs font-medium border border-[var(--color-border)]
                               hover:border-[var(--color-border-hover)] disabled:opacity-50"
                      >
                        Not a problem
                      </button>

                      {/* Nothing to delete when the report names a person. */}
                      {isEvent && c.reported_content !== null && (
                        <button
                          onClick={() => void resolve(c, 'delete_event')}
                          disabled={busy}
                          class="px-3 py-1.5 rounded text-xs font-medium border border-[var(--color-border)]
                                 hover:border-[var(--color-border-hover)] disabled:opacity-50"
                        >
                          Delete message
                        </button>
                      )}

                      {/* Only meaningful when we know which group. */}
                      {c.group_id && (
                        <button
                          onClick={() => void resolve(c, 'remove_from_group')}
                          disabled={busy}
                          class="px-3 py-1.5 rounded text-xs font-medium border border-[var(--color-border)]
                                 hover:border-[var(--color-border-hover)] disabled:opacity-50"
                        >
                          Remove reported account from group
                        </button>
                      )}

                      {/* The heaviest action, so it asks twice. */}
                      {confirming?.key === c.key && confirming.action === 'blacklist' ? (
                        <>
                          <span class="text-xs text-yellow-300">
                            Block{' '}
                            <strong>
                              {subjectHex
                                ? (displayName(subjectHex, profiles) ?? shortHex(subjectHex))
                                : 'this account'}
                            </strong>{' '}
                            from the whole relay? This is the reported account, not the reporter.
                          </span>
                          <button
                            onClick={() => void resolve(c, 'blacklist')}
                            disabled={busy}
                            class="px-3 py-1.5 rounded text-xs font-medium bg-red-600 hover:bg-red-700 disabled:opacity-50"
                          >
                            {busy ? 'Blocking…' : 'Confirm block'}
                          </button>
                          <button
                            onClick={() => setConfirming(null)}
                            class="px-3 py-1.5 rounded text-xs text-[var(--color-text-secondary)]"
                          >
                            Cancel
                          </button>
                        </>
                      ) : (
                        <button
                          onClick={() => setConfirming({ key: c.key, action: 'blacklist' })}
                          // Without a subject there is nobody to block: the
                          // reported message is gone, so its author is unknown.
                          disabled={busy || !subjectHex}
                          title={
                            subjectHex
                              ? `Blocks ${toNpub(subjectHex) || subjectHex}`
                              : 'The reported message is gone, so its author cannot be determined'
                          }
                          class="px-3 py-1.5 rounded text-xs font-medium text-red-400
                                 border border-red-500/30 hover:border-red-500/60
                                 disabled:opacity-40 disabled:cursor-not-allowed"
                        >
                          Block reported account
                        </button>
                      )}
                    </div>
                  </div>
                )}

                {c.resolution && (
                  <p class="admin-access-hint mt-2">
                    {c.resolution.action.replace(/_/g, ' ')} by {shortHex(c.resolution.resolved_by)}
                    {' · '}{when(c.resolution.resolved_at)}
                    {c.resolution.note && ` · ${c.resolution.note}`}
                  </p>
                )}
              </section>
            )
          })}
        </div>
      )}
    </div>
  )
}
