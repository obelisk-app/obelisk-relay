import { useEffect, useState } from 'preact/hooks'
import { adminApi, type TierEntry, type TierPage } from '../../../services/AdminApiClient'
import { fetchProfiles, type NostrProfile } from '../../../services/ProfileFetcher'
import { AdminEmptyState } from '../AdminEmptyState'
import { AccountRow } from '../AccountRow'
import { SearchIcon } from '../SearchIcon'

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
}: TierPanelProps) => {
  const [page, setPage] = useState<TierPage | null>(null)
  const [offset, setOffset] = useState(0)
  const [query, setQuery] = useState('')
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [profiles, setProfiles] = useState<Map<string, NostrProfile>>(new Map())
  const [confirming, setConfirming] = useState<string | null>(null)
  const [busy, setBusy] = useState<string | null>(null)

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

      {page && page.entries.length > 0 && (
        <>
          <div class="admin-account-list mt-3">
            {page.entries.map(entry => (
              <AccountRow
                key={entry.hex}
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
