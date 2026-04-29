import { useState, useEffect } from 'preact/hooks'
import { adminApi } from '../../services/AdminApiClient'
import { fetchProfiles, getDisplayName, type NostrProfile } from '../../services/ProfileFetcher'
import { ProfileCard, CopyNpubButton } from './ProfileCard'

interface WhitelistEntry {
  hex: string
  npub: string
}

interface BlacklistEntry {
  hex: string
  npub: string
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

  // Blacklist state
  const [blacklist, setBlacklist] = useState<BlacklistEntry[]>([])
  const [blacklistProfiles, setBlacklistProfiles] = useState<Map<string, NostrProfile>>(new Map())
  const [newBlacklistPubkey, setNewBlacklistPubkey] = useState('')
  const [blacklistOpen, setBlacklistOpen] = useState(false)
  const [blacklistLoading, setBlacklistLoading] = useState(false)
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

  useEffect(() => { fetchWhitelist(); fetchBlacklist() }, [])

  const handleAdd = async () => {
    if (!newPubkey.trim()) return
    setError(null)
    try {
      const entry = await adminApi.addToWhitelist(newPubkey.trim())
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

  return (
    <div>
      <h2 class="text-xl font-bold mb-6">Whitelist Management</h2>

      {toast && (
        <div class="mb-4 p-3 rounded-lg text-sm border" style={{ background: 'rgba(180,249,83,0.08)', color: '#b4f953', borderColor: 'rgba(180,249,83,0.2)' }}>
          {toast}
        </div>
      )}

      {error && (
        <div class="mb-4 p-3 rounded-lg text-sm bg-red-500/10 text-red-400 border border-red-500/20">
          {error}
        </div>
      )}

      {/* Add form */}
      <div class="flex gap-2 mb-6">
        <input
          type="text"
          value={newPubkey}
          onInput={(e) => setNewPubkey((e.target as HTMLInputElement).value)}
          placeholder="npub1... or hex pubkey"
          class="flex-1 px-4 py-2 rounded-lg text-sm"
          style={{ background: 'var(--color-bg-tertiary)', color: 'var(--color-text-primary)', border: '1px solid var(--color-border)' }}
          onKeyDown={(e) => e.key === 'Enter' && handleAdd()}
        />
        <button
          onClick={handleAdd}
          disabled={!newPubkey.trim()}
          class="lc-pill-primary text-sm"
          style={{ padding: '8px 20px', borderRadius: '10px' }}
        >
          Add
        </button>
      </div>

      {/* Table */}
      {loading ? (
        <div class="space-y-2">
          {[...Array(3)].map((_, i) => (
            <div key={i} class="lc-skeleton h-14 w-full" />
          ))}
        </div>
      ) : entries.length === 0 ? (
        <div style={{ color: 'var(--color-text-secondary)' }}>No whitelisted pubkeys. The relay is open to all.</div>
      ) : (
        <div class="lc-card overflow-hidden" style={{ padding: 0 }}>
          <table class="w-full">
            <thead>
              <tr style={{ background: 'var(--color-bg-primary)' }}>
                <th class="text-left px-4 py-3 text-sm font-medium" style={{ color: 'var(--color-text-secondary)' }}>Profile</th>
                <th class="text-left px-4 py-3 text-sm font-medium" style={{ color: 'var(--color-text-secondary)' }}>npub</th>
                <th class="text-right px-4 py-3 text-sm font-medium" style={{ color: 'var(--color-text-secondary)' }}>Actions</th>
              </tr>
            </thead>
            <tbody>
              {entries.map(entry => {
                const profile = profiles.get(entry.hex)
                const isBlacklisted = blacklistedSet.has(entry.hex)
                return (
                  <tr key={entry.hex} style={{ borderTop: '1px solid var(--color-border)' }} class="hover:bg-white/[0.02] transition-colors">
                    <td class="px-4 py-3">
                      <div
                        class="flex items-center gap-3 cursor-pointer"
                        onClick={() => openProfile(entry.hex, entry.npub)}
                      >
                        {profilesLoading && !profile ? (
                          <div class="lc-skeleton w-8 h-8 rounded-full flex-shrink-0" />
                        ) : profile?.picture ? (
                          <img src={profile.picture} alt="" class="w-8 h-8 rounded-full object-cover flex-shrink-0"
                            style={{ border: '1px solid var(--color-border)' }}
                            onError={(e) => { (e.target as HTMLImageElement).style.display = 'none' }} />
                        ) : (
                          <div class="w-8 h-8 rounded-full flex-shrink-0 flex items-center justify-center text-xs font-bold"
                            style={{ background: 'rgba(180,249,83,0.1)', color: '#b4f953' }}>
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
                            {truncate(entry.hex)}
                          </div>
                        </div>
                      </div>
                    </td>
                    <td class="px-4 py-3 text-sm font-mono" style={{ color: 'var(--color-text-secondary)' }}>
                      <span class="flex items-center">
                        {truncate(entry.npub)}
                        <CopyNpubButton npub={entry.npub} />
                      </span>
                    </td>
                    <td class="px-4 py-3 text-right">
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
      )}

      <div class="mt-4 text-sm" style={{ color: 'var(--color-text-secondary)' }}>
        {entries.length} whitelisted pubkey{entries.length !== 1 ? 's' : ''}
      </div>

      {/* Blacklist Section */}
      <div class="mt-8">
        <button
          onClick={() => setBlacklistOpen(!blacklistOpen)}
          class="flex items-center gap-2 text-lg font-bold mb-4 cursor-pointer"
        >
          <span style={{ transform: blacklistOpen ? 'rotate(90deg)' : 'rotate(0deg)', transition: 'transform 0.2s', display: 'inline-block' }}>
            &#9654;
          </span>
          Blacklist
          {blacklist.length > 0 && (
            <span class="text-sm font-normal px-2 py-0.5 rounded" style={{ background: 'rgba(239,68,68,0.15)', color: '#f87171' }}>
              {blacklist.length}
            </span>
          )}
        </button>

        {blacklistOpen && (
          <div>
            <p class="text-sm mb-4" style={{ color: 'var(--color-text-secondary)' }}>
              Blacklisted pubkeys are blocked even if they appear in the whitelist or follow-derived list.
            </p>

            <div class="flex gap-2 mb-4">
              <input
                type="text"
                value={newBlacklistPubkey}
                onInput={(e) => setNewBlacklistPubkey((e.target as HTMLInputElement).value)}
                placeholder="npub1... or hex pubkey"
                class="flex-1 px-4 py-2 rounded-lg text-sm"
                style={{ background: 'var(--color-bg-tertiary)', color: 'var(--color-text-primary)', border: '1px solid var(--color-border)' }}
                onKeyDown={(e) => e.key === 'Enter' && handleBlacklistAdd()}
              />
              <button
                onClick={handleBlacklistAdd}
                disabled={!newBlacklistPubkey.trim()}
                class="text-sm px-5 py-2 rounded-lg transition-colors"
                style={{ background: 'rgba(239,68,68,0.15)', color: '#f87171', border: '1px solid rgba(239,68,68,0.3)' }}
              >
                Block
              </button>
            </div>

            {blacklistLoading ? (
              <div class="space-y-2">
                {[...Array(2)].map((_, i) => (
                  <div key={i} class="lc-skeleton h-14 w-full" />
                ))}
              </div>
            ) : blacklist.length === 0 ? (
              <div class="text-sm" style={{ color: 'var(--color-text-secondary)' }}>No blacklisted pubkeys.</div>
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
                          <div class="text-xs font-mono" style={{ color: 'var(--color-text-secondary)' }}>
                            {truncate(entry.npub)}
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
      </div>

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
