import { useState, useEffect } from 'preact/hooks'
import { adminApi } from '../../services/AdminApiClient'

interface GroupInfo {
  id: string
  name: string
  about: string | null
  member_count: number
  private: boolean
  closed: boolean
  broadcast: boolean
}

export const GroupsOverview = () => {
  const [groups, setGroups] = useState<GroupInfo[]>([])
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  const [search, setSearch] = useState('')
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null)
  const [toast, setToast] = useState<string | null>(null)
  const [deleting, setDeleting] = useState<string | null>(null)

  const showToast = (msg: string) => {
    setToast(msg)
    setTimeout(() => setToast(null), 3000)
  }

  const fetchGroups = () => {
    setLoading(true)
    adminApi.getGroups()
      .then(data => { setGroups(data); setError(null) })
      .catch(e => setError(e.message))
      .finally(() => setLoading(false))
  }

  useEffect(() => { fetchGroups() }, [])

  const handleDelete = async (id: string) => {
    setDeleting(id)
    try {
      await adminApi.deleteGroup(id)
      setGroups(prev => prev.filter(g => g.id !== id))
      setConfirmDelete(null)
      showToast(`Group "${id}" deleted`)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to delete group')
    } finally {
      setDeleting(null)
    }
  }

  if (error) {
    return (
      <div>
        <div class="mb-4 p-4 rounded-lg bg-red-500/10 text-red-400 border border-red-500/20">{error}</div>
        <button onClick={() => setError(null)} class="text-sm" style={{ color: 'var(--color-text-secondary)' }}>Dismiss</button>
      </div>
    )
  }

  const badge = (text: string, active: boolean) => (
    <span class={`px-2 py-0.5 rounded-full text-xs font-medium ${active ? 'text-lc-green' : 'text-lc-muted'}`}
      style={{ background: active ? 'rgba(180,249,83,0.1)' : 'rgba(163,163,163,0.1)' }}>
      {text}
    </span>
  )

  const q = search.toLowerCase()
  const filtered = groups.filter(g =>
    !q ||
    g.name.toLowerCase().includes(q) ||
    g.id.toLowerCase().includes(q) ||
    (g.about || '').toLowerCase().includes(q)
  )

  return (
    <div>
      <h2 class="text-xl font-bold mb-6">Groups Overview</h2>

      {toast && (
        <div class="mb-4 p-3 rounded-lg text-sm border" style={{ background: 'rgba(180,249,83,0.08)', color: '#b4f953', borderColor: 'rgba(180,249,83,0.2)' }}>
          {toast}
        </div>
      )}

      {/* Search */}
      {groups.length > 0 && (
        <div class="mb-4">
          <input
            type="text"
            value={search}
            onInput={(e) => setSearch((e.target as HTMLInputElement).value)}
            placeholder="Search by name or ID..."
            class="w-full px-4 py-2 rounded-lg text-sm"
            style={{ background: 'var(--color-bg-tertiary)', color: 'var(--color-text-primary)', border: '1px solid var(--color-border)' }}
          />
        </div>
      )}

      {loading ? (
        <div class="flex items-center gap-3" style={{ color: 'var(--color-text-secondary)' }}>
          <span class="lc-spinner" />
          Loading groups...
        </div>
      ) : groups.length === 0 ? (
        <div style={{ color: 'var(--color-text-secondary)' }}>No groups yet.</div>
      ) : filtered.length === 0 ? (
        <div style={{ color: 'var(--color-text-secondary)' }}>No groups match "{search}".</div>
      ) : (
        <div class="lc-card overflow-hidden" style={{ padding: 0 }}>
          <table class="w-full">
            <thead>
              <tr style={{ background: 'var(--color-bg-primary)' }}>
                <th class="text-left px-4 py-3 text-sm font-medium" style={{ color: 'var(--color-text-secondary)' }}>Name</th>
                <th class="text-left px-4 py-3 text-sm font-medium" style={{ color: 'var(--color-text-secondary)' }}>ID</th>
                <th class="text-center px-4 py-3 text-sm font-medium" style={{ color: 'var(--color-text-secondary)' }}>Members</th>
                <th class="text-left px-4 py-3 text-sm font-medium" style={{ color: 'var(--color-text-secondary)' }}>Type</th>
                <th class="text-right px-4 py-3 text-sm font-medium" style={{ color: 'var(--color-text-secondary)' }}>Actions</th>
              </tr>
            </thead>
            <tbody>
              {filtered.map(group => (
                <tr key={group.id} style={{ borderTop: '1px solid var(--color-border)' }} class="hover:bg-white/[0.02] transition-colors">
                  <td class="px-4 py-3">
                    <div class="font-medium">{group.name || '(unnamed)'}</div>
                    {group.about && (
                      <div class="text-xs mt-0.5" style={{ color: 'var(--color-text-secondary)' }}>
                        {group.about.length > 60 ? group.about.slice(0, 60) + '...' : group.about}
                      </div>
                    )}
                  </td>
                  <td class="px-4 py-3 text-sm font-mono" style={{ color: 'var(--color-text-secondary)' }}>
                    {group.id.length > 12 ? group.id.slice(0, 12) + '...' : group.id}
                  </td>
                  <td class="px-4 py-3 text-center text-sm">{group.member_count}</td>
                  <td class="px-4 py-3">
                    <div class="flex gap-1 flex-wrap">
                      {badge(group.private ? 'Private' : 'Public', group.private)}
                      {badge(group.closed ? 'Closed' : 'Open', group.closed)}
                      {group.broadcast && badge('Broadcast', true)}
                    </div>
                  </td>
                  <td class="px-4 py-3 text-right">
                    {confirmDelete === group.id ? (
                      <span class="flex items-center justify-end gap-2">
                        <button
                          onClick={() => handleDelete(group.id)}
                          disabled={deleting === group.id}
                          class="text-sm text-red-400 hover:text-red-300 transition-colors"
                        >
                          {deleting === group.id ? 'Deleting...' : 'Confirm delete'}
                        </button>
                        <button
                          onClick={() => setConfirmDelete(null)}
                          class="text-sm transition-colors" style={{ color: 'var(--color-text-secondary)' }}
                        >
                          Cancel
                        </button>
                      </span>
                    ) : (
                      <button
                        onClick={() => setConfirmDelete(group.id)}
                        class="text-sm text-red-400 hover:text-red-300 transition-colors opacity-60 hover:opacity-100"
                      >
                        Delete
                      </button>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      <div class="mt-4 text-sm" style={{ color: 'var(--color-text-secondary)' }}>
        {filtered.length !== groups.length
          ? `${filtered.length} of ${groups.length} group${groups.length !== 1 ? 's' : ''}`
          : `${groups.length} group${groups.length !== 1 ? 's' : ''}`
        }
      </div>
    </div>
  )
}
