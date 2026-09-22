import { useState, useEffect } from 'preact/hooks'
import {
  adminApi,
  type AccessSettings,
  type AccessSources,
  type WotConfigured,
  type WotStatus,
} from '../../../services/AdminApiClient'
import { useDirtySection } from '../settingsDirty'

/**
 * The rules, not the people.
 *
 * This is what is left of `WhitelistManager` after the tier panels took over
 * the lists. Everything it used to render below the settings — the whitelist
 * table, the blacklist disclosure, the allow/block forms, the "who can connect"
 * stat cards, the check-a-pubkey card, and the 500-chip grid of admitted
 * accounts — said the same thing as Tier 1 / Tier 2 / Tier 3 / Blocked and the
 * search bar above them, in a second shape. Two shapes for one fact is the
 * confusion this rework set out to remove, so only the settings remain:
 * whether admission is enforced, the rate budget, and how the graph is built.
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

/**
 * Budget rungs in the console's tier language.
 *
 * The backend names each rung after the mechanism that produced it — "added by
 * hand", "followed by a reference account", "2 hops away". Those are accurate
 * and were also the reason the screen read as a list of unrelated rules: two of
 * them are the same people on the same budget, and none of them used the tier
 * numbers every other panel is now labelled with. Keyed on the backend's own
 * budget key so a new rung shows up rather than being silently dropped.
 */
const BUDGET_ROWS: Record<string, { tier: string; who: string }> = {
  manual: {
    tier: 'Tier 1',
    who: 'Added by hand, the reference accounts, and everyone they follow',
  },
  // Same row as `manual` on purpose: both sit at the full budget, and the
  // backend already folds follow sync and one hop into one bucket.
  direct: {
    tier: 'Tier 1',
    who: 'Added by hand, the reference accounts, and everyone they follow',
  },
  wot_mid: { tier: 'Tier 2', who: 'Two hops away in the follow graph' },
  wot_far: { tier: 'Tier 3', who: 'Three hops away in the follow graph' },
  open: { tier: 'No tier', who: 'Anyone at all, while admission is not enforced' },
}

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

export const PoliciesPanel = () => {
  const [error, setError] = useState<string | null>(null)
  const [toast, setToast] = useState<string | null>(null)
  const [accessSettings, setAccessSettings] = useState<AccessSettings | null>(null)
  const [accessLoading, setAccessLoading] = useState(true)
  const [savingAccess, setSavingAccess] = useState(false)
  // Last-saved copy, so the save bar can tell an edit from a reload.
  const [accessBaseline, setAccessBaseline] = useState<AccessSettings | null>(null)
  // Only the budget ladder is read from this now; the per-tier counts it also
  // carries are the tier tabs' job.
  const [sources, setSources] = useState<AccessSources | null>(null)

  const [wot, setWot] = useState<WotStatus | null>(null)
  const [wotForm, setWotForm] = useState<WotConfigured | null>(null)

  const showToast = (msg: string) => {
    setToast(msg)
    setTimeout(() => setToast(null), 4000)
  }

  const loadWot = () => {
    adminApi
      .getWotStatus()
      .then(status => {
        setWot(status)
        setWotForm(status.configured)
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

  const fetchAccessSettings = () => {
    setAccessLoading(true)
    adminApi
      .getAccessSettings()
      .then(settings => {
        setAccessSettings(settings)
        setAccessBaseline(settings)
      })
      .catch(e => setError(e.message))
      .finally(() => setAccessLoading(false))
  }

  useEffect(() => {
    fetchAccessSettings()
    adminApi.getAccessSources().then(setSources).catch(() => undefined)
    loadWot()
  }, [])

  const updateAccessField = (
    field: keyof AccessSettings,
    value: AccessSettings[keyof AccessSettings],
  ) => {
    setAccessSettings(prev => (prev ? { ...prev, [field]: value } : prev))
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
      showToast(
        next.restart_required
          ? 'Access settings saved. Restart relay to apply rate-limit changes.'
          : 'Access settings saved',
      )
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
        accessSettings.pubkey_rate_limit_per_minute !==
          accessBaseline.pubkey_rate_limit_per_minute ||
        accessSettings.connection_rate_limit_per_minute !==
          accessBaseline.connection_rate_limit_per_minute ||
        accessSettings.global_rate_limit_per_minute !==
          accessBaseline.global_rate_limit_per_minute),
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
      consequence:
        accessDirty &&
        accessSettings?.access_policy === 'open' &&
        accessBaseline?.access_policy !== 'open' &&
        (sources?.manual ?? 0) > 0
          ? `Saving open mode clears all ${sources?.manual} hand-added Tier 1 entr${
              sources?.manual === 1 ? 'y' : 'ies'
            }.`
          : null,
    },
    {
      save: handleSaveAccess,
      discard: () => setAccessSettings(accessBaseline),
    },
  )

  // One row per tier. Rungs that share a tier share a row -- "added by hand"
  // and "followed by a reference account" are the same people on the same
  // budget, and listing both read as a duplicate.
  const ladder = (() => {
    if (!sources) return []
    const rows: { tier: string; who: string; percent: number; perMinute: number }[] = []
    for (const rung of sources.budget_ladder) {
      const row = BUDGET_ROWS[rung.tier]
      if (!row) continue
      if (rows.some(r => r.tier === row.tier)) continue
      rows.push({
        tier: row.tier,
        who: row.who,
        percent: rung.percent,
        perMinute: rung.events_per_minute,
      })
    }
    return rows.sort((a, b) => b.percent - a.percent)
  })()

  return (
    <div>
      {toast && (
        <div
          class="mb-4 p-3 rounded-lg text-sm border"
          style={{
            background: 'rgba(var(--color-accent-rgb), 0.08)',
            color: 'var(--color-accent)',
            borderColor: 'rgba(var(--color-accent-rgb), 0.2)',
          }}
        >
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
            <h3>Relay access mode</h3>
            <p>
              Whether the tiers are enforced at all. Open access requires rate limits,
              because they become the only brake.
            </p>
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
                  Only Tier 1, 2 and 3: you, anyone added by hand, everyone your
                  reference accounts follow, and — if enabled — anyone close enough
                  in the follow graph.
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
                  {RATE_TIERS.map(t => (
                    <span key={t}>{t}</span>
                  ))}
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

              {ladder.length > 0 && (
                <div class="admin-budget-ladder">
                  <div class="admin-budget-ladder-head">
                    What each tier may publish
                    <small>
                      The per-pubkey slider above sets Tier 1's budget. Every other
                      tier gets a share of it: the further from your reference
                      accounts someone is, the less evidence there is that they
                      should be trusted with your disk.
                    </small>
                  </div>
                  {ladder.map(row => (
                    <div key={row.tier} class="admin-budget-rung">
                      <span class="admin-budget-rung-label">
                        <strong>{row.tier}</strong>
                        <small>{row.who}</small>
                      </span>
                      <span class="admin-budget-rung-bar">
                        <span style={{ width: `${row.percent}%` }} />
                      </span>
                      <span class="admin-budget-rung-value">
                        {row.perMinute.toLocaleString()}/min
                      </span>
                    </div>
                  ))}
                </div>
              )}
            </div>

            {savingAccess && (
              <div class="admin-settings-actions">
                <div class="text-sm" style={{ color: 'var(--color-text-secondary)' }}>
                  Saving…
                </div>
              </div>
            )}
          </>
        )}
      </section>

      {wot && wotForm && (
        <section class="admin-settings-card mb-5">
          <div class="admin-settings-card-header">
            <div>
              <h3>Web of Trust admission</h3>
              <p>
                What fills Tier 2 and Tier 3. Admits anyone within a few follow-graph
                hops of your reference accounts, without adding them by hand. Blocked
                accounts still override it, and an unreachable oracle admits nobody
                rather than everybody.
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
              onChange={e =>
                setWotForm({ ...wotForm, enabled: (e.target as HTMLInputElement).checked })
              }
            />
            <span>
              <strong>Admit pubkeys inside the web of trust</strong>
              <small>
                Applied at startup, so this takes effect on the next relay restart.
                Leave off and only Tier 1 decides.
              </small>
            </span>
          </label>

          {wotForm.enabled && (
            <>
              <label class="admin-toggle-row">
                <input
                  type="checkbox"
                  checked={wotForm.local}
                  onChange={e =>
                    setWotForm({ ...wotForm, local: (e.target as HTMLInputElement).checked })
                  }
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
                    onInput={e =>
                      setWotForm({
                        ...wotForm,
                        max_hops: Number((e.target as HTMLInputElement).value),
                      })
                    }
                  />
                </label>
                {!wotForm.local && (
                  <label style={{ gridColumn: 'span 2' }}>
                    <span>Oracle URL</span>
                    <input
                      type="text"
                      placeholder="http://wot-oracle:8080"
                      value={wotForm.oracle_url}
                      onInput={e =>
                        setWotForm({
                          ...wotForm,
                          oracle_url: (e.target as HTMLInputElement).value,
                        })
                      }
                    />
                  </label>
                )}
              </div>
              <p class="text-xs mt-2" style={{ color: 'var(--color-text-secondary)' }}>
                1 hop is Tier 1, which you already have; past 3 is noise. Graph roots
                are the reference accounts listed under Tier 1.
                {wot.local && wot.graph_accounts > 0 && (
                  <>
                    {' '}
                    Graph currently knows {wot.graph_accounts.toLocaleString()} follow
                    list{wot.graph_accounts === 1 ? '' : 's'} and{' '}
                    {wot.graph_edges.toLocaleString()} follow edges.
                  </>
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
              {wot.graph_complete_to_hop === 1 ? '' : 's'}: fetching every contact list
              needed for {wot.max_hops} would take tens of thousands of requests. Past{' '}
              {wot.graph_complete_to_hop} hops a refusal means the path was never
              fetched, not that it does not exist — which is why people you expect to be
              admitted are not. Set max hops to {wot.graph_complete_to_hop} for an
              answer that is actually complete, or add more reference accounts under
              Tier 1 to widen the graph.
            </p>
          )}
        </section>
      )}
    </div>
  )
}
