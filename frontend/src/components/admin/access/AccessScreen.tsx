import { useEffect, useState } from 'preact/hooks'
import { adminApi, type AccessTierSummary } from '../../../services/AdminApiClient'
import { ProfileCard } from '../ProfileCard'
import { fetchProfiles, type NostrProfile } from '../../../services/ProfileFetcher'
import { AccessSearch } from './AccessSearch'
import { TierPanel } from './TierPanel'
import { BlockedPanel } from './BlockedPanel'
// The access mode, rate-limit ladder and web-of-trust configuration still live
// here. Only its listing sections moved out -- those are what the tier panels
// replaced, and keeping both would show the same accounts twice in two shapes.
import { WhitelistManager } from '../WhitelistManager'

type Section = 'tier1' | 'tier2' | 'tier3' | 'blocked' | 'policy'

/**
 * Everything that decides who can connect, in one place.
 *
 * Access and References used to be two screens that each explained the
 * mechanism by pointing at the other — "graph roots are managed on the
 * References screen", "the rules that use them live on the Access screen" —
 * and one of those pointers named the wrong screen for a toggle 25 lines
 * below it. Reference accounts are not a separate concept; they are what
 * seeds Tier 1, so they live inside it.
 *
 * Admission is presented in the order the relay actually decides in: blocked
 * overrides everything, then hand-added, then followed-by-a-reference, then
 * distance in the follow graph. The tiers partition the admitted set — every
 * account the relay lets in appears in exactly one of them.
 */
export const AccessScreen = () => {
  const [section, setSection] = useState<Section>('tier1')
  const [summary, setSummary] = useState<AccessTierSummary | null>(null)
  const [inspecting, setInspecting] = useState<{ hex: string; npub: string } | null>(null)
  const [profiles, setProfiles] = useState<Map<string, NostrProfile>>(new Map())
  const [reloadKey, setReloadKey] = useState(0)

  const loadSummary = () => {
    adminApi
      .getAccessTiers()
      .then(setSummary)
      // The tiers still render from their own endpoints; only the header counts
      // are missing, so a failure here must not blank the screen.
      .catch(() => undefined)
  }

  useEffect(loadSummary, [reloadKey])

  useEffect(() => {
    if (!inspecting) return
    fetchProfiles([inspecting.hex])
      .then(found => setProfiles(prev => new Map([...prev, ...found])))
      .catch(() => undefined)
  }, [inspecting])

  const changed = () => setReloadKey(k => k + 1)

  const hopCount = (hops: number) =>
    summary?.wot.per_hop.find(h => h.hops === hops)?.count ?? 0

  const tabs: { id: Section; label: string; count: number | null; hint: string }[] = [
    {
      id: 'tier1',
      label: 'Tier 1',
      count: summary?.tier1.total ?? null,
      hint: 'Added by hand, the reference accounts themselves, and everyone they follow. Full publishing budget.',
    },
    {
      id: 'tier2',
      label: 'Tier 2',
      count: summary ? hopCount(2) : null,
      hint: 'Two hops away in the follow graph. Half the publishing budget.',
    },
    {
      id: 'tier3',
      label: 'Tier 3',
      count: summary ? hopCount(3) : null,
      hint: 'Three hops away. A quarter of the publishing budget, and the tier most likely to be incomplete.',
    },
    {
      id: 'blocked',
      label: 'Blocked',
      count: summary?.blocked ?? null,
      hint: 'Refused regardless of any tier. The blacklist overrides everything above.',
    },
    {
      id: 'policy',
      label: 'Policy',
      count: null,
      hint: 'Whether admission is enforced at all, the rate budget each tier gets, and how the follow graph is built.',
    },
  ]

  return (
    <div>
      {summary?.open_relay && (
        <div class="admin-settings-card mb-4">
          <div class="admin-settings-card-header">
            <div>
              <h3>Admission is not enforced</h3>
              <p>
                Nothing restricts who can connect, so the tiers below describe what
                <em> would </em> apply rather than what does. Any pubkey on the internet
                can publish here, and the rate limits are the only brake.
              </p>
            </div>
            <span class="admin-status-badge admin-status-badge-warn">Open relay</span>
          </div>
        </div>
      )}

      <AccessSearch onInspect={setInspecting} onChanged={changed} />

      <nav class="admin-tier-tabs mt-4" aria-label="Access tiers">
        {tabs.map(t => (
          <button
            key={t.id}
            class={`admin-tier-tab ${section === t.id ? 'is-active' : ''}`}
            onClick={() => setSection(t.id)}
            aria-pressed={section === t.id}
            title={t.hint}
          >
            <span class="admin-tier-tab-label">{t.label}</span>
            <span class="admin-tier-tab-count">
              {t.count == null
                ? '—'
                : `${summary?.wot.truncated && t.id === 'tier3' ? '≥' : ''}${t.count.toLocaleString()}`}
            </span>
          </button>
        ))}
      </nav>

      <div class="mt-4">
        {section === 'tier1' && (
          <TierPanel
            key={`t1-${reloadKey}`}
            tier={1}
            title="Tier 1"
            blurb="Added by hand, the reference accounts, and everyone they follow. These publish at the full rate budget."
            onInspect={setInspecting}
            onRemove={async entry => {
              await adminApi.removeFromWhitelist(entry.hex)
              changed()
            }}
          />
        )}

        {section === 'tier2' && (
          <TierPanel
            key={`t2-${reloadKey}`}
            tier={2}
            title="Tier 2"
            blurb="Two hops away in the follow graph — followed by someone a reference account follows. Half the rate budget. Membership follows the graph, so there is nothing to remove here; block an account to refuse it."
            onInspect={setInspecting}
          />
        )}

        {section === 'tier3' && (
          <TierPanel
            key={`t3-${reloadKey}`}
            tier={3}
            title="Tier 3"
            blurb="Three hops away. A quarter of the rate budget. This is the outermost tier, so it is the one the graph's fetch budget truncates first — a count here can be a floor rather than a total."
            onInspect={setInspecting}
          />
        )}

        {section === 'blocked' && (
          <BlockedPanel key={`b-${reloadKey}`} onInspect={setInspecting} onChanged={changed} />
        )}

        {section === 'policy' && <WhitelistManager />}
      </div>

      {inspecting && (
        <ProfileCard
          profile={profiles.get(inspecting.hex)}
          hex={inspecting.hex}
          npub={inspecting.npub}
          onClose={() => setInspecting(null)}
        />
      )}
    </div>
  )
}
