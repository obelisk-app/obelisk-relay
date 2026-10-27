import { useEffect, useState } from 'preact/hooks'
import {
  adminApi,
  type InviteMinter,
  type InviteReferral,
  type RelayInvite,
} from '../../../services/AdminApiClient'
import { fetchProfiles, getDisplayName, type NostrProfile } from '../../../services/ProfileFetcher'
import { resolvePubkeyInput } from '../../../services/nip05'
import { AdminEmptyState } from '../AdminEmptyState'
import { AccountRow } from '../AccountRow'

interface InvitesPanelProps {
  onInspect: (v: { hex: string; npub: string }) => void
  onChanged: () => void
}

type View = 'codes' | 'minters' | 'referrals'

const DAY = 86_400

/**
 * Where the clients accept an invite link. The relay itself does not care which
 * app redeems the code -- any NIP-43 client can -- these are only the links the
 * console offers to copy. `relay` is the host this console is served from, so
 * the app knows which relay the code belongs to.
 */
const INVITE_APPS = [
  { id: 'dex', label: 'Obelisk', base: 'https://obelisk.ar/invite/' },
  { id: 'sky', label: 'Sky', base: 'https://sky.obelisk.ar/invite/' },
] as const

const inviteLink = (base: string, code: string) =>
  `${base}${encodeURIComponent(code)}?relay=${encodeURIComponent(window.location.host)}`

const formatDate = (secs: number | null) =>
  secs == null ? 'never' : new Date(secs * 1000).toLocaleDateString()

const usage = (i: RelayInvite) => `${i.uses} / ${i.max_uses == null ? '∞' : i.max_uses}`

const STATUS_BADGE: Record<RelayInvite['status'], string> = {
  active: 'admin-status-badge-ok',
  exhausted: 'admin-status-badge-warn',
  expired: 'admin-status-badge-warn',
  revoked: 'admin-status-badge-danger',
}

/**
 * Copy, and say whether it worked. The Clipboard API is refused outside a
 * secure context and by some browser policies, and used to fail silently
 * here, so the button looked dead; fall back to a selected textarea.
 */
const copyText = async (text: string): Promise<boolean> => {
  try {
    await navigator.clipboard.writeText(text)
    return true
  } catch {
    const area = document.createElement('textarea')
    area.value = text
    area.setAttribute('readonly', '')
    area.style.position = 'fixed'
    area.style.opacity = '0'
    document.body.appendChild(area)
    area.select()
    let ok = false
    try {
      ok = document.execCommand('copy')
    } catch {
      ok = false
    }
    area.remove()
    return ok
  }
}

/**
 * Relay invitation codes: the front door for people with a brand-new key, who
 * are on no list and in no follow graph and so would otherwise be refused
 * everything.
 *
 * Three views of one store. Codes is what exists and how much of it is used.
 * Minters is who besides admins may create codes, and how many. Referrals is
 * the answer to "where did all these people come from" -- every invitee,
 * grouped under whoever handed them the code.
 */
export const InvitesPanel = ({ onInspect, onChanged }: InvitesPanelProps) => {
  const [view, setView] = useState<View>('codes')
  const [invites, setInvites] = useState<RelayInvite[]>([])
  const [minters, setMinters] = useState<InviteMinter[]>([])
  const [referrals, setReferrals] = useState<InviteReferral[]>([])
  const [profiles, setProfiles] = useState<Map<string, NostrProfile>>(new Map())
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  const [confirming, setConfirming] = useState<string | null>(null)
  const [created, setCreated] = useState<RelayInvite | null>(null)
  // Which copy button just worked (or failed), so it can say so for a moment.
  const [copied, setCopied] = useState<{ key: string; ok: boolean } | null>(null)

  const copy = async (key: string, text: string) => {
    const ok = await copyText(text)
    setCopied({ key, ok })
    setTimeout(() => setCopied(c => (c?.key === key ? null : c)), 2000)
  }

  const copyLabel = (key: string, label: string) =>
    copied?.key === key ? (copied.ok ? 'Copied ✓' : 'Copy failed') : label

  const [codeForm, setCodeForm] = useState({ label: '', max_uses: 25, days: 30 })
  const [grantForm, setGrantForm] = useState({
    pubkey: '',
    max_codes: 5,
    max_uses_per_code: 10,
    max_total_admits: 25,
    max_ttl_days: 30,
  })

  const load = async () => {
    setLoading(true)
    setError(null)
    try {
      const [list, mintRows, tree] = await Promise.all([
        adminApi.getRelayInvites(),
        adminApi.getInviteMinters(),
        adminApi.getInviteReferrals(),
      ])
      setInvites(list.invites)
      setMinters(mintRows)
      setReferrals(tree)
      const keys = new Set<string>()
      for (const i of list.invites) {
        keys.add(i.created_by)
        i.redemptions.forEach(r => keys.add(r.pubkey))
      }
      mintRows.forEach(m => keys.add(m.pubkey))
      fetchProfiles([...keys])
        .then(found => setProfiles(prev => new Map([...prev, ...found])))
        .catch(() => undefined)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to load invite codes')
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    void load()
  }, [])

  const run = async (key: string, action: () => Promise<unknown>, failure: string) => {
    setBusy(key)
    setError(null)
    try {
      await action()
      setConfirming(null)
      await load()
      onChanged()
    } catch (err) {
      setError(err instanceof Error ? err.message : failure)
    } finally {
      setBusy(null)
    }
  }

  const createCode = (e: Event) => {
    e.preventDefault()
    void run(
      'create',
      async () => {
        const invite = await adminApi.createRelayInvite({
          label: codeForm.label.trim() || undefined,
          max_uses: Math.max(0, codeForm.max_uses),
          expires_in: Math.max(0, codeForm.days) * DAY,
        })
        setCreated(invite)
        setCodeForm({ ...codeForm, label: '' })
      },
      'Could not create the code',
    )
  }

  const grant = (e: Event) => {
    e.preventDefault()
    const raw = grantForm.pubkey.trim()
    if (!raw) return
    void run(
      'grant',
      async () => {
        const pubkey = await resolvePubkeyInput(raw)
        await adminApi.grantInviteMinter({ ...grantForm, pubkey })
        setGrantForm({ ...grantForm, pubkey: '' })
      },
      'Could not grant minting rights',
    )
  }

  const nameOf = (hex: string, npub: string) => {
    const p = profiles.get(hex)
    return p ? getDisplayName(p, npub) : `${npub.slice(0, 12)}…`
  }

  const numberField = (
    label: string,
    value: number,
    onChange: (n: number) => void,
    hint?: string,
  ) => (
    <label title={hint}>
      <span>{label}</span>
      <input
        type="number"
        min="0"
        value={value}
        onInput={e => onChange(Number((e.target as HTMLInputElement).value))}
      />
    </label>
  )

  const linkButtons = (code: string) => (
    <>
      <button
        class="admin-action-btn admin-action-btn-secondary"
        onClick={() => void copy(`${code}:code`, code)}
        aria-live="polite"
      >
        {copyLabel(`${code}:code`, 'Copy code')}
      </button>
      {INVITE_APPS.map(app => (
        <button
          key={app.id}
          class={`admin-action-btn ${copied?.key === `${code}:${app.id}` && copied.ok ? '' : 'admin-action-btn-secondary'}`}
          onClick={() => void copy(`${code}:${app.id}`, inviteLink(app.base, code))}
          title={inviteLink(app.base, code)}
          aria-live="polite"
        >
          {copyLabel(`${code}:${app.id}`, `Copy ${app.label} link`)}
        </button>
      ))}
    </>
  )

  const codeRow = (i: RelayInvite) => (
    <div key={i.code} class="admin-settings-card mt-3">
      <div class="admin-settings-card-header">
        <div>
          <h3 style={{ fontFamily: 'monospace' }}>{i.code}</h3>
          <p>
            {i.label ? `${i.label} · ` : ''}
            by {i.created_by_admin ? 'admin ' : ''}
            {nameOf(i.created_by, i.created_by_npub)} · {usage(i)} used · expires{' '}
            {formatDate(i.expires_at)}
          </p>
        </div>
        <span class={`admin-status-badge ${STATUS_BADGE[i.status]}`}>{i.status}</span>
      </div>
      <div class="flex flex-wrap gap-2 mt-2">
        {i.status === 'active' && linkButtons(i.code)}
        {i.revoked_at == null &&
          (confirming === `revoke:${i.code}` ? (
            <>
              <button
                class="admin-action-btn admin-action-btn-secondary"
                disabled={busy !== null}
                onClick={() =>
                  void run(i.code, () => adminApi.revokeRelayInvite(i.code, false), 'Could not revoke')
                }
              >
                Revoke, keep invitees
              </button>
              <button
                class="admin-action-btn"
                disabled={busy !== null}
                onClick={() =>
                  void run(i.code, () => adminApi.revokeRelayInvite(i.code, true), 'Could not revoke')
                }
                title="Also removes invite admission from everyone who used this code."
              >
                Revoke and remove {i.uses} invitee{i.uses === 1 ? '' : 's'}
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
              onClick={() => setConfirming(`revoke:${i.code}`)}
            >
              Revoke
            </button>
          ))}
      </div>
    </div>
  )

  const views: { id: View; label: string; count: number }[] = [
    { id: 'codes', label: 'Codes', count: invites.filter(i => i.status === 'active').length },
    { id: 'minters', label: 'Minters', count: minters.length },
    {
      id: 'referrals',
      label: 'Referrals',
      count: referrals.reduce((n, r) => n + r.total_admitted, 0),
    },
  ]

  return (
    <section class="admin-settings-card">
      <div class="admin-settings-card-header">
        <div>
          <h3>Invites</h3>
          <p>
            Codes that admit a new key to this relay, whatever tier it would
            otherwise fall in. Each distinct pubkey that redeems a code uses it up
            once; the apps replay a stored code when someone makes a new key, so
            that costs a use too. Invitees publish at the Tier 1 budget.
          </p>
        </div>
      </div>

      <nav class="admin-tier-tabs mt-3" aria-label="Invite views">
        {views.map(v => (
          <button
            key={v.id}
            class={`admin-tier-tab ${view === v.id ? 'is-active' : ''}`}
            onClick={() => setView(v.id)}
            aria-pressed={view === v.id}
          >
            <span class="admin-tier-tab-label">{v.label}</span>
            <span class="admin-tier-tab-count">{v.count.toLocaleString()}</span>
          </button>
        ))}
      </nav>

      {error && (
        <p class="admin-access-hint mt-3" role="alert">{error}</p>
      )}

      {loading && invites.length === 0 && <div class="lc-skeleton h-24 w-full mt-3" />}

      {view === 'codes' && (
        <>
          <form onSubmit={createCode} class="mt-4">
            <div class="admin-rate-grid">
              <label style={{ gridColumn: 'span 2' }}>
                <span>Label (only you see it)</span>
                <input
                  type="text"
                  maxLength={80}
                  placeholder="Meetup in Buenos Aires"
                  value={codeForm.label}
                  onInput={e =>
                    setCodeForm({ ...codeForm, label: (e.target as HTMLInputElement).value })
                  }
                />
              </label>
              {numberField(
                'Max uses (0 = unlimited)',
                codeForm.max_uses,
                n => setCodeForm({ ...codeForm, max_uses: n }),
              )}
              {numberField(
                'Expires in days (0 = never)',
                codeForm.days,
                n => setCodeForm({ ...codeForm, days: n }),
              )}
            </div>
            <button class="admin-action-btn mt-3" type="submit" disabled={busy !== null}>
              {busy === 'create' ? 'Creating…' : 'Create invite code'}
            </button>
          </form>

          {created && (
            <div class="admin-settings-card mt-3">
              <div class="admin-settings-card-header">
                <div>
                  <h3 style={{ fontFamily: 'monospace' }}>{created.code}</h3>
                  <p>New code. Share a link, or the code for any NIP-43 client.</p>
                </div>
                <span class="admin-status-badge admin-status-badge-ok">new</span>
              </div>
              <div class="flex flex-wrap gap-2 mt-2">{linkButtons(created.code)}</div>
            </div>
          )}

          {!loading && invites.length === 0 && (
            <div class="mt-3">
              <AdminEmptyState headline="No invite codes yet">
                Create one above, or grant someone minting rights under Minters so
                they can invite people themselves.
              </AdminEmptyState>
            </div>
          )}
          {invites.map(codeRow)}
        </>
      )}

      {view === 'minters' && (
        <>
          <form onSubmit={grant} class="mt-4">
            <div class="admin-rate-grid">
              <label style={{ gridColumn: 'span 2' }}>
                <span>Who may invite</span>
                <input
                  type="text"
                  placeholder="npub1…, hex, or name@domain.com"
                  value={grantForm.pubkey}
                  onInput={e =>
                    setGrantForm({ ...grantForm, pubkey: (e.target as HTMLInputElement).value })
                  }
                />
              </label>
              {numberField(
                'Active codes at once',
                grantForm.max_codes,
                n => setGrantForm({ ...grantForm, max_codes: n }),
              )}
              {numberField(
                'Uses per code',
                grantForm.max_uses_per_code,
                n => setGrantForm({ ...grantForm, max_uses_per_code: n }),
              )}
              {numberField(
                'People in total',
                grantForm.max_total_admits,
                n => setGrantForm({ ...grantForm, max_total_admits: n }),
                'When this is spent, every code they made stops admitting.',
              )}
              {numberField(
                'Longest code life (days)',
                grantForm.max_ttl_days,
                n => setGrantForm({ ...grantForm, max_ttl_days: n }),
              )}
            </div>
            <button
              class="admin-action-btn mt-3"
              type="submit"
              disabled={busy !== null || !grantForm.pubkey.trim()}
            >
              {busy === 'grant' ? 'Saving…' : 'Grant or update'}
            </button>
            <p class="admin-access-hint">
              Minters see an “Invite people” option in Obelisk and Sky settings.
              Granting again to the same person updates their limits.
            </p>
          </form>

          {!loading && minters.length === 0 && (
            <div class="mt-3">
              <AdminEmptyState headline="Only admins can invite">
                Grant minting rights to let trusted people create their own codes,
                within the limits you set.
              </AdminEmptyState>
            </div>
          )}

          <div class="admin-account-list mt-3">
            {minters.map(m => (
              <AccountRow
                key={m.pubkey}
                hex={m.pubkey}
                npub={m.npub}
                profile={profiles.get(m.pubkey)}
                onInspect={() => onInspect({ hex: m.pubkey, npub: m.npub })}
                subtitle={`${m.active_codes}/${m.max_codes} codes · ${m.admits_used}/${m.max_total_admits} people · ${m.max_uses_per_code} per code · ${Math.round(m.max_ttl_secs / DAY)}d`}
                actions={
                  confirming === `minter:${m.pubkey}` ? (
                    <>
                      <button
                        class="admin-action-btn admin-action-btn-secondary"
                        disabled={busy !== null}
                        onClick={() =>
                          void run(
                            m.pubkey,
                            () => adminApi.revokeInviteMinter(m.pubkey),
                            'Could not withdraw the grant',
                          )
                        }
                      >
                        Confirm
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
                      onClick={() => setConfirming(`minter:${m.pubkey}`)}
                      title="Their codes stop admitting. People they already invited keep access."
                    >
                      Withdraw
                    </button>
                  )
                }
              />
            ))}
          </div>
        </>
      )}

      {view === 'referrals' && (
        <>
          {!loading && referrals.length === 0 && (
            <div class="mt-3">
              <AdminEmptyState headline="Nobody has come in on a code yet">
                Every redemption shows up here under the person whose code it was.
              </AdminEmptyState>
            </div>
          )}
          {referrals.map(branch => {
            const invitedBy = branch.invited_by
              ? referrals.find(r => r.inviter === branch.invited_by)
              : undefined
            return (
              <div key={branch.inviter} class="admin-settings-card mt-3">
                <div class="admin-settings-card-header">
                  <div>
                    <h3>{nameOf(branch.inviter, branch.inviter_npub)}</h3>
                    <p>
                      {branch.is_admin_code ? 'Admin' : branch.has_grant ? 'Minter' : 'Former minter'}
                      {' · '}
                      {branch.codes.length} code{branch.codes.length === 1 ? '' : 's'}
                      {branch.invited_by &&
                        ` · was invited by ${
                          invitedBy
                            ? nameOf(invitedBy.inviter, invitedBy.inviter_npub)
                            : `${branch.invited_by.slice(0, 8)}…`
                        }`}
                    </p>
                  </div>
                  <span class="admin-status-badge admin-status-badge-ok">
                    {branch.total_admitted} brought in
                  </span>
                </div>
                <div class="admin-account-list mt-2">
                  {branch.codes.flatMap(code =>
                    code.redemptions.map(r => (
                      <AccountRow
                        key={`${code.code}:${r.pubkey}`}
                        hex={r.pubkey}
                        npub={r.npub}
                        profile={profiles.get(r.pubkey)}
                        onInspect={() => onInspect({ hex: r.pubkey, npub: r.npub })}
                        subtitle={`${r.evicted_at ? 'removed · ' : ''}via ${code.label ?? code.code.slice(0, 8) + '…'} · ${formatDate(r.at)}`}
                        actions={
                          r.evicted_at ? undefined : confirming === `evict:${r.pubkey}` ? (
                            <>
                              <button
                                class="admin-action-btn admin-action-btn-secondary"
                                disabled={busy !== null}
                                onClick={() =>
                                  void run(
                                    r.pubkey,
                                    () => adminApi.evictInvitee(r.pubkey),
                                    'Could not remove that invitee',
                                  )
                                }
                              >
                                Confirm
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
                              onClick={() => setConfirming(`evict:${r.pubkey}`)}
                              title="Removes their invite admission. Another tier may still admit them; block them to make it final."
                            >
                              Remove
                            </button>
                          )
                        }
                      />
                    )),
                  )}
                </div>
              </div>
            )
          })}
        </>
      )}
    </section>
  )
}
