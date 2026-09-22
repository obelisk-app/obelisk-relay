import { useState } from 'preact/hooks'
import { adminApi, type AccessCheck } from '../../../services/AdminApiClient'
import { fetchProfiles, getDisplayName, type NostrProfile } from '../../../services/ProfileFetcher'
import { resolvePubkeyInput } from '../../../services/nip05'
import { SearchIcon } from '../SearchIcon'

/** Which tier a backend verdict belongs to, in the console's language. */
const TIER_OF: Record<string, string> = {
  manual: 'Tier 1 — added by hand',
  follow_sync: 'Tier 1 — followed by a reference account',
  web_of_trust: 'Web of trust',
  open_relay: 'No restriction',
  blacklist: 'Blocked',
  none: 'Not admitted',
}

interface AccessSearchProps {
  onInspect: (v: { hex: string; npub: string }) => void
  /** Called after an action that changes admission, so counts can refresh. */
  onChanged: () => void
}

/**
 * Is this person allowed, and why.
 *
 * The one question the Access screens never answered. A "check a pubkey" card
 * already existed and already asked the right backend — `/whitelist/check`
 * runs the real admission ladder against the graph rather than the resolution
 * cache, so it answers "would this key get in" and not merely "has it" — but
 * the UI threw most of the answer away. It fetched the tier and never rendered
 * it, and discarded the hex and npub, so there was no avatar, no name, no
 * profile link, and no way to act on what you had just found out.
 *
 * Accepts an npub, a hex key, or a NIP-05 address, because an operator arrives
 * with whichever one the complaint mentioned.
 */
export const AccessSearch = ({ onInspect, onChanged }: AccessSearchProps) => {
  const [input, setInput] = useState('')
  const [result, setResult] = useState<AccessCheck | null>(null)
  const [profile, setProfile] = useState<NostrProfile | undefined>()
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState<string | null>(null)

  const run = async (e?: Event) => {
    e?.preventDefault()
    const raw = input.trim()
    if (!raw) return

    setLoading(true)
    setError(null)
    setResult(null)
    setProfile(undefined)
    try {
      const pubkey = await resolvePubkeyInput(raw)
      const check = await adminApi.checkAccess(pubkey)
      setResult(check)
      fetchProfiles([check.hex])
        .then(found => setProfile(found.get(check.hex)))
        .catch(() => undefined)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Could not look that up')
    } finally {
      setLoading(false)
    }
  }

  const act = async (what: 'allow' | 'block') => {
    if (!result) return
    setBusy(what)
    setError(null)
    try {
      if (what === 'allow') await adminApi.addToWhitelist(result.hex)
      else await adminApi.addToBlacklist(result.hex)
      // Re-ask rather than assume: the answer is the ladder's, not ours.
      setResult(await adminApi.checkAccess(result.hex))
      onChanged()
    } catch (err) {
      setError(err instanceof Error ? err.message : 'That did not work')
    } finally {
      setBusy(null)
    }
  }

  return (
    <section class="admin-settings-card">
      <div class="admin-settings-card-header">
        <div>
          <h3>Is this account allowed?</h3>
          <p>
            Paste an npub, a hex key, or a NIP-05 address. Answers with the tier that
            admits them, or why they are refused.
          </p>
        </div>
      </div>

      <form onSubmit={run} class="admin-search-row mt-3">
        <div class="admin-search-field">
          <SearchIcon class="admin-search-icon" />
          <input
            class="admin-search-input"
            type="text"
            value={input}
            onInput={e => setInput((e.target as HTMLInputElement).value)}
            placeholder="npub1…, 64-char hex, or name@domain.com"
            aria-label="Account to check"
          />
        </div>
        <button class="admin-action-btn" type="submit" disabled={loading || !input.trim()}>
          {loading ? 'Checking…' : 'Check'}
        </button>
      </form>

      {error && (
        <p class="admin-access-hint" role="alert">{error}</p>
      )}

      {result && (
        <div class={`admin-check-result mt-3 ${result.admitted ? 'is-admitted' : 'is-refused'}`}>
          <div class="admin-check-head">
            <button
              class="admin-check-identity"
              onClick={() => onInspect({ hex: result.hex, npub: result.npub })}
              title={result.npub}
            >
              {profile?.picture && (
                <img
                  src={profile.picture}
                  alt=""
                  referrerpolicy="no-referrer"
                  class="admin-account-avatar"
                />
              )}
              <span>{getDisplayName(profile, result.npub)}</span>
            </button>
            <span class={`admin-status-badge ${result.admitted ? 'admin-status-badge-ok' : 'admin-status-badge-danger'}`}>
              {result.admitted ? 'Allowed' : 'Refused'}
            </span>
          </div>

          <p class="admin-check-tier">
            {TIER_OF[result.tier] ?? result.tier}
            {result.hops != null && ` · ${result.hops} hop${result.hops === 1 ? '' : 's'} away`}
          </p>

          {/* The relay's own words. It knows things the UI does not — whether
              the graph is still building, or incomplete at the outer hop. */}
          <p class="admin-check-explanation">{result.explanation}</p>

          <div class="admin-check-actions">
            {!result.admitted && result.tier !== 'blacklist' && (
              <button
                class="admin-action-btn"
                disabled={busy !== null}
                onClick={() => void act('allow')}
              >
                {busy === 'allow' ? 'Adding…' : 'Add to Tier 1'}
              </button>
            )}
            {result.tier !== 'blacklist' && (
              <button
                class="admin-action-btn admin-action-btn-secondary"
                disabled={busy !== null}
                onClick={() => void act('block')}
              >
                {busy === 'block' ? 'Blocking…' : 'Block'}
              </button>
            )}
          </div>
        </div>
      )}
    </section>
  )
}
