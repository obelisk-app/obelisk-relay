import { useEffect, useState } from 'preact/hooks'
import { adminApi, type TierEntry, type TierPage } from '../../../services/AdminApiClient'
import { fetchProfiles, type NostrProfile } from '../../../services/ProfileFetcher'
import { AdminEmptyState } from '../AdminEmptyState'
import { AccountRow } from '../AccountRow'
import { SearchIcon } from '../SearchIcon'
import { useRowSelection } from '../useRowSelection'

const PAGE = 100

/** Why an account is in Tier 1 — and what happens if you remove it. */
const SOURCE_LABEL: Record<TierEntry['source'], string> = {
  manual: 'added by hand',
  follow_sync: 'from follow sync',
  reference: 'reference account',
  follows_reference: 'followed by a reference account',
  web_of_trust: 'follow graph',
}

interface TierPanelProps {
  tier: number
  title: string
  blurb: string
  onInspect: (v: { hex: string; npub: string }) => void
  /** Rendered in the header — the tier's own controls. */
  controls?: preact.ComponentChildren
  /** Offered per row when the tier supports removal. */
  onRemove?: (entry: TierEntry) => Promise<void>
  removeLabel?: string
  /**
   * Actions offered over a multi-row selection. Each runs once per selected
   * entry; the panel reloads the window and clears the selection afterwards.
   */
  bulkActions?: {
    label: string
    /** Present-tense progress label, e.g. "Blocking". */
    busyLabel: string
    run: (entry: TierEntry) => Promise<void>
    /** Spelled out before anything happens, because these hit many accounts. */
    describe: (count: number) => string
    danger?: boolean
  }[]
  /** Called once a bulk run finishes, so the tier counts above can refresh. */
  onBulkComplete?: () => void
}

/**
 * One access tier: how many, who, and what you can do about it.
 *
 * Paged rather than rendered whole. Tier 1 is around a thousand accounts on
 * this relay and the web-of-trust tiers are six figures, so "show the list"
 * has to mean a window over it. The search box filters server-side across the
 * whole tier, not just the loaded page — otherwise looking someone up would
 * only ever consider the first hundred rows.
 */
export const TierPanel = ({
  tier,
  title,
  blurb,
  onInspect,
  controls,
  onRemove,
  removeLabel = 'Remove',
  bulkActions,
  onBulkComplete,
}: TierPanelProps) => {
  const [page, setPage] = useState<TierPage | null>(null)
  const [offset, setOffset] = useState(0)
  const [query, setQuery] = useState('')
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [profiles, setProfiles] = useState<Map<string, NostrProfile>>(new Map())
  const [confirming, setConfirming] = useState<string | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  // Which bulk action is awaiting confirmation, and how far through it we are.
  const [pendingBulk, setPendingBulk] = useState<string | null>(null)
  const [bulkProgress, setBulkProgress] = useState<{ done: number; total: number } | null>(null)

  const rowIds = page?.entries.map(e => e.hex) ?? []
  const { selected, toggle, selectAll, clear } = useRowSelection(rowIds)

  useEffect(() => {
    let cancelled = false
    // Debounced: every keystroke would otherwise be a request, and the search
    // is server-side because the tier is larger than the page.
    const timer = setTimeout(async () => {
      setLoading(true)
      setError(null)
      try {
        const data = await adminApi.getAccessTier(tier, { limit: PAGE, offset, q: query })
        if (cancelled) return
        setPage(data)
        fetchProfiles(data.entries.map(e => e.hex))
          .then(found => !cancelled && setProfiles(prev => new Map([...prev, ...found])))
          // Decoration: a slow profile relay must not blank the list.
          .catch(() => undefined)
      } catch (e) {
        if (!cancelled) setError(e instanceof Error ? e.message : 'Failed to load this tier')
      } finally {
        if (!cancelled) setLoading(false)
      }
    }, query ? 250 : 0)

    return () => {
      cancelled = true
      clearTimeout(timer)
    }
  }, [tier, offset, query])

  const remove = async (entry: TierEntry) => {
    if (!onRemove) return
    setBusy(entry.hex)
    setError(null)
    try {
      await onRemove(entry)
      setConfirming(null)
      // Reload the same window rather than resetting to the top, so removing
      // the fifth of six hundred does not send you back to the first page.
      const data = await adminApi.getAccessTier(tier, { limit: PAGE, offset, q: query })
      setPage(data)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to remove')
    } finally {
      setBusy(null)
    }
  }

  const runBulk = async (action: NonNullable<TierPanelProps['bulkActions']>[number]) => {
    const targets = (page?.entries ?? []).filter(e => selected.has(e.hex))
    if (targets.length === 0) return
    setError(null)
    setBulkProgress({ done: 0, total: targets.length })
    // Sequential, not Promise.all: these are writes to one JSON-backed list on
    // the relay, and a hundred concurrent mutations of the same file is how you
    // get a partially-applied list. Slower, and the progress count is honest.
    const failed: string[] = []
    for (const [i, entry] of targets.entries()) {
      try {
        await action.run(entry)
      } catch {
        failed.push(entry.npub || entry.hex)
      }
      setBulkProgress({ done: i + 1, total: targets.length })
    }
    setBulkProgress(null)
    setPendingBulk(null)
    clear()
    if (failed.length > 0) {
      setError(
        `${action.label} failed for ${failed.length} of ${targets.length}: ${failed
          .slice(0, 3)
          .join(', ')}${failed.length > 3 ? '…' : ''}`,
      )
    }
    try {
      setPage(await adminApi.getAccessTier(tier, { limit: PAGE, offset, q: query }))
    } catch {
      // The actions themselves succeeded; a failed refresh is cosmetic.
    }
    onBulkComplete?.()
  }

  const total = page?.total ?? 0
  const shown = page?.entries.length ?? 0
  const hasPrev = offset > 0
  const hasNext = offset + shown < total

  return (
    <section class="admin-settings-card">
      <div class="admin-settings-card-header">
        <div>
          <h3>{title}</h3>
          <p>{blurb}</p>
        </div>
        <div class="admin-row-actions">
          {page?.truncated && (
            <span
              class="admin-status-badge admin-status-badge-warn"
              title={
                'The follow graph ran out of fetch budget at this depth, so this is a floor '
                + 'rather than a total: there are at least this many, and a refusal out here '
                + 'means "no path was fetched", not "no path exists".'
              }
            >
              at least {total.toLocaleString()}
            </span>
          )}
          {!page?.truncated && (
            <span class="admin-status-badge">{total.toLocaleString()}</span>
          )}
          {controls}
        </div>
      </div>

      {error && (
        <p class="admin-access-hint" role="alert">{error}</p>
      )}

      <div class="admin-search-field mt-3">
        <SearchIcon class="admin-search-icon" />
        <input
          class="admin-search-input"
          type="text"
          value={query}
          onInput={e => {
            setOffset(0)
            setQuery((e.target as HTMLInputElement).value)
          }}
          placeholder="Search this tier by npub or hex…"
          aria-label={`Search ${title}`}
        />
      </div>

      {loading && !page && <div class="lc-skeleton h-32 w-full mt-3" />}

      {page && page.entries.length === 0 && (
        <div class="mt-3">
          <AdminEmptyState headline={query ? 'No match' : 'Nobody here yet'}>
            {query
              ? `Nothing in ${title} matches “${query}”.`
              : `No accounts are admitted through ${title}.`}
          </AdminEmptyState>
        </div>
      )}

      {page && page.entries.length > 0 && bulkActions && bulkActions.length > 0 && (
        <div class={`admin-bulk-bar mt-3 ${selected.size === 0 ? 'is-idle' : ''}`}>
          <label class="admin-bulk-select-all">
            <input
              type="checkbox"
              checked={selected.size > 0 && selected.size === rowIds.length}
              // Indeterminate is the honest state for a partial selection, and
              // without it "select all" reads as "nothing selected" whenever a
              // few rows are ticked.
              ref={el => {
                if (el) el.indeterminate = selected.size > 0 && selected.size < rowIds.length
              }}
              onChange={() => (selected.size === rowIds.length ? clear() : selectAll())}
              aria-label={`Select all ${shown} accounts on this page`}
            />
            <span>
              {selected.size > 0
                ? `${selected.size} selected`
                : `Select accounts on this page`}
            </span>
          </label>

          {selected.size > 0 && (
            <div class="admin-row-actions">
              {bulkProgress ? (
                <span class="admin-bulk-progress">
                  {bulkProgress.done} of {bulkProgress.total}…
                </span>
              ) : pendingBulk ? (
                <>
                  <span class="admin-bulk-warning">
                    {bulkActions.find(a => a.label === pendingBulk)?.describe(selected.size)}
                  </span>
                  <button
                    class="admin-action-btn"
                    onClick={() => {
                      const action = bulkActions.find(a => a.label === pendingBulk)
                      if (action) void runBulk(action)
                    }}
                  >
                    Confirm
                  </button>
                  <button
                    class="admin-action-btn admin-action-btn-secondary"
                    onClick={() => setPendingBulk(null)}
                  >
                    Cancel
                  </button>
                </>
              ) : (
                <>
                  {bulkActions.map(action => (
                    <button
                      key={action.label}
                      class={`admin-action-btn ${action.danger ? '' : 'admin-action-btn-secondary'}`}
                      onClick={() => setPendingBulk(action.label)}
                    >
                      {action.label}
                    </button>
                  ))}
                  <button class="admin-action-btn admin-action-btn-secondary" onClick={clear}>
                    Clear
                  </button>
                </>
              )}
            </div>
          )}
        </div>
      )}

      {page && page.entries.length > 0 && (
        <>
          <div class="admin-account-list mt-3">
            {page.entries.map((entry, index) => (
              <AccountRow
                key={entry.hex}
                selection={
                  bulkActions && bulkActions.length > 0
                    ? {
                        checked: selected.has(entry.hex),
                        onToggle: shiftKey => toggle(entry.hex, index, shiftKey),
                        label: `Select ${entry.npub || entry.hex}`,
                      }
                    : undefined
                }
                hex={entry.hex}
                npub={entry.npub}
                profile={profiles.get(entry.hex)}
                onInspect={() => onInspect({ hex: entry.hex, npub: entry.npub })}
                subtitle={
                  <>
                    {SOURCE_LABEL[entry.source]}
                    {entry.hops != null && ` · ${entry.hops} hop${entry.hops === 1 ? '' : 's'}`}
                  </>
                }
                actions={
                  onRemove && (
                    confirming === entry.hex ? (
                      <>
                        <button
                          class="admin-action-btn admin-action-btn-secondary"
                          disabled={busy === entry.hex}
                          onClick={() => void remove(entry)}
                        >
                          {busy === entry.hex ? 'Removing…' : 'Confirm'}
                        </button>
                        <button
                          class="admin-action-btn admin-action-btn-secondary"
                          onClick={() => setConfirming(null)}
                        >
                          Cancel
                        </button>
                      </>
                    ) : (
                      <button
                        class="admin-action-btn admin-action-btn-secondary"
                        onClick={() => setConfirming(entry.hex)}
                        // Removal means different things per source; say which.
                        title={
                          entry.source === 'follow_sync' || entry.source === 'follows_reference'
                            ? 'This account is here because a reference account follows it. '
                              + 'Removing it will not stick — the next sync brings it back. '
                              + 'Block it, or remove the reference account.'
                            : removeLabel
                        }
                      >
                        {removeLabel}
                      </button>
                    )
                  )
                }
              />
            ))}
          </div>

          <div class="admin-pager mt-2">
            <span>
              {offset + 1}–{offset + shown} of {page.truncated ? 'at least ' : ''}
              {total.toLocaleString()}
            </span>
            <span class="admin-pager-buttons">
              <button
                class="admin-action-btn admin-action-btn-secondary"
                disabled={!hasPrev || loading}
                onClick={() => setOffset(Math.max(0, offset - PAGE))}
              >
                Previous
              </button>
              <button
                class="admin-action-btn admin-action-btn-secondary"
                disabled={!hasNext || loading}
                onClick={() => setOffset(offset + PAGE)}
              >
                Next
              </button>
            </span>
          </div>
        </>
      )}
    </section>
  )
}
