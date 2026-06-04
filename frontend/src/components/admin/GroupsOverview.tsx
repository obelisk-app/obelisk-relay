import { useState, useEffect } from 'preact/hooks'
import { adminApi, type GroupInfo } from '../../services/AdminApiClient'
import { GroupEventBrowser } from './GroupEventBrowser'
import { SearchIcon } from './SearchIcon'

const short = (value: string, head = 10, tail = 6) => (
  value.length > head + tail + 3 ? `${value.slice(0, head)}...${value.slice(-tail)}` : value
)

const groupType = (group: GroupInfo) => {
  const channel = group.channel_kind ? group.channel_kind : 'text'
  if (group.broadcast) return `${channel} broadcast`
  return channel
}

const accessLabel = (group: GroupInfo) => {
  const read = group.private ? 'Private' : 'Public'
  const join = group.closed ? 'Closed' : 'Open'
  return `${read} / ${join}`
}

const badge = (text: string, active = false) => (
  <span
    class="px-2 py-0.5 text-xs font-medium"
    style={{
      borderRadius: '999px',
      background: active ? 'rgba(180,249,83,0.12)' : 'rgba(255,255,255,0.06)',
      color: active ? '#b4f953' : 'var(--color-text-secondary)',
      border: active ? '1px solid rgba(180,249,83,0.20)' : '1px solid var(--color-border)',
    }}
  >
    {text}
  </span>
)

export const GroupsOverview = () => {
  const [groups, setGroups] = useState<GroupInfo[]>([])
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  const [search, setSearch] = useState('')
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null)
  const [toast, setToast] = useState<string | null>(null)
  const [deleting, setDeleting] = useState<string | null>(null)
  const [selectedGroup, setSelectedGroup] = useState<GroupInfo | null>(null)
  const [browserGroup, setBrowserGroup] = useState<GroupInfo | null>(null)

  const showToast = (msg: string) => {
    setToast(msg)
    setTimeout(() => setToast(null), 3000)
  }

  const fetchGroups = () => {
    setLoading(true)
    adminApi.getGroups()
      .then(data => {
        setGroups(data)
        setSelectedGroup(prev => {
          if (!data.length) return null
          if (!prev) return data[0]
          return data.find(group => group.id === prev.id) ?? data[0]
        })
        setError(null)
      })
      .catch(e => setError(e.message))
      .finally(() => setLoading(false))
  }

  useEffect(() => { fetchGroups() }, [])

  const handleDelete = async (id: string) => {
    setDeleting(id)
    try {
      await adminApi.deleteGroup(id)
      setGroups(prev => prev.filter(g => g.id !== id))
      setSelectedGroup(prev => prev?.id === id ? null : prev)
      setConfirmDelete(null)
      showToast(`Group "${id}" deleted`)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to delete group')
    } finally {
      setDeleting(null)
    }
  }

  const q = search.toLowerCase()
  const filtered = groups.filter(g =>
    !q ||
    g.name.toLowerCase().includes(q) ||
    g.id.toLowerCase().includes(q) ||
    (g.about || '').toLowerCase().includes(q) ||
    (g.channel_kind || '').toLowerCase().includes(q) ||
    (g.parent || '').toLowerCase().includes(q)
  )

  const totalMembers = groups.reduce((sum, group) => sum + group.member_count, 0)
  const privateGroups = groups.filter(group => group.private).length
  const broadcastGroups = groups.filter(group => group.broadcast).length

  if (error) {
    return (
      <div>
        <div class="mb-4 p-4 rounded-lg bg-red-500/10 text-red-400 border border-red-500/20">{error}</div>
        <button onClick={() => setError(null)} class="text-sm" style={{ color: 'var(--color-text-secondary)' }}>Dismiss</button>
      </div>
    )
  }

  return (
    <div>
      <div class="flex flex-col md:flex-row md:items-end md:justify-between gap-4 mb-6">
        <div>
          <h2 class="text-xl font-bold">Groups</h2>
          <p class="text-sm mt-1" style={{ color: 'var(--color-text-secondary)' }}>
            Browse relay groups, metadata, members, and stored events.
          </p>
        </div>
        <button onClick={fetchGroups} disabled={loading} class="lc-pill-secondary text-sm" style={{ borderRadius: '8px', padding: '8px 16px' }}>
          {loading ? 'Refreshing...' : 'Refresh'}
        </button>
      </div>

      {toast && (
        <div class="mb-4 p-3 rounded-lg text-sm border" style={{ background: 'rgba(180,249,83,0.08)', color: '#b4f953', borderColor: 'rgba(180,249,83,0.2)' }}>
          {toast}
        </div>
      )}

      <div class="grid grid-cols-2 lg:grid-cols-4 gap-3 mb-5">
        {[
          ['Groups', groups.length],
          ['Members', totalMembers],
          ['Private', privateGroups],
          ['Broadcast', broadcastGroups],
        ].map(([label, value]) => (
          <div key={label} class="lc-card p-4">
            <div class="text-2xl font-bold" style={{ color: label === 'Groups' ? '#b4f953' : 'var(--color-text-primary)' }}>{value}</div>
            <div class="text-xs mt-1" style={{ color: 'var(--color-text-secondary)' }}>{label}</div>
          </div>
        ))}
      </div>

      <div class="admin-search-field mb-4">
        <SearchIcon class="admin-search-icon" />
        <input
          type="text"
          value={search}
          onInput={(e) => setSearch((e.target as HTMLInputElement).value)}
          placeholder="Search name, ID, parent, or channel kind"
          class="admin-search-input"
        />
      </div>

      {loading ? (
        <div class="flex items-center gap-3" style={{ color: 'var(--color-text-secondary)' }}>
          <span class="lc-spinner" />
          Loading groups...
        </div>
      ) : groups.length === 0 ? (
        <div class="lc-card p-8 text-center" style={{ borderStyle: 'dashed' }}>
          <div class="text-lg" style={{ color: 'var(--color-text-secondary)' }}>No groups yet.</div>
        </div>
      ) : (
        <div class="grid xl:grid-cols-[minmax(0,1fr)_360px] gap-5 items-start">
          <div class="lc-card overflow-hidden" style={{ padding: 0 }}>
            <div class="overflow-x-auto">
              <table class="w-full">
                <thead>
                  <tr style={{ background: 'var(--color-bg-primary)' }}>
                    <th class="text-left px-4 py-3 text-sm font-medium" style={{ color: 'var(--color-text-secondary)' }}>Group</th>
                    <th class="text-left px-4 py-3 text-sm font-medium" style={{ color: 'var(--color-text-secondary)' }}>Access</th>
                    <th class="text-center px-4 py-3 text-sm font-medium" style={{ color: 'var(--color-text-secondary)' }}>Members</th>
                    <th class="text-right px-4 py-3 text-sm font-medium" style={{ color: 'var(--color-text-secondary)' }}>Actions</th>
                  </tr>
                </thead>
                <tbody>
                  {filtered.map(group => {
                    const selected = selectedGroup?.id === group.id
                    return (
                      <tr
                        key={group.id}
                        onClick={() => setSelectedGroup(group)}
                        style={{
                          borderTop: '1px solid var(--color-border)',
                          background: selected ? 'rgba(180,249,83,0.045)' : 'transparent',
                        }}
                        class="hover:bg-white/[0.03] transition-colors cursor-pointer"
                      >
                        <td class="px-4 py-3">
                          <div class="flex items-center gap-3 min-w-[260px]">
                            {group.picture ? (
                              <img src={group.picture} alt="" class="w-9 h-9 object-cover flex-shrink-0" style={{ borderRadius: '8px', border: '1px solid var(--color-border)' }} />
                            ) : (
                              <div class="w-9 h-9 flex items-center justify-center text-xs font-bold flex-shrink-0" style={{ borderRadius: '8px', background: 'rgba(180,249,83,0.10)', color: '#b4f953' }}>
                                {(group.name || group.id).slice(0, 2).toUpperCase()}
                              </div>
                            )}
                            <div class="min-w-0">
                              <div class="font-medium truncate">{group.name || '(unnamed)'}</div>
                              <div class="text-xs font-mono truncate" style={{ color: 'var(--color-text-secondary)' }}>{short(group.id, 12, 8)}</div>
                              {group.about && (
                                <div class="text-xs mt-0.5 truncate max-w-md" style={{ color: 'var(--color-text-secondary)' }}>
                                  {group.about}
                                </div>
                              )}
                            </div>
                          </div>
                        </td>
                        <td class="px-4 py-3">
                          <div class="flex gap-1 flex-wrap">
                            {badge(group.private ? 'Private' : 'Public', group.private)}
                            {badge(group.closed ? 'Closed' : 'Open', group.closed)}
                            {group.broadcast && badge('Broadcast', true)}
                            {group.channel_kind && badge(group.channel_kind)}
                          </div>
                        </td>
                        <td class="px-4 py-3 text-center text-sm">
                          <div>{group.member_count}</div>
                          {group.admin_count > 0 && <div class="text-xs" style={{ color: 'var(--color-text-secondary)' }}>{group.admin_count} admin{group.admin_count !== 1 ? 's' : ''}</div>}
                        </td>
                        <td class="px-4 py-3 text-right">
                          <span class="flex items-center justify-end gap-3">
                            <button
                              onClick={(e) => { e.stopPropagation(); setBrowserGroup(group) }}
                              class="text-sm transition-colors opacity-80 hover:opacity-100"
                              style={{ color: '#b4f953' }}
                            >
                              Open
                            </button>
                            {confirmDelete === group.id ? (
                              <>
                                <button
                                  onClick={(e) => { e.stopPropagation(); void handleDelete(group.id) }}
                                  disabled={deleting === group.id}
                                  class="text-sm text-red-400 hover:text-red-300 transition-colors"
                                >
                                  {deleting === group.id ? 'Deleting...' : 'Confirm'}
                                </button>
                                <button
                                  onClick={(e) => { e.stopPropagation(); setConfirmDelete(null) }}
                                  class="text-sm transition-colors" style={{ color: 'var(--color-text-secondary)' }}
                                >
                                  Cancel
                                </button>
                              </>
                            ) : (
                              <button
                                onClick={(e) => { e.stopPropagation(); setConfirmDelete(group.id) }}
                                class="text-sm text-red-400 hover:text-red-300 transition-colors opacity-70 hover:opacity-100"
                              >
                                Delete
                              </button>
                            )}
                          </span>
                        </td>
                      </tr>
                    )
                  })}
                </tbody>
              </table>
            </div>
          </div>

          <aside class="lc-card p-5 xl:sticky xl:top-6">
            {selectedGroup ? (
              <div>
                {selectedGroup.banner && (
                  <img src={selectedGroup.banner} alt="" class="w-full h-28 object-cover mb-4" style={{ borderRadius: '8px', border: '1px solid var(--color-border)' }} />
                )}
                <div class="flex items-start justify-between gap-3">
                  <div class="min-w-0">
                    <h3 class="text-lg font-bold break-words">{selectedGroup.name || '(unnamed)'}</h3>
                    <div class="mt-1 text-xs font-mono break-all" style={{ color: 'var(--color-text-secondary)' }}>{selectedGroup.id}</div>
                  </div>
                </div>
                {selectedGroup.about && (
                  <p class="mt-4 text-sm leading-relaxed" style={{ color: 'var(--color-text-secondary)' }}>{selectedGroup.about}</p>
                )}

                <div class="mt-5 grid grid-cols-2 gap-3 text-sm">
                  <div>
                    <div class="text-xs" style={{ color: 'var(--color-text-secondary)' }}>Type</div>
                    <div class="mt-1">{groupType(selectedGroup)}</div>
                  </div>
                  <div>
                    <div class="text-xs" style={{ color: 'var(--color-text-secondary)' }}>Access</div>
                    <div class="mt-1">{accessLabel(selectedGroup)}</div>
                  </div>
                  <div>
                    <div class="text-xs" style={{ color: 'var(--color-text-secondary)' }}>Members</div>
                    <div class="mt-1">{selectedGroup.member_count}</div>
                  </div>
                  <div>
                    <div class="text-xs" style={{ color: 'var(--color-text-secondary)' }}>Admins</div>
                    <div class="mt-1">{selectedGroup.admin_count}</div>
                  </div>
                </div>

                <div class="mt-5 space-y-3 text-sm">
                  {selectedGroup.parent && (
                    <div>
                      <div class="text-xs" style={{ color: 'var(--color-text-secondary)' }}>Parent</div>
                      <div class="mt-1 font-mono break-all">{selectedGroup.parent}</div>
                    </div>
                  )}
                  {selectedGroup.picture && (
                    <div>
                      <div class="text-xs" style={{ color: 'var(--color-text-secondary)' }}>Picture</div>
                      <a href={selectedGroup.picture} target="_blank" rel="noopener noreferrer" class="mt-1 block font-mono break-all hover:underline" style={{ color: '#b4f953' }}>
                        {selectedGroup.picture}
                      </a>
                    </div>
                  )}
                  {selectedGroup.metadata_tags.length > 0 && (
                    <div>
                      <div class="text-xs mb-2" style={{ color: 'var(--color-text-secondary)' }}>Extra metadata tags</div>
                      <div class="space-y-1">
                        {selectedGroup.metadata_tags.map((tag, index) => (
                          <div key={index} class="px-2 py-1 text-xs font-mono break-all" style={{ background: 'var(--color-bg-tertiary)', border: '1px solid var(--color-border)', borderRadius: '6px' }}>
                            [{tag.join(', ')}]
                          </div>
                        ))}
                      </div>
                    </div>
                  )}
                </div>

                <button onClick={() => setBrowserGroup(selectedGroup)} class="mt-5 w-full lc-pill-primary text-sm" style={{ borderRadius: '8px' }}>
                  Events and members
                </button>
              </div>
            ) : (
              <div class="text-sm" style={{ color: 'var(--color-text-secondary)' }}>Select a group to inspect metadata.</div>
            )}
          </aside>
        </div>
      )}

      <div class="mt-4 text-sm" style={{ color: 'var(--color-text-secondary)' }}>
        {filtered.length !== groups.length
          ? `${filtered.length} of ${groups.length} group${groups.length !== 1 ? 's' : ''}`
          : `${groups.length} group${groups.length !== 1 ? 's' : ''}`
        }
      </div>

      {browserGroup && (
        <GroupEventBrowser
          group={browserGroup}
          onClose={() => setBrowserGroup(null)}
        />
      )}
    </div>
  )
}
