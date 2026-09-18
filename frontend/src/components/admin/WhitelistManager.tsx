import { useState, useEffect, useRef } from 'preact/hooks'
import {
  adminApi,
  type AccessSettings,
  type AccessSources,
  type AccessCheck,
  type WotConfigured,
  type WotStatus,
} from '../../services/AdminApiClient'
import { fetchProfiles, getDisplayName, type NostrProfile } from '../../services/ProfileFetcher'
import { ProfileCard, CopyNpubButton } from './ProfileCard'
import { SearchIcon } from './SearchIcon'
import { AdminEmptyState } from './AdminEmptyState'
import { AccessIcon, ChevronIcon } from './icons'
import { useDirtySection } from './settingsDirty'
import { resolveNip05, resolvePubkeyInput } from '../../services/nip05'

interface WhitelistEntry {
  hex: string
  npub: string
}

interface BlacklistEntry {
  hex: string
  npub: string
}


/**
 * Rate-limit presets, as sliders rather than free number fields.
 *
 * "6000 events per minute" means nothing on its own -- an operator cannot tell
 * whether it is generous or about to throttle their own users, and a typo of
 * one digit silently changes the relay by 10x. Stepping through a named ladder
 * makes the choice legible.
 *
 * The three ladders share tier names and indices so one master control can move
 * them together; the values still differ, because a per-socket cap sensibly
 * sits above a per-key one and the whole-relay backstop above both.
 */
const RATE_TIERS = ['Strict', 'Tight', 'Moderate', 'Default', 'Generous', 'Unlimited'] as const

/** The tier every relay ships with, and what "Reset to defaults" restores. */
const DEFAULT_TIER = 3

type RateField =
  | 'pubkey_rate_limit_per_minute'
  | 'connection_rate_limit_per_minute'
  | 'global_rate_limit_per_minute'

const RATE_LIMITS: { field: RateField; label: string; hint: string; values: number[] }[] = [
  {
    field: 'pubkey_rate_limit_per_minute',
    label: 'Per pubkey',
    hint: "One key's budget. Spammers rotating connections still share this bucket.",
    values: [60, 300, 1200, 6000, 30000, 120000],
  },
  {
    field: 'connection_rate_limit_per_minute',
    label: 'Per connection',
    hint: 'Caps a single socket. Should sit above the per-pubkey budget.',
    values: [120, 600, 2400, 12000, 60000, 240000],
  },
  {
    field: 'global_rate_limit_per_minute',
    label: 'Whole relay',
    hint: 'Total across every connection — the backstop against a flood.',
    values: [6000, 60000, 300000, 600000, 2000000, 10000000],
  },
]

const formatRate = (n: number) =>
  n >= 1_000_000 ? `${n / 1_000_000}M` : n >= 1_000 ? `${n / 1_000}k` : String(n)

/** Index of the rung nearest `value`, for a value that need not be on the ladder. */
const nearestTier = (values: number[], value: number) =>
  values.reduce(
    (best, v, i) => (Math.abs(v - value) < Math.abs(values[best] - value) ? i : best),
    0,
  )

const RateLimitSlider = ({
  label,
  hint,
  values,
  value,
  onChange,
}: {
  label: string
  hint: string
  values: number[]
  value: number
  onChange: (value: number) => void
}) => {
  // A configured value need not be on the ladder -- an operator may have set it
  // by hand in YAML. Snap the handle to the nearest rung rather than pretending
  // the value is something it is not, and label it "custom".
  const tier = nearestTier(values, value)
  const exact = values[tier] === value

  return (
    <div class="admin-rate-slider">
      <div class="admin-rate-slider-head">
        <span class="admin-rate-slider-label">{label}</span>
        <span class="admin-rate-slider-value">
          <strong>{formatRate(value)}</strong> /min
          <em>{exact ? RATE_TIERS[tier] : 'custom'}</em>
        </span>
      </div>
      <input
        type="range"
        class="admin-rate-range"
        min={0}
        max={values.length - 1}
        step={1}
        value={tier}
        aria-label={`${label} events per minute`}
        onInput={e => onChange(values[Number((e.target as HTMLInputElement).value)])}
      />
      <div class="admin-rate-ticks" aria-hidden="true">
        {values.map(v => (
          <span key={v}>{formatRate(v)}</span>
        ))}
      </div>
      <p class="admin-rate-slider-hint">{hint}</p>
    </div>
  )
}

export const WhitelistManager = () => {
  const [entries, setEntries] = useState<WhitelistEntry[]>([])
  const [profiles, setProfiles] = useState<Map<string, NostrProfile>>(new Map())
  const [newPubkey, setNewPubkey] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [toast, setToast] = useState<string | null>(null)
  const [confirmRemove, setConfirmRemove] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  const [profilesLoading, setProfilesLoading] = useState(false)
  const [adding, setAdding] = useState(false)
  const [search, setSearch] = useState('')
  const [accessSettings, setAccessSettings] = useState<AccessSettings | null>(null)
  const [accessLoading, setAccessLoading] = useState(true)
  const [savingAccess, setSavingAccess] = useState(false)
  // Last-saved copy, so the save bar can tell an edit from a reload.
  const [accessBaseline, setAccessBaseline] = useState<AccessSettings | null>(null)
  // Target for the empty state's call to action, so "add a pubkey" is a button
  // that puts the cursor in the field rather than an instruction to go find it.
  const addPubkeyInput = useRef<HTMLInputElement | null>(null)
  // Per-tier counts. The table below can only ever show the enumerable tiers,
  // so without this the screen under-reports who can actually connect.
  const [sources, setSources] = useState<AccessSources | null>(null)

  // Web-of-Trust admission lives here, not on the References screen. It is a
  // rule about who may connect, which is what this screen is for; having it
  // next to the reference accounts meant the tier that admits the most people
  // was the one place you would not look for access settings.
  const [wot, setWot] = useState<WotStatus | null>(null)
  const [wotForm, setWotForm] = useState<WotConfigured | null>(null)
  const [admittedOpen, setAdmittedOpen] = useState(true)

  // "Would this key get in, and why" — the only way to see the Web of Trust
  // working, since it never produces a list.
  const [checkInput, setCheckInput] = useState('')
  const [checkResult, setCheckResult] = useState<AccessCheck | null>(null)
  const [checking, setChecking] = useState(false)
  const [checkError, setCheckError] = useState<string | null>(null)

  const runCheck = async () => {
    if (!checkInput.trim()) return
    setChecking(true)
    setCheckError(null)
    setCheckResult(null)
    try {
      const pubkey = await resolvePubkeyInput(checkInput)
      setCheckResult(await adminApi.checkAccess(pubkey))
    } catch (e) {
      setCheckError((e as Error).message)
    } finally {
      setChecking(false)
    }
  }

  const loadWot = () => {
    adminApi.getWotStatus()
      .then(status => {
        setWot(status)
        setWotForm(status.configured)
        const visible = status.admitted.slice(0, 40).map(a => a.hex)
        if (visible.length > 0) {
          fetchProfiles(visible)
            .then(fetched => setProfiles(prev => {
              const next = new Map(prev)
              for (const [hex, profile] of fetched) next.set(hex, profile)
              return next
            }))
            .catch(() => undefined)
        }
      })
      .catch(() => undefined)
  }

  const saveWot = async () => {
    if (!wotForm) return
    await adminApi.updateWotSettings(wotForm)
    showToast('Web-of-Trust settings saved. Restart the relay to apply them.')
    loadWot()
    adminApi.getAccessSources().then(setSources).catch(() => undefined)
  }

  const wotDirty = Boolean(
    wot && wotForm && JSON.stringify(wotForm) !== JSON.stringify(wot.configured),
  )

  useDirtySection(
    {
      id: 'wot',
      label: 'Web of Trust',
      dirty: wotDirty,
      blocked:
        wotDirty && wotForm?.enabled && !wotForm.local && !wotForm.oracle_url.trim()
          ? 'an oracle URL is required unless computing locally'
          : null,
      consequence:
        wotDirty && wotForm?.enabled && !wot?.configured.enabled
          ? 'Enabling admission lets anyone within the hop limit connect without being on the allowlist — and, with an empty allowlist, stops this being an open relay.'
          : null,
    },
    { save: saveWot, discard: () => setWotForm(wot?.configured ?? null) },
  )

  // Blacklist state
  const [blacklist, setBlacklist] = useState<BlacklistEntry[]>([])
  const [blacklistProfiles, setBlacklistProfiles] = useState<Map<string, NostrProfile>>(new Map())
  const [newBlacklistPubkey, setNewBlacklistPubkey] = useState('')
  const [blacklistOpen, setBlacklistOpen] = useState(false)
  const [blacklistLoading, setBlacklistLoading] = useState(false)
  const [blacklistAdding, setBlacklistAdding] = useState(false)
  const [confirmBlacklistRemove, setConfirmBlacklistRemove] = useState<string | null>(null)

  // Profile card state
  const [selectedProfile, setSelectedProfile] = useState<{ hex: string; npub: string; profile?: NostrProfile } | null>(null)

  // Set of blacklisted hex keys for quick lookup
  const blacklistedSet = new Set(blacklist.map(b => b.hex))

  const showToast = (msg: string) => {
    setToast(msg)
    setTimeout(() => setToast(null), 3000)
  }

  const fetchWhitelist = () => {
    setLoading(true)
    adminApi.getWhitelist()
      .then(data => { setEntries(data); setError(null); loadProfiles(data) })
      .catch(e => setError(e.message))
      .finally(() => setLoading(false))
  }

  const fetchBlacklist = () => {
    setBlacklistLoading(true)
    adminApi.getBlacklist()
      .then(data => { setBlacklist(data); loadBlacklistProfiles(data) })
      .catch(() => {})
      .finally(() => setBlacklistLoading(false))
  }

  const loadProfiles = async (data: WhitelistEntry[]) => {
    if (data.length === 0) return
    setProfilesLoading(true)
    try {
      const profs = await fetchProfiles(data.map(e => e.hex))
      setProfiles(profs)
    } catch {
      // Profiles are optional
    } finally {
      setProfilesLoading(false)
    }
  }

  const loadBlacklistProfiles = async (data: BlacklistEntry[]) => {
    if (data.length === 0) return
    try {
      const profs = await fetchProfiles(data.map(e => e.hex))
      setBlacklistProfiles(profs)
    } catch {
      // optional
    }
  }

  const fetchAccessSettings = () => {
    setAccessLoading(true)
    adminApi.getAccessSettings()
      .then(settings => {
        setAccessSettings(settings)
        setAccessBaseline(settings)
      })
      .catch(e => setError(e.message))
      .finally(() => setAccessLoading(false))
  }

  useEffect(() => {
    fetchWhitelist()
    fetchBlacklist()
    fetchAccessSettings()
    adminApi.getAccessSources().then(setSources).catch(() => undefined)
    loadWot()
  }, [])

  const updateAccessField = (field: keyof AccessSettings, value: AccessSettings[keyof AccessSettings]) => {
    setAccessSettings(prev => prev ? { ...prev, [field]: value } : prev)
  }

  // The tier all three limits agree on, or null when they have been tuned
  // apart. Derived rather than stored, so a hand-edited YAML value is reflected
  // honestly instead of the master claiming a tier nothing is actually on.
  const masterTier = (() => {
    if (!accessSettings) return DEFAULT_TIER
    const tiers = RATE_LIMITS.map(l => {
      const value = accessSettings[l.field] as number
      const t = nearestTier(l.values, value)
      return l.values[t] === value ? t : -1
    })
    return tiers.every(t => t >= 0 && t === tiers[0]) ? tiers[0] : null
  })()

  const applyTier = (tier: number) => {
    setAccessSettings(prev => {
      if (!prev) return prev
      const next = { ...prev }
      for (const limit of RATE_LIMITS) {
        next[limit.field] = limit.values[tier]
      }
      return next
    })
  }

  const handleSaveAccess = async () => {
    if (!accessSettings) return
    setSavingAccess(true)
    setError(null)
    try {
      const next = await adminApi.updateAccessSettings({
        access_policy: accessSettings.access_policy,
        pubkey_rate_limit_per_minute: accessSettings.pubkey_rate_limit_per_minute,
        connection_rate_limit_per_minute: accessSettings.connection_rate_limit_per_minute,
        global_rate_limit_per_minute: accessSettings.global_rate_limit_per_minute,
      })
      setAccessSettings(next)
      setAccessBaseline(next)
      showToast(next.restart_required ? 'Access settings saved. Restart relay to apply rate-limit changes.' : 'Access settings saved')
      fetchWhitelist()
    } catch (e) {
      // Rethrown so the save bar can name this section in its failure list.
      setError(e instanceof Error ? e.message : 'Failed to save access settings')
      throw e
    } finally {
      setSavingAccess(false)
    }
  }

  // Only the four fields the save actually sends. Comparing whole objects would
  // report a pending change whenever the server echoed back a derived field
  // like `restart_required`.
  const accessDirty = Boolean(
    accessSettings &&
    accessBaseline &&
    (accessSettings.access_policy !== accessBaseline.access_policy ||
      accessSettings.pubkey_rate_limit_per_minute !== accessBaseline.pubkey_rate_limit_per_minute ||
      accessSettings.connection_rate_limit_per_minute !== accessBaseline.connection_rate_limit_per_minute ||
      accessSettings.global_rate_limit_per_minute !== accessBaseline.global_rate_limit_per_minute),
  )

  // Rate limits are mandatory in open mode, so a blank or zero field is not a
  // saveable state. Reported as blocked rather than clean: the edit is real,
  // it just cannot be committed yet.
  const accessBlocked =
    accessDirty &&
    accessSettings?.access_policy === 'open' &&
    [
      accessSettings.pubkey_rate_limit_per_minute,
      accessSettings.connection_rate_limit_per_minute,
      accessSettings.global_rate_limit_per_minute,
    ].some(n => !Number.isFinite(n) || n < 1)
      ? 'rate limits must be at least 1 in open mode'
      : null

  useDirtySection(
    {
      id: 'access',
      label: 'Access',
      dirty: accessDirty,
      blocked: accessBlocked,
      // Only while the switch to open mode is genuinely pending. The old
      // static line showed this whenever open mode was selected, including
      // when it was already saved and there was nothing left to clear.
      consequence:
        accessDirty &&
        accessSettings?.access_policy === 'open' &&
        accessBaseline?.access_policy !== 'open' &&
        entries.length > 0
          ? `Saving open mode clears all ${entries.length} whitelist entr${entries.length === 1 ? 'y' : 'ies'}.`
          : null,
    },
    {
      save: handleSaveAccess,
      discard: () => setAccessSettings(accessBaseline),
    },
  )

  const handleAdd = async () => {
    const value = newPubkey.trim()
    if (!value) return
    setError(null)
    setAdding(true)
    try {
      let pubkeyToAdd = value

      // NIP-05 resolution: name@domain
      if (value.includes('@') && !value.startsWith('npub')) {
        showToast(`Looking up ${value}…`)
        pubkeyToAdd = await resolveNip05(value)
      }

      const entry = await adminApi.addToWhitelist(pubkeyToAdd)
      setEntries(prev => [...prev.filter(e => e.hex !== entry.hex), entry])
      setNewPubkey('')
      showToast('Pubkey added to whitelist')
      fetchProfiles([entry.hex]).then(profs => {
        setProfiles(prev => {
          const next = new Map(prev)
          const p = profs.get(entry.hex)
          if (p) next.set(entry.hex, p)
          return next
        })
      })
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to add')
    } finally {
      setAdding(false)
    }
  }

  const handleRemove = async (hex: string) => {
    setError(null)
    try {
      await adminApi.removeFromWhitelist(hex)
      setEntries(prev => prev.filter(e => e.hex !== hex))
      setConfirmRemove(null)
      showToast('Pubkey removed from whitelist')
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to remove')
    }
  }

  const handleBlacklistAdd = async () => {
    if (!newBlacklistPubkey.trim()) return
    setError(null)
    setBlacklistAdding(true)
    try {
      const entry = await adminApi.addToBlacklist(newBlacklistPubkey.trim())
      setBlacklist(prev => [...prev.filter(e => e.hex !== entry.hex), entry])
      setNewBlacklistPubkey('')
      showToast('Pubkey added to blacklist')
      fetchProfiles([entry.hex]).then(profs => {
        setBlacklistProfiles(prev => {
          const next = new Map(prev)
          const p = profs.get(entry.hex)
          if (p) next.set(entry.hex, p)
          return next
        })
      })
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to add to blacklist')
    } finally {
      setBlacklistAdding(false)
    }
  }

  const handleBlacklistRemove = async (hex: string) => {
    setError(null)
    try {
      await adminApi.removeFromBlacklist(hex)
      setBlacklist(prev => prev.filter(e => e.hex !== hex))
      setConfirmBlacklistRemove(null)
      showToast('Pubkey removed from blacklist')
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to remove from blacklist')
    }
  }

  const truncate = (s: string) => s.length > 16 ? `${s.slice(0, 8)}...${s.slice(-8)}` : s

  const openProfile = (hex: string, npub: string) => {
    const profile = profiles.get(hex) || blacklistProfiles.get(hex)
    setSelectedProfile({ hex, npub, profile })
  }

  // Filter by search query: name, npub, hex, nip05
  const q = search.toLowerCase()
  const filtered = entries.filter(entry => {
    if (!q) return true
    if (entry.hex.toLowerCase().includes(q)) return true
    if (entry.npub.toLowerCase().includes(q)) return true
    const profile = profiles.get(entry.hex)
    if (profile?.name?.toLowerCase().includes(q)) return true
    if (profile?.display_name?.toLowerCase().includes(q)) return true
    if (profile?.nip05?.toLowerCase().includes(q)) return true
    return false
  })

  // Hint: is the add input a NIP-05?
  const isNip05Input = newPubkey.includes('@') && !newPubkey.startsWith('npub')

  return (
    <div>

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

      <section class="admin-settings-card mb-5">
        <div class="admin-settings-card-header">
          <div>
            <h3>Relay Access Mode</h3>
            <p>Open access requires rate limits. Enforced access uses the whitelist below.</p>
          </div>
          {accessSettings?.restart_required && (
            <span class="admin-status-badge admin-status-badge-warn">Restart required</span>
          )}
        </div>

        {accessLoading || !accessSettings ? (
          <div class="lc-skeleton h-28 w-full" />
        ) : (
          <>
            <div class="admin-mode-grid">
              <button
                type="button"
                onClick={() => updateAccessField('access_policy', 'owner_only')}
                class={`admin-mode-option ${accessSettings.access_policy === 'owner_only' ? 'admin-mode-option-active' : ''}`}
              >
                <span class="admin-mode-title">Restricted · recommended</span>
                <span class="admin-mode-copy">
                  Only people you allow: you, anyone added by hand, everyone your
                  reference accounts follow, and — if enabled — anyone close
                  enough in the follow graph.
                </span>
              </button>
              <button
                type="button"
                onClick={() => updateAccessField('access_policy', 'open')}
                class={`admin-mode-option admin-mode-option-danger ${accessSettings.access_policy === 'open' ? 'admin-mode-option-active' : ''}`}
              >
                <span class="admin-mode-title">Open to everyone · dangerous</span>
                <span class="admin-mode-copy">
                  Any pubkey on the internet can publish here. Storage grows until
                  the disk is full and rate limits are the only brake.
                </span>
              </button>
            </div>

            {/* Shown in both modes. These were only rendered for an open relay,
                yet they are enforced either way -- so a restricted relay had
                per-pubkey limits in force that the screen never mentioned. */}
            <div class="admin-rate-sliders mt-4">
                {/* One control for the common case. Tuning three interdependent
                    numbers individually is rarely what an operator wants; they
                    want "tighter" or "looser" overall. The individual sliders
                    stay for when it genuinely matters. */}
                <div class="admin-rate-master">
                  <div class="admin-rate-slider-head">
                    <span class="admin-rate-slider-label">Overall strictness</span>
                    <span class="admin-rate-slider-value">
                      <em>{masterTier === null ? 'mixed' : RATE_TIERS[masterTier]}</em>
                    </span>
                  </div>
                  <input
                    type="range"
                    class="admin-rate-range"
                    min={0}
                    max={RATE_TIERS.length - 1}
                    step={1}
                    value={masterTier ?? DEFAULT_TIER}
                    aria-label="Overall rate-limit strictness"
                    onInput={e => applyTier(Number((e.target as HTMLInputElement).value))}
                  />
                  <div class="admin-rate-ticks" aria-hidden="true">
                    {RATE_TIERS.map(t => <span key={t}>{t}</span>)}
                  </div>
                  <div class="admin-rate-master-actions">
                    <span class="admin-rate-slider-hint">
                      {masterTier === null
                        ? 'The three limits are on different tiers. Move this to align them.'
                        : 'Moves all three together.'}
                    </span>
                    <button
                      type="button"
                      class="admin-rate-reset"
                      onClick={() => applyTier(DEFAULT_TIER)}
                      disabled={masterTier === DEFAULT_TIER}
                    >
                      Reset to defaults
                    </button>
                  </div>
                </div>

                {RATE_LIMITS.map(limit => (
                  <RateLimitSlider
                    key={limit.field}
                    label={limit.label}
                    hint={limit.hint}
                    values={limit.values}
                    value={accessSettings[limit.field] as number}
                    onChange={v => updateAccessField(limit.field, v)}
                  />
                ))}

              {sources && sources.budget_ladder.length > 0 && (
                <div class="admin-budget-ladder">
                  <div class="admin-budget-ladder-head">
                    How the budget is shared
                    <small>
                      The slider above sets what a fully trusted key may publish.
                      Everyone else gets a share of it: the further from your
                      reference accounts someone is, the less evidence there is
                      that they should be trusted with your disk.
                    </small>
                  </div>
                  {/* Grouped by budget, not by tier. Listing every tier gave
                      rows that read as duplicates -- "followed by a reference
                      account" and "1 hop" are the same people, and several
                      tiers share a number. One row per distinct level. */}
                  {Object.values(
                    sources.budget_ladder.reduce((acc, rung) => {
                      const key = String(rung.percent)
                      if (!acc[key]) {
                        acc[key] = { percent: rung.percent, perMinute: rung.events_per_minute, who: [] }
                      }
                      acc[key].who.push(rung.label)
                      return acc
                    }, {} as Record<string, { percent: number; perMinute: number; who: string[] }>),
                  )
                    .sort((a, b) => b.percent - a.percent)
                    .map(level => (
                      <div key={level.percent} class="admin-budget-rung">
                        <span class="admin-budget-rung-label">
                          {level.who.join(', ')}
                        </span>
                        <span class="admin-budget-rung-bar">
                          <span style={{ width: `${level.percent}%` }} />
                        </span>
                        <span class="admin-budget-rung-value">
                          {level.perMinute.toLocaleString()}/min
                        </span>
                      </div>
                    ))}
                </div>
              )}
            </div>

            {/* Saving is the shared bar's job now, so this row states the
                current position only. What saving would *also* do moves to the
                bar, where it appears only while that change is pending. */}
            <div class="admin-settings-actions">
              <div class="text-sm" style={{ color: 'var(--color-text-secondary)' }}>
                {entries.length} pubkey{entries.length !== 1 ? 's' : ''} currently whitelisted.
                {savingAccess && ' Saving…'}
              </div>
            </div>
          </>
        )}
      </section>

      {sources && (
        <section class="admin-settings-card mb-5">
          <div class="admin-settings-card-header">
            <div>
              <h3>Who can connect</h3>
              <p>
                Every tier that grants access, and how many each accounts for.
                The table further down lists only the first two — the Web of
                Trust is computed per pubkey against the follow graph, so it has
                no list to show.
              </p>
            </div>
          </div>

          <div class="admin-storage-stats mt-4">
            <div class="admin-stat-card" title="Pubkeys added by hand on this screen.">
              <span>Added by hand</span>
              <strong>{sources.manual.toLocaleString()}</strong>
            </div>
            <div class="admin-stat-card" title="Pulled from the reference accounts' contact lists by follow sync.">
              <span>Follow sync</span>
              <strong>{sources.follow_derived.toLocaleString()}</strong>
            </div>
            <div
              class="admin-stat-card"
              title={
                sources.wot_enabled
                  ? `Accounts within ${sources.wot_max_hops} hops of the reference accounts. Resolved per pubkey on connect, so they never appear as a list.`
                  : 'Web-of-Trust admission is off. Enable it on the References screen.'
              }
            >
              <span>Web of Trust</span>
              <strong>
                {sources.wot_enabled
                  ? sources.wot_admitted > 0
                    ? sources.wot_admitted.toLocaleString()
                    : 'building…'
                  : 'off'}
              </strong>
            </div>
            <div class="admin-stat-card" title="Blocked outright; overrides every tier to its left.">
              <span>Blocked</span>
              <strong>{sources.blacklisted.toLocaleString()}</strong>
            </div>
          </div>

          {sources.open_relay && (
            <p class="text-xs mt-3" style={{ color: '#fcd34d' }}>
              No tier restricts anything, so any pubkey can connect.
            </p>
          )}
        </section>
      )}

      {wot && wotForm && (
        <section class="admin-settings-card mb-5">
          <div class="admin-settings-card-header">
            <div>
              <h3>Web of Trust admission</h3>
              <p>
                Admit anyone within a few follow-graph hops of the accounts
                below, without adding them to the allowlist by hand. The
                blacklist still overrides it, and an unreachable oracle admits
                nobody rather than everybody.
              </p>
            </div>
            {/* Whether it is ON is far less useful than whether it is WORKING:
                an enabled tier with a dead oracle or no roots admits nobody and
                looks identical to a quiet relay. */}
            <span
              class={`admin-status-badge ${
                wot.enabled && wot.oracle_reachable && !wot.degraded
                  ? ''
                  : wot.enabled
                    ? 'admin-status-badge-danger'
                    : 'admin-status-badge-warn'
              }`}
            >
              {wot.status}
            </span>
          </div>

          <label class="admin-toggle-row">
            <input
              type="checkbox"
              checked={wotForm.enabled}
              onChange={e => setWotForm({ ...wotForm, enabled: (e.target as HTMLInputElement).checked })}
            />
            <span>
              <strong>Admit pubkeys inside the web of trust</strong>
              <small>
                Applied at startup, so this takes effect on the next relay
                restart. Leave off and only the allowlist and follow sync decide.
              </small>
            </span>
          </label>

          {wotForm.enabled && (
            <>
              <label class="admin-toggle-row">
                <input
                  type="checkbox"
                  checked={wotForm.local}
                  onChange={e => setWotForm({ ...wotForm, local: (e.target as HTMLInputElement).checked })}
                />
                <span>
                  <strong>Compute the graph on this relay</strong>
                  <small>
                    Builds the follow graph from contact lists this relay holds,
                    fetching the rest as needed. No sidecar to run, nothing to be
                    unreachable. Turn off only to query an external oracle.
                  </small>
                </span>
              </label>

              <div class="admin-rate-grid mt-4">
                <label>
                  <span>Max hops</span>
                  <input
                    type="number"
                    min="1"
                    max="5"
                    value={wotForm.max_hops}
                    onInput={e => setWotForm({ ...wotForm, max_hops: Number((e.target as HTMLInputElement).value) })}
                  />
                </label>
                {!wotForm.local && (
                  <label style={{ gridColumn: 'span 2' }}>
                    <span>Oracle URL</span>
                    <input
                      type="text"
                      placeholder="http://wot-oracle:8080"
                      value={wotForm.oracle_url}
                      onInput={e => setWotForm({ ...wotForm, oracle_url: (e.target as HTMLInputElement).value })}
                    />
                  </label>
                )}
              </div>
              <p class="text-xs mt-2" style={{ color: 'var(--color-text-secondary)' }}>
                1 hop is what follow sync already gives you; past 3 is noise.
                Graph roots are the relay's reference accounts, managed on the
                References screen.
                {wot.local && wot.graph_accounts > 0 && (
                  <> Graph currently knows {wot.graph_accounts.toLocaleString()} follow
                  {' '}list{wot.graph_accounts === 1 ? '' : 's'} and
                  {' '}{wot.graph_edges.toLocaleString()} follow edges.</>
                )}
              </p>
            </>
          )}

          {wot.oracle_error && wotForm.enabled && (
            <p class="text-xs mt-3" style={{ color: wot.local ? '#fcd34d' : '#fca5a5' }}>
              {wot.local
                ? `${wot.oracle_error}. It is rebuilt hourly and on startup; nobody is admitted through this tier until it is ready.`
                : `Oracle unreachable: ${wot.oracle_error}. Nobody is being admitted through this tier until it responds.`}
            </p>
          )}

          {wot.restart_required && (
            <p class="text-xs mt-3" style={{ color: '#fcd34d' }}>
              Saved settings differ from what is running. Restart the relay from
              Settings to apply them.
            </p>
          )}

          {wotForm.enabled && wot.graph_truncated && (
            <p class="text-xs mt-3" style={{ color: '#fcd34d' }}>
              The graph is complete only to {wot.graph_complete_to_hop} hop
              {wot.graph_complete_to_hop === 1 ? '' : 's'}: fetching every contact
              list needed for {wot.max_hops} would take tens of thousands of
              requests. Past {wot.graph_complete_to_hop} hops a refusal means the
              path was never fetched, not that it does not exist — which is why
              people you expect to be admitted are not. Set max hops to{' '}
              {wot.graph_complete_to_hop} for an answer that is actually
              complete, or add more reference accounts to widen the graph.
            </p>
          )}

          {wot.admitted.length > 0 && (
            <div class="mt-4">
              <div class="flex items-baseline justify-between gap-3 mb-2">
                <div class="text-xs" style={{ color: 'var(--color-text-secondary)' }}>
                  {/* The count is the answer to "is this working"; the list is
                      just enough faces to recognise that it is the right crowd. */}
                  <strong style={{ color: 'var(--color-text-primary)' }}>
                    {wot.would_admit_total.toLocaleString()}
                  </strong>{' '}
                  account{wot.would_admit_total === 1 ? '' : 's'} admitted within{' '}
                  {wot.max_hops} hop{wot.max_hops === 1 ? '' : 's'}
                  {wot.would_admit_total > wot.admitted.length && (
                    <> — showing the {wot.admitted.length} nearest</>
                  )}
                  . The number on each is its hop count; click for the profile.
                </div>
                <button
                  type="button"
                  class="text-xs"
                  style={{ color: 'var(--color-text-secondary)' }}
                  onClick={() => setAdmittedOpen(!admittedOpen)}
                >
                  {admittedOpen ? 'Hide' : 'Show'}
                </button>
              </div>

              {admittedOpen && (
                <div class="admin-admitted-grid">
                  {wot.admitted.map(entry => {
                    const profile = profiles.get(entry.hex)
                    // One identity line, not two. Without a profile the name
                    // fell back to a truncated npub while the line beneath
                    // showed the same npub truncated differently, so every row
                    // said the same thing twice.
                    const name = profile ? getDisplayName(profile, entry.npub) : null
                    return (
                      <button
                        key={entry.hex}
                        type="button"
                        class="admin-admitted-chip"
                        onClick={() => setSelectedProfile({ hex: entry.hex, npub: entry.npub, profile })}
                        title={`${entry.npub} — ${entry.hops} hop${entry.hops === 1 ? '' : 's'}`}
                      >
                        {profile?.picture ? (
                          <img
                            src={profile.picture}
                            alt=""
                            class="admin-admitted-avatar"
                            onError={e => { (e.target as HTMLImageElement).style.display = 'none' }}
                          />
                        ) : (
                          <span class="admin-admitted-avatar admin-admitted-avatar-fallback">
                            {entry.npub.slice(5, 7).toUpperCase()}
                          </span>
                        )}
                        <span class="admin-admitted-name">
                          {name ?? `${entry.npub.slice(0, 12)}…`}
                        </span>
                        <span class="admin-admitted-hops">{entry.hops}</span>
                      </button>
                    )
                  })}
                </div>
              )}
            </div>
          )}
        </section>
      )}

      {/* The Web of Trust admits by computation, so it never shows up as a
          list. Without a way to ask about one key, an operator cannot tell a
          working tier from a broken one. */}
      <section class="admin-settings-card mb-5">
        <div class="admin-settings-card-header">
          <div>
            <h3>Check a pubkey</h3>
            <p>Whether this key can connect right now, and which tier decides it.</p>
          </div>
        </div>
        <div class="admin-access-input-row mt-3">
          <input
            type="text"
            value={checkInput}
            onInput={e => setCheckInput((e.target as HTMLInputElement).value)}
            onKeyDown={e => { if (e.key === 'Enter') void runCheck() }}
            placeholder="npub1..., hex, or name@domain.com"
          />
          <button
            type="button"
            onClick={() => void runCheck()}
            disabled={checking || !checkInput.trim()}
            class="admin-access-button admin-access-button-allow"
          >
            {checking ? 'Checking…' : 'Check'}
          </button>
        </div>
        {checkError && <p class="text-xs mt-2" style={{ color: '#fca5a5' }}>{checkError}</p>}
        {checkResult && (
          <div
            class="admin-modal-preview mt-3"
            style={{
              borderColor: checkResult.admitted ? 'rgba(var(--color-accent-rgb), 0.35)' : undefined,
              background: checkResult.admitted ? 'rgba(var(--color-accent-rgb), 0.08)' : undefined,
              color: checkResult.admitted ? 'var(--color-text-secondary)' : undefined,
            }}
          >
            <strong style={{ color: checkResult.admitted ? 'var(--color-accent)' : undefined }}>
              {checkResult.admitted ? 'Admitted' : 'Refused'}
              {checkResult.hops !== null && ` — ${checkResult.hops} hop${checkResult.hops === 1 ? '' : 's'} away`}
            </strong>
            <span>{checkResult.explanation}</span>
          </div>
        )}
      </section>

      <div class="admin-access-actions mb-5">
        <section class="admin-access-panel admin-access-panel-allow">
          <div class="admin-access-panel-header">
            <div>
              <h3>Allow Pubkey</h3>
              <p>Whitelist access</p>
            </div>
            <span class="admin-access-count">{entries.length}</span>
          </div>
          <div class="admin-access-input-row">
            <input
              ref={addPubkeyInput}
              type="text"
              value={newPubkey}
              onInput={(e) => setNewPubkey((e.target as HTMLInputElement).value)}
              placeholder="npub1..., hex, or name@domain.com"
              class="admin-access-input"
              onKeyDown={(e) => e.key === 'Enter' && handleAdd()}
            />
            <button
              onClick={handleAdd}
              disabled={!newPubkey.trim() || adding}
              class="admin-access-button admin-access-button-allow"
            >
              {adding ? '...' : 'Add'}
            </button>
          </div>
          <div class="admin-access-hint">
            {isNip05Input ? 'NIP-05 will resolve on add' : 'Accepts npub, hex, or NIP-05'}
          </div>
        </section>

        <section class="admin-access-panel admin-access-panel-block">
          <div class="admin-access-panel-header">
            <div>
              <h3>Block Pubkey</h3>
              <p>Override access</p>
            </div>
            <span class="admin-access-count admin-access-count-danger">{blacklist.length}</span>
          </div>
          <div class="admin-access-input-row">
            <input
              type="text"
              value={newBlacklistPubkey}
              onInput={(e) => setNewBlacklistPubkey((e.target as HTMLInputElement).value)}
              placeholder="npub1... or hex pubkey"
              class="admin-access-input"
              onKeyDown={(e) => e.key === 'Enter' && handleBlacklistAdd()}
            />
            <button
              onClick={handleBlacklistAdd}
              disabled={!newBlacklistPubkey.trim() || blacklistAdding}
              class="admin-access-button admin-access-button-block"
            >
              {blacklistAdding ? '...' : 'Block'}
            </button>
          </div>
          <div class="admin-access-hint">Blocked keys cannot connect or publish</div>
        </section>
      </div>

      {/* Search */}
      {entries.length > 0 && (
        <div class="admin-search-field mb-4">
          <SearchIcon class="admin-search-icon" />
          <input
            type="text"
            value={search}
            onInput={(e) => setSearch((e.target as HTMLInputElement).value)}
            placeholder="Search by name, npub, NIP-05…"
            class="admin-search-input"
          />
        </div>
      )}

      {/* Table */}
      {loading ? (
        <div class="space-y-2">
          {[...Array(3)].map((_, i) => (
            <div key={i} class="lc-skeleton h-14 w-full" />
          ))}
        </div>
      ) : entries.length === 0 ? (
        /* An empty allowlist does not mean an open relay. The web of trust
           admits by reachability and lists no keys, so with it on this table is
           empty while a six-figure number of accounts get in — saying "anyone
           can connect" there is the opposite of what is happening. */
        sources?.wot_enabled ? (
          <AdminEmptyState
            icon={AccessIcon}
            headline="No keys listed here — the web of trust decides"
            stat={
              sources.wot_admitted > 0
                ? `~${sources.wot_admitted.toLocaleString()} accounts admitted`
                : 'graph not built yet'
            }
            action={{
              label: 'Add a pubkey anyway',
              onClick: () => addPubkeyInput.current?.focus(),
            }}
          >
            Nothing is listed by hand, so admission is decided entirely by the
            web of trust below: anyone within {sources.wot_max_hops} hops of a
            root gets in, and everyone else is refused. Adding a key here lets
            someone in regardless of the graph.
          </AdminEmptyState>
        ) : (
          <AdminEmptyState
            tone="caution"
            icon={AccessIcon}
            headline="No allowlist — anyone can connect"
            stat="0 pubkeys allowed"
            action={{
              label: 'Add the first pubkey',
              onClick: () => addPubkeyInput.current?.focus(),
            }}
          >
            Any pubkey may read from and publish to this relay. That is a valid
            way to run a public relay, but storage growth is then bounded only by
            your retention policy. Add one and everyone else is refused.
          </AdminEmptyState>
        )
      ) : filtered.length === 0 ? (
        <div style={{ color: 'var(--color-text-secondary)' }}>No entries match "{search}".</div>
      ) : (
        /* The allowlist runs to hundreds of entries, and rendering all of them
           inline pushed everything below it off the page. Capped and scrolled
           instead, so the screen is a fixed height whether the list holds 3
           keys or 3,000; the header stays put so columns stay readable. */
        <div class="lc-card overflow-hidden" style={{ padding: 0 }}>
          <div style={{ maxHeight: '60vh', overflowY: 'auto' }}>
          <table class="w-full">
            <thead>
              <tr style={{ background: 'var(--color-bg-primary)' }}>
                <th class="text-left px-4 py-2 text-sm font-medium" style={{ color: 'var(--color-text-secondary)', position: 'sticky', top: 0, background: 'var(--color-bg-primary)', zIndex: 1 }}>Profile</th>
                <th class="text-left px-4 py-2 text-sm font-medium" style={{ color: 'var(--color-text-secondary)', position: 'sticky', top: 0, background: 'var(--color-bg-primary)', zIndex: 1 }}>npub</th>
                <th class="text-right px-4 py-2 text-sm font-medium" style={{ color: 'var(--color-text-secondary)', position: 'sticky', top: 0, background: 'var(--color-bg-primary)', zIndex: 1 }}>Actions</th>
              </tr>
            </thead>
            <tbody>
              {filtered.map(entry => {
                const profile = profiles.get(entry.hex)
                const isBlacklisted = blacklistedSet.has(entry.hex)
                return (
                  <tr key={entry.hex} style={{ borderTop: '1px solid var(--color-border)' }} class="hover:bg-white/[0.02] transition-colors">
                    <td class="px-4 py-2">
                      <div
                        class="flex items-center gap-3 cursor-pointer"
                        onClick={() => openProfile(entry.hex, entry.npub)}
                      >
                        {profilesLoading && !profile ? (
                          <div class="lc-skeleton w-7 h-7 rounded-full flex-shrink-0" />
                        ) : profile?.picture ? (
                          <img src={profile.picture} alt="" class="w-7 h-7 rounded-full object-cover flex-shrink-0"
                            style={{ border: '1px solid var(--color-border)' }}
                            onError={(e) => { (e.target as HTMLImageElement).style.display = 'none' }} />
                        ) : (
                          <div class="w-7 h-7 rounded-full flex-shrink-0 flex items-center justify-center text-xs font-bold"
                            style={{ background: 'rgba(var(--color-accent-rgb), 0.1)', color: 'var(--color-accent)' }}>
                            {(profile?.name || entry.npub.slice(5, 7) || '??').slice(0, 2).toUpperCase()}
                          </div>
                        )}
                        <div>
                          <div class="text-sm font-medium flex items-center gap-2">
                            {profilesLoading && !profile ? (
                              <span class="lc-skeleton inline-block w-24 h-4" />
                            ) : (
                              getDisplayName(profile, entry.npub)
                            )}
                            {isBlacklisted && (
                              <span class="text-xs px-1.5 py-0.5 rounded" style={{ background: 'rgba(239,68,68,0.15)', color: '#f87171', fontSize: '10px' }}>
                                blocked
                              </span>
                            )}
                          </div>
                          <div class="text-xs font-mono" style={{ color: 'var(--color-text-secondary)' }}>
                            {profile?.nip05 ? profile.nip05 : truncate(entry.hex)}
                          </div>
                        </div>
                      </div>
                    </td>
                    <td class="px-4 py-2 text-sm font-mono" style={{ color: 'var(--color-text-secondary)' }}>
                      <span class="flex items-center">
                        {truncate(entry.npub)}
                        <CopyNpubButton npub={entry.npub} />
                      </span>
                    </td>
                    <td class="px-4 py-2 text-right">
                      {confirmRemove === entry.hex ? (
                        <span class="space-x-2">
                          <button
                            onClick={() => handleRemove(entry.hex)}
                            class="text-sm text-red-400 hover:text-red-300 transition-colors"
                          >
                            Confirm
                          </button>
                          <button
                            onClick={() => setConfirmRemove(null)}
                            class="text-sm transition-colors" style={{ color: 'var(--color-text-secondary)' }}
                          >
                            Cancel
                          </button>
                        </span>
                      ) : (
                        <button
                          onClick={() => setConfirmRemove(entry.hex)}
                          class="text-sm text-red-400 hover:text-red-300 transition-colors"
                        >
                          Remove
                        </button>
                      )}
                    </td>
                  </tr>
                )
              })}
            </tbody>
          </table>
          </div>
        </div>
      )}

      <div class="mt-4 text-sm" style={{ color: 'var(--color-text-secondary)' }}>
        {filtered.length !== entries.length
          ? `${filtered.length} of ${entries.length} whitelisted pubkey${entries.length !== 1 ? 's' : ''}`
          : `${entries.length} whitelisted pubkey${entries.length !== 1 ? 's' : ''}`
        }
      </div>

      {/* Blacklist. A collapsed section card rather than a bare text caret:
          every other group of settings here is a card with a title, a
          description and a status on the right, and this was the one place
          that rendered as a naked glyph with its description stranded below. */}
      <section class="admin-settings-card admin-disclosure-card mt-5">
        <button
          type="button"
          onClick={() => setBlacklistOpen(!blacklistOpen)}
          class="admin-disclosure-trigger"
          aria-expanded={blacklistOpen}
        >
          <ChevronIcon class={`admin-disclosure-chevron ${blacklistOpen ? 'is-open' : ''}`} />
          <span class="admin-disclosure-heading">
            <strong>Blacklist</strong>
            <small>
              Blocked even when allowlisted or arriving through follow sync — the
              one list that always wins.
            </small>
          </span>
          <span class={`admin-disclosure-count ${blacklist.length > 0 ? 'is-active' : ''}`}>
            {blacklist.length === 0 ? 'None' : `${blacklist.length} blocked`}
          </span>
        </button>

        {blacklistOpen && (
          <div class="admin-disclosure-body">

            {blacklistLoading ? (
              <div class="space-y-2">
                {[...Array(2)].map((_, i) => (
                  <div key={i} class="lc-skeleton h-14 w-full" />
                ))}
              </div>
            ) : blacklist.length === 0 ? (
              <AdminEmptyState icon={AccessIcon} headline="Nobody is blocked">
                A blocked pubkey is refused even when it is on the allowlist or
                arrives through follow sync, so this is the one list that always
                wins. Empty is the normal state.
              </AdminEmptyState>
            ) : (
              <div class="space-y-2">
                {blacklist.map(entry => {
                  const profile = blacklistProfiles.get(entry.hex)
                  return (
                    <div key={entry.hex} class="lc-card p-3 flex items-center justify-between" style={{ borderColor: 'rgba(239,68,68,0.2)' }}>
                      <div
                        class="flex items-center gap-3 cursor-pointer"
                        onClick={() => openProfile(entry.hex, entry.npub)}
                      >
                        {profile?.picture ? (
                          <img src={profile.picture} alt="" class="w-8 h-8 rounded-full object-cover flex-shrink-0"
                            style={{ border: '1px solid var(--color-border)', opacity: 0.6 }}
                            onError={(e) => { (e.target as HTMLImageElement).style.display = 'none' }} />
                        ) : (
                          <div class="w-8 h-8 rounded-full flex-shrink-0 flex items-center justify-center text-xs font-bold"
                            style={{ background: 'rgba(239,68,68,0.1)', color: '#f87171' }}>
                            {(profile?.name || entry.npub.slice(5, 7) || '??').slice(0, 2).toUpperCase()}
                          </div>
                        )}
                        <div>
                          <div class="text-sm font-medium">{getDisplayName(profile, entry.npub)}</div>
                          <div class="flex items-center text-xs font-mono" style={{ color: 'var(--color-text-secondary)' }}>
                            <span>{truncate(entry.npub)}</span>
                            <CopyNpubButton npub={entry.npub} />
                          </div>
                        </div>
                      </div>
                      <div>
                        {confirmBlacklistRemove === entry.hex ? (
                          <span class="space-x-2">
                            <button onClick={() => handleBlacklistRemove(entry.hex)} class="text-sm text-green-400 hover:text-green-300 transition-colors">Unblock</button>
                            <button onClick={() => setConfirmBlacklistRemove(null)} class="text-sm transition-colors" style={{ color: 'var(--color-text-secondary)' }}>Cancel</button>
                          </span>
                        ) : (
                          <button onClick={() => setConfirmBlacklistRemove(entry.hex)} class="text-sm text-green-400 hover:text-green-300 transition-colors">Unblock</button>
                        )}
                      </div>
                    </div>
                  )
                })}
              </div>
            )}
          </div>
        )}
      </section>

      {/* Profile Card Modal */}
      {selectedProfile && (
        <ProfileCard
          profile={selectedProfile.profile}
          hex={selectedProfile.hex}
          npub={selectedProfile.npub}
          onClose={() => setSelectedProfile(null)}
        />
      )}
    </div>
  )
}
