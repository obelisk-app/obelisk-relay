import { useState, useEffect, useRef } from 'preact/hooks'
import { adminApi } from '../../services/AdminApiClient'
import { fetchProfiles, getDisplayName, type NostrProfile } from '../../services/ProfileFetcher'
import { ProfileCard, CopyNpubButton } from './ProfileCard'
import { resolvePubkeyInput } from '../../services/nip05'
import { AdminEmptyState } from './AdminEmptyState'

interface RefAccount {
  hex: string
  npub: string
}

export const ReferenceAccountsManager = () => {
  const [accounts, setAccounts] = useState<RefAccount[]>([])
  const [profiles, setProfiles] = useState<Map<string, NostrProfile>>(new Map())
  const [newPubkey, setNewPubkey] = useState('')
  const [error, setError] = useState<string | null>(null)
  const [toast, setToast] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  const [syncing, setSyncing] = useState(false)
  const [autoSyncing, setAutoSyncing] = useState(false)
  const [syncResult, setSyncResult] = useState<string | null>(null)
  const [confirmRemove, setConfirmRemove] = useState<string | null>(null)
  const [selectedProfile, setSelectedProfile] = useState<{ hex: string; npub: string; profile?: NostrProfile } | null>(null)
  const autoSyncTimer = useRef<ReturnType<typeof setTimeout> | null>(null)

  const showToast = (msg: string) => {
    setToast(msg)
    setTimeout(() => setToast(null), 4000)
  }

  const fetchAccounts = () => {
    setLoading(true)
    adminApi.getReferenceAccounts()
      .then(data => { setAccounts(data); setError(null); loadProfiles(data) })
      .catch(e => setError(e.message))
      .finally(() => setLoading(false))
  }

  const loadProfiles = async (data: RefAccount[]) => {
    if (data.length === 0) return
    try {
      const profs = await fetchProfiles(data.map(e => e.hex))
      setProfiles(profs)
    } catch {
      // Profiles are optional
    }
  }

  useEffect(() => {
    fetchAccounts()
    return () => {
      if (autoSyncTimer.current) clearTimeout(autoSyncTimer.current)
    }
  }, [])

  const handleAdd = async () => {
    if (!newPubkey.trim()) return
    setError(null)
    try {
      // Accepts npub, hex, or a NIP-05 address, same as the Access field.
      const entry = await adminApi.addReferenceAccount(await resolvePubkeyInput(newPubkey))
      setAccounts(prev => [...prev.filter(e => e.hex !== entry.hex), entry])
      setNewPubkey('')
      showToast('Reference account added — syncing follows...')
      fetchProfiles([entry.hex]).then(profs => {
        setProfiles(prev => {
          const next = new Map(prev)
          const p = profs.get(entry.hex)
          if (p) next.set(entry.hex, p)
          return next
        })
      })

      // Show auto-syncing indicator and poll for updated stats
      setAutoSyncing(true)
      autoSyncTimer.current = setTimeout(async () => {
        try {
          const stats = await adminApi.getStats()
          setSyncResult(`Auto-sync complete: ${stats.whitelisted_count} total whitelisted pubkeys`)
        } catch {
          // ignore
        }
        setAutoSyncing(false)
      }, 20000) // Follow sync typically takes ~15-20s
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to add')
    }
  }

  const handleRemove = async (hex: string) => {
    setError(null)
    try {
      await adminApi.removeReferenceAccount(hex)
      setAccounts(prev => prev.filter(e => e.hex !== hex))
      setConfirmRemove(null)
      showToast('Reference account removed')
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to remove')
    }
  }

  const handleSync = async () => {
    setSyncing(true)
    setSyncResult(null)
    setError(null)
    try {
      const result = await adminApi.syncFollows()
      setSyncResult(result.message)
      showToast(`Sync complete: ${result.derived_count} follows whitelisted`)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Sync failed')
    } finally {
      setSyncing(false)
    }
  }

  const truncate = (s: string) => s.length > 16 ? `${s.slice(0, 8)}...${s.slice(-8)}` : s

  const openProfile = (hex: string, npub: string) => {
    const profile = profiles.get(hex)
    setSelectedProfile({ hex, npub, profile })
  }

  return (
    <section class="admin-settings-card">
      {/* A named card with its own header, because this is no longer a screen.
          It had neither: the heading lived in the tab chrome of the References
          tab, so once these controls moved inside Tier 1 the most important
          setting on the relay rendered as an unlabelled button and a paragraph
          — you could not tell what it configured, or that it was here at all.

          These accounts do two jobs, and the second is easy to miss: they are
          the follow-sync sources AND the roots of the Web-of-Trust graph, so
          adding one here widens Tier 1, Tier 2 and Tier 3 at once. */}
      <div class="admin-settings-card-header">
        <div>
          <h3>Reference accounts</h3>
          <p>
            The accounts every tier is measured from. Everyone they follow lands in
            Tier 1, and the follow graph is walked outward from here to fill Tier 2
            and Tier 3 — so adding one widens all three. The rules that use them,
            the hop limit and the rate budget, are under Policies.
          </p>
        </div>
        <div class="admin-row-actions">
          <span class="admin-status-badge">{accounts.length}</span>
          <button
            onClick={handleSync}
            disabled={syncing || accounts.length === 0}
            class="admin-action-btn"
            title="Re-reads each reference account's follow list and refreshes the follow-derived part of Tier 1."
          >
            {syncing ? (
              <>
                <span class="lc-spinner" style={{ width: '14px', height: '14px', borderTopColor: '#0a0a0a', borderWidth: '2px' }} />
                Syncing…
              </>
            ) : (
              'Sync follows'
            )}
          </button>
        </div>
      </div>

      {toast && (
        <div class="mb-4 p-3 rounded-lg text-sm border" style={{ background: 'rgba(180,249,83,0.08)', color: '#b4f953', borderColor: 'rgba(180,249,83,0.2)' }}>
          {toast}
        </div>
      )}

      {autoSyncing && (
        <div class="mb-4 p-3 rounded-lg text-sm border flex items-center gap-2" style={{ background: 'rgba(180,249,83,0.05)', color: 'var(--color-text-secondary)', borderColor: 'var(--color-border)' }}>
          <span class="lc-spinner" style={{ width: '14px', height: '14px', borderTopColor: '#b4f953', borderWidth: '2px' }} />
          Syncing follows in background...
        </div>
      )}

      {syncResult && !toast && !autoSyncing && (
        <div class="mb-4 p-3 rounded-lg text-sm border" style={{ background: 'rgba(180,249,83,0.05)', color: 'var(--color-text-secondary)', borderColor: 'var(--color-border)' }}>
          {syncResult}
        </div>
      )}

      {error && (
        <div class="mb-4 p-3 rounded-lg text-sm bg-red-500/10 text-red-400 border border-red-500/20">
          {error}
        </div>
      )}

      {/* Add form */}
      <div class="admin-search-row mb-4">
        <div class="admin-search-field">
          <input
            type="text"
            value={newPubkey}
            onInput={(e) => setNewPubkey((e.target as HTMLInputElement).value)}
            placeholder="npub1…, hex, or name@domain.com"
            class="admin-search-input"
            aria-label="Reference account to add"
            onKeyDown={(e) => e.key === 'Enter' && handleAdd()}
          />
        </div>
        <button onClick={handleAdd} disabled={!newPubkey.trim()} class="admin-action-btn">
          Add
        </button>
      </div>

      {/* Accounts List */}
      {loading ? (
        <div class="space-y-2">
          {[...Array(2)].map((_, i) => (
            <div key={i} class="lc-skeleton h-16 w-full" />
          ))}
        </div>
      ) : accounts.length === 0 ? (
        <AdminEmptyState headline="No reference accounts">
          Nothing seeds the tiers yet: with no roots, follow sync has nobody to read
          and the follow graph has nowhere to start, so Tier 2 and Tier 3 are empty
          however the Policies are set. Add an account above and sync.
        </AdminEmptyState>
      ) : (
        <div class="space-y-2">
          {accounts.map(account => {
            const profile = profiles.get(account.hex)
            return (
              <div key={account.hex} class="lc-card p-4 flex items-center justify-between" style={{ cursor: 'default' }}>
                <div
                  class="flex items-center gap-3 cursor-pointer"
                  onClick={() => openProfile(account.hex, account.npub)}
                >
                  {profile?.picture ? (
                    <img src={profile.picture} alt="" class="w-10 h-10 rounded-full object-cover flex-shrink-0"
                      style={{ border: '2px solid rgba(180,249,83,0.2)' }}
                      onError={(e) => { (e.target as HTMLImageElement).style.display = 'none' }} />
                  ) : (
                    <div class="w-10 h-10 rounded-full flex-shrink-0 flex items-center justify-center text-sm font-bold"
                      style={{ background: 'rgba(180,249,83,0.1)', color: '#b4f953', border: '2px solid rgba(180,249,83,0.2)' }}>
                      {(profile?.name || account.npub.slice(5, 7) || '??').slice(0, 2).toUpperCase()}
                    </div>
                  )}
                  <div>
                    <div class="font-medium">{getDisplayName(profile, account.npub)}</div>
                    <div class="flex items-center text-xs font-mono" style={{ color: 'var(--color-text-secondary)' }}>
                      <span>{truncate(account.npub)}</span>
                      <CopyNpubButton npub={account.npub} />
                    </div>
                  </div>
                </div>
                <div>
                  {confirmRemove === account.hex ? (
                    <span class="space-x-2">
                      <button
                        onClick={() => handleRemove(account.hex)}
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
                      onClick={() => setConfirmRemove(account.hex)}
                      class="text-sm text-red-400 hover:text-red-300 transition-colors"
                    >
                      Remove
                    </button>
                  )}
                </div>
              </div>
            )
          })}
        </div>
      )}

      {/* Profile Card Modal */}
      {selectedProfile && (
        <ProfileCard
          profile={selectedProfile.profile}
          hex={selectedProfile.hex}
          npub={selectedProfile.npub}
          onClose={() => setSelectedProfile(null)}
        />
      )}
    </section>
  )
}
