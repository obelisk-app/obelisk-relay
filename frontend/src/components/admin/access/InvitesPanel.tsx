import { useEffect, useState } from 'preact/hooks'
import {
  adminApi,
  type InviteAdmission,
  type InviteBasis,
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

type View = 'entries' | 'codes' | 'minters' | 'referrals'

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

/** The automatic allowance in the apps; keep in step with `DEFAULT_INVITE_ALLOWANCE`. */
const DEFAULT_ALLOWANCE = 10

const BASIS_LABEL: Record<InviteBasis, string> = {
  owner: 'Owner',
  admin: 'Admin',
  reference: 'Reference account',
  grant: 'Granted',
}

/** How an inviter's role reads on a referral or an entry. */
const inviterRole = (basis: InviteBasis | null, adminCode: boolean) =>
  adminCode ? 'Admin (console code)' : basis ? BASIS_LABEL[basis] : 'Can no longer invite'

const shortNpub = (npub: string) => `${npub.slice(0, 12)}…${npub.slice(-6)}`

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
 * Four views of one store. Entries is the log: every npub that came in, and
 * whose code let them in. Codes is what exists and how much of it is used.
 * Minters is everyone who may create links from the apps -- the owner, admins
 * and reference accounts on the automatic allowance, plus explicit grants.
 * Referrals is the same people as Entries, grouped under each inviter.
 */
export const InvitesPanel = ({ onInspect, onChanged }: InvitesPanelProps) => {
  const [view, setView] = useState<View>('entries')
  const [admissions, setAdmissions] = useState<InviteAdmission[]>([])
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

  // "Add people" amount per minter row, keyed by hex.
  const [extra, setExtra] = useState<Record<string, number>>({})

  const addPeople = (m: InviteMinter) => {
    const n = extra[m.pubkey] ?? DEFAULT_ALLOWANCE
    if (n <= 0) return
    void run(
      `extra:${m.pubkey}`,
      () => adminApi.grantInviteMinter({ pubkey: m.pubkey, add_admits: n }),
      'Could not add more invites',
    )
  }

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
      const [list, mintRows, tree, entries] = await Promise.all([
        adminApi.getRelayInvites(),
        adminApi.getInviteMinters(),
        adminApi.getInviteReferrals(),
        adminApi.getInviteAdmissions(),
      ])
      setInvites(list.invites)
      setMinters(mintRows)
      setReferrals(tree)
      setAdmissions(entries)
      const keys = new Set<string>()
      for (const i of list.invites) {
        keys.add(i.created_by)
        i.redemptions.forEach(r => keys.add(r.pubkey))
      }
      mintRows.forEach(m => keys.add(m.pubkey))
      tree.forEach(b => b.invited_by && keys.add(b.invited_by))
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

  /** Name of a pubkey we only know as hex (an inviter's own inviter). */
  const nameOfHex = (hex: string) => {
    const known =
      admissions.find(a => a.pubkey === hex)?.npub ??
      referrals.find(r => r.inviter === hex)?.inviter_npub
    return known ? nameOf(hex, known) : `${hex.slice(0, 8)}…`
  }

  const evictButton = (pubkey: string) =>
    confirming === `evict:${pubkey}` ? (
      <>
        <button
          class="admin-action-btn admin-action-btn-secondary"
          disabled={busy !== null}
          onClick={() =>
            void run(pubkey, () => adminApi.evictInvitee(pubkey), 'Could not remove that invitee')
          }
        >
          Confirm
        </button>
        <button class="admin-action-btn admin-action-btn-secondary" onClick={() => setConfirming(null)}>
          Cancel
        </button>
      </>
    ) : (
      <button
        class="admin-action-btn admin-action-btn-secondary"
        onClick={() => setConfirming(`evict:${pubkey}`)}
        title="Removes their invite admission. Another tier may still admit them; block them to make it final."
      >
        Remove
      </button>
    )

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
    { id: 'entries', label: 'Entries', count: admissions.filter(a => !a.evicted_at).length },
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
          <p>
            <strong>Who can invite from Obelisk and Sky:</strong> the owner, every
            admin and every reference account, {DEFAULT_ALLOWANCE} people each in
            total, split over as many links as they like. Anyone else needs a grant
            under Minters, which also replaces the automatic allowance for that
            person. Codes made here, in the console, have no allowance.
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

      {view === 'entries' && (
        <>
          {!loading && admissions.length === 0 && (
            <div class="mt-3">
              <AdminEmptyState headline="Nobody has come in on a code yet">
                Every key that redeems an invite link shows up here, with the
                account whose link it was.
              </AdminEmptyState>
            </div>
          )}
          <div class="admin-account-list mt-3">
            {admissions.map(a => (
              <AccountRow
                key={`${a.code}:${a.pubkey}`}
                hex={a.pubkey}
                npub={a.npub}
                profile={profiles.get(a.pubkey)}
                onInspect={() => onInspect({ hex: a.pubkey, npub: a.npub })}
                subtitle={[
                  a.evicted_at ? 'removed' : null,
                  shortNpub(a.npub),
                  `entered ${formatDate(a.at)}`,
                  `invited by ${nameOf(a.inviter, a.inviter_npub)} (${inviterRole(a.inviter_basis, a.inviter_admin_code)})`,
                  a.inviter_invited_by ? `who came in via ${nameOfHex(a.inviter_invited_by)}` : null,
                  `link ${a.code_label ?? a.code.slice(0, 8) + '…'}`,
                ]
                  .filter(Boolean)
                  .join(' · ')}
                actions={a.evicted_at ? undefined : evictButton(a.pubkey)}
              />
            ))}
          </div>
        </>
      )}

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
              Granting again to the same person updates their limits; “Add people”
              on a row raises their total by that many and keeps everything else. The owner,
              admins and reference accounts are listed without a grant: to stop
              one of them inviting, remove them from the reference accounts, or
              give them a grant with smaller limits.
            </p>
          </form>

          {!loading && minters.length === 0 && (
            <div class="mt-3">
              <AdminEmptyState headline="Nobody can invite from the apps yet">
                Add reference accounts, or grant minting rights to let trusted
                people create their own links within the limits you set.
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
                subtitle={`${BASIS_LABEL[m.basis]}${m.basis === 'grant' ? '' : ' · automatic'} · ${m.admits_used}/${m.max_total_admits} people · ${m.active_codes} live link${m.active_codes === 1 ? '' : 's'} · ${m.max_uses_per_code} per link · ${Math.round(m.max_ttl_secs / DAY)}d`}
                actions={
                  <>
                    <input
                      type="number"
                      min="1"
                      class="admin-inline-number"
                      style={{ width: '4.5rem' }}
                      aria-label="People to add"
                      title="How many more people they may invite, on top of what they have now"
                      value={extra[m.pubkey] ?? DEFAULT_ALLOWANCE}
                      onInput={e =>
                        setExtra({
                          ...extra,
                          [m.pubkey]: Number((e.target as HTMLInputElement).value),
                        })
                      }
                    />
                    <button
                      class="admin-action-btn"
                      disabled={busy !== null}
                      onClick={() => addPeople(m)}
                      title="Raise their total. Their links widen with it, so they can make one link for all of the extra people."
                    >
                      {busy === `extra:${m.pubkey}` ? 'Adding…' : 'Add people'}
                    </button>
                  {m.basis !== 'grant' ? null : confirming === `minter:${m.pubkey}` ? (
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
                      title={
                        m.automatic
                          ? `They go back to the automatic allowance of ${DEFAULT_ALLOWANCE} as ${BASIS_LABEL[m.automatic].toLowerCase()}.`
                          : 'Their codes stop admitting. People they already invited keep access.'
                      }
                    >
                      {m.automatic ? 'Reset to automatic' : 'Withdraw'}
                    </button>
                  )}
                  </>
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
                      {inviterRole(branch.basis, branch.is_admin_code)} · {shortNpub(branch.inviter_npub)}
                      {' · '}
                      {branch.codes.length} code{branch.codes.length === 1 ? '' : 's'}
                      {branch.invited_by &&
                        ` · was invited by ${
                          invitedBy
                            ? nameOf(invitedBy.inviter, invitedBy.inviter_npub)
                            : nameOfHex(branch.invited_by)
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
                        subtitle={`${r.evicted_at ? 'removed · ' : ''}${shortNpub(r.npub)} · via ${code.label ?? code.code.slice(0, 8) + '…'} · ${formatDate(r.at)}`}
                        actions={r.evicted_at ? undefined : evictButton(r.pubkey)}
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
