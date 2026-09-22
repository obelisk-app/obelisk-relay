import { useEffect, useState } from 'preact/hooks'
import { adminApi } from '../../../services/AdminApiClient'
import { fetchProfiles, type NostrProfile } from '../../../services/ProfileFetcher'
import { resolvePubkeyInput } from '../../../services/nip05'
import { AdminEmptyState } from '../AdminEmptyState'
import { AccountRow } from '../AccountRow'
import { SearchIcon } from '../SearchIcon'

interface BlockedPanelProps {
  onInspect: (v: { hex: string; npub: string }) => void
  onChanged: () => void
}

/**
 * Accounts refused regardless of tier.
 *
 * A first-class list rather than the collapsed disclosure it used to be at the
 * bottom of the Access screen. The blacklist is the only control that overrides
 * every admission path, which makes it the most consequential list in the
 * console and the one that was hardest to find.
 *
 * The add field resolves npub, hex and NIP-05 alike. The old one took the raw
 * string and passed it straight through, so blocking by npub worked only
 * because the backend happened to decode it, and NIP-05 did not work at all —
 * while the allowlist field two columns away accepted all three.
 */
export const BlockedPanel = ({ onInspect, onChanged }: BlockedPanelProps) => {
  const [entries, setEntries] = useState<{ hex: string; npub: string }[]>([])
  const [profiles, setProfiles] = useState<Map<string, NostrProfile>>(new Map())
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [query, setQuery] = useState('')
  const [input, setInput] = useState('')
  const [busy, setBusy] = useState<string | null>(null)
  const [confirming, setConfirming] = useState<string | null>(null)

  const load = async () => {
    setLoading(true)
    setError(null)
    try {
      const list = await adminApi.getBlacklist()
      setEntries(list)
      fetchProfiles(list.map(e => e.hex))
        .then(found => setProfiles(prev => new Map([...prev, ...found])))
        .catch(() => undefined)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to load the blocked list')
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    void load()
  }, [])

  const block = async (e: Event) => {
    e.preventDefault()
    const raw = input.trim()
    if (!raw) return
    setBusy('add')
    setError(null)
    try {
      const pubkey = await resolvePubkeyInput(raw)
      await adminApi.addToBlacklist(pubkey)
      setInput('')
      await load()
      onChanged()
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Could not block that account')
    } finally {
      setBusy(null)
    }
  }

  const unblock = async (hex: string) => {
    setBusy(hex)
    setError(null)
    try {
      await adminApi.removeFromBlacklist(hex)
      setConfirming(null)
      await load()
      onChanged()
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Could not unblock that account')
    } finally {
      setBusy(null)
    }
  }

  const q = query.trim().toLowerCase()
  const visible = q
    ? entries.filter(e => {
        const p = profiles.get(e.hex)
        return (
          e.hex.toLowerCase().includes(q) ||
          e.npub.toLowerCase().includes(q) ||
          (p?.display_name ?? '').toLowerCase().includes(q) ||
          (p?.name ?? '').toLowerCase().includes(q) ||
          (p?.nip05 ?? '').toLowerCase().includes(q)
        )
      })
    : entries

  return (
    <section class="admin-settings-card">
      <div class="admin-settings-card-header">
        <div>
          <h3>Blocked</h3>
          <p>
            Refused however they would otherwise qualify. This overrides every tier,
            including accounts you added by hand.
          </p>
        </div>
        <span class="admin-status-badge admin-status-badge-danger">{entries.length}</span>
      </div>

      {error && (
        <p class="admin-access-hint" role="alert">{error}</p>
      )}

      <form onSubmit={block} class="admin-search-row mt-3">
        <div class="admin-search-field">
          <input
            class="admin-search-input"
            type="text"
            value={input}
            onInput={e => setInput((e.target as HTMLInputElement).value)}
            placeholder="npub1…, hex, or name@domain.com"
            aria-label="Account to block"
          />
        </div>
        <button class="admin-action-btn" type="submit" disabled={busy !== null || !input.trim()}>
          {busy === 'add' ? 'Blocking…' : 'Block'}
        </button>
      </form>

      {loading && entries.length === 0 && <div class="lc-skeleton h-24 w-full mt-3" />}

      {!loading && entries.length === 0 && (
        <div class="mt-3">
          <AdminEmptyState headline="Nobody is blocked">
            Blocking an account refuses it everywhere, whichever tier would otherwise
            let it in.
          </AdminEmptyState>
        </div>
      )}

      {entries.length > 0 && (
        <>
          {entries.length > 8 && (
            <div class="admin-search-field mt-3">
              <SearchIcon class="admin-search-icon" />
              <input
                class="admin-search-input"
                type="text"
                value={query}
                onInput={e => setQuery((e.target as HTMLInputElement).value)}
                placeholder="Search blocked accounts…"
                aria-label="Search blocked accounts"
              />
            </div>
          )}

          <div class="admin-account-list mt-3">
            {visible.map(entry => (
              <AccountRow
                key={entry.hex}
                hex={entry.hex}
                npub={entry.npub}
                profile={profiles.get(entry.hex)}
                onInspect={() => onInspect(entry)}
                subtitle="blocked"
                actions={
                  confirming === entry.hex ? (
                    <>
                      <button
                        class="admin-action-btn admin-action-btn-secondary"
                        disabled={busy === entry.hex}
                        onClick={() => void unblock(entry.hex)}
                      >
                        {busy === entry.hex ? 'Unblocking…' : 'Confirm'}
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
                      title="Unblocking lets this account back in if any tier admits it."
                    >
                      Unblock
                    </button>
                  )
                }
              />
            ))}
          </div>

          {visible.length === 0 && (
            <p class="admin-access-hint">No blocked account matches “{query}”.</p>
          )}
        </>
      )}
    </section>
  )
}
