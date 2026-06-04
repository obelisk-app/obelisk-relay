import { useState, useEffect, useRef } from 'preact/hooks'
import { adminApi, EventInfo, MemberInfo, type GroupInfo } from '../../services/AdminApiClient'
import { SearchIcon } from './SearchIcon'

interface Props {
  group: GroupInfo
  onClose: () => void
}

type Tab = 'events' | 'members'

const KIND_LABELS: Record<number, string> = {
  9: 'message',
  9021: 'join req',
  9022: 'leave req',
  9009: 'invite',
  9000: 'add user',
  9001: 'remove user',
  9002: 'edit meta',
  9005: 'delete event',
  9006: 'set roles',
  9007: 'create group',
  9008: 'delete group',
}

const kindLabel = (k: number) => KIND_LABELS[k] ?? `kind ${k}`

const relativeTime = (ts: number): string => {
  const diff = Math.floor(Date.now() / 1000) - ts
  if (diff < 60) return `${diff}s ago`
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`
  return `${Math.floor(diff / 86400)}d ago`
}

const short = (s: string, n = 8) => `${s.slice(0, n)}…`

const accessText = (group: GroupInfo) => {
  const read = group.private ? 'Private' : 'Public'
  const join = group.closed ? 'Closed' : 'Open'
  return `${read} / ${join}`
}

export const GroupEventBrowser = ({ group, onClose }: Props) => {
  const [tab, setTab] = useState<Tab>('events')

  // Events state
  const [events, setEvents] = useState<EventInfo[]>([])
  const [eventsLoading, setEventsLoading] = useState(true)
  const [eventsError, setEventsError] = useState<string | null>(null)
  const [search, setSearch] = useState('')
  const [authorFilter, setAuthorFilter] = useState<string | null>(null)
  const [deletingEvent, setDeletingEvent] = useState<string | null>(null)
  const [wipingUser, setWipingUser] = useState(false)
  const [confirmWipe, setConfirmWipe] = useState(false)

  // Members state
  const [members, setMembers] = useState<MemberInfo[]>([])
  const [membersLoading, setMembersLoading] = useState(false)
  const [membersError, setMembersError] = useState<string | null>(null)
  const [removingMember, setRemovingMember] = useState<string | null>(null)
  const [confirmRemove, setConfirmRemove] = useState<string | null>(null)

  const [toast, setToast] = useState<{ msg: string; type: 'ok' | 'err' } | null>(null)
  const overlayRef = useRef<HTMLDivElement>(null)

  const showToast = (msg: string, type: 'ok' | 'err' = 'ok') => {
    setToast({ msg, type })
    setTimeout(() => setToast(null), 3500)
  }

  // Load events on mount or when author filter changes
  const loadEvents = (author?: string | null) => {
    setEventsLoading(true)
    setEventsError(null)
    adminApi.getGroupEvents(group.id, 500, author ?? undefined)
      .then(data => setEvents(data))
      .catch(e => setEventsError(e.message))
      .finally(() => setEventsLoading(false))
  }

  useEffect(() => { loadEvents(authorFilter) }, [group.id, authorFilter])

  // Load members when tab switches to members
  useEffect(() => {
    if (tab !== 'members' || members.length > 0) return
    setMembersLoading(true)
    adminApi.getGroupMembers(group.id)
      .then(data => { setMembers(data); setMembersError(null) })
      .catch(e => setMembersError(e.message))
      .finally(() => setMembersLoading(false))
  }, [tab])

  const handleDeleteEvent = async (id: string) => {
    setDeletingEvent(id)
    try {
      await adminApi.deleteEvent(id)
      setEvents(prev => prev.filter(e => e.id !== id))
      showToast('Event deleted')
    } catch (e) {
      showToast(e instanceof Error ? e.message : 'Failed to delete', 'err')
    } finally {
      setDeletingEvent(null)
    }
  }

  const handleWipeUser = async () => {
    if (!authorFilter) return
    setWipingUser(true)
    try {
      await adminApi.deleteUserEvents(authorFilter)
      setEvents([])
      setAuthorFilter(null)
      setConfirmWipe(false)
      showToast('All events by this user deleted relay-wide')
    } catch (e) {
      showToast(e instanceof Error ? e.message : 'Failed to wipe', 'err')
    } finally {
      setWipingUser(false)
    }
  }

  const handleRemoveMember = async (pubkey: string) => {
    setRemovingMember(pubkey)
    try {
      await adminApi.removeGroupMember(group.id, pubkey)
      setMembers(prev => prev.filter(m => m.pubkey !== pubkey))
      setConfirmRemove(null)
      showToast('Member removed from group')
    } catch (e) {
      showToast(e instanceof Error ? e.message : 'Failed to remove', 'err')
    } finally {
      setRemovingMember(null)
    }
  }

  const handleOverlayClick = (e: MouseEvent) => {
    if (e.target === overlayRef.current) onClose()
  }

  // Client-side content search (after server-side author filter)
  const q = search.toLowerCase()
  const filteredEvents = events.filter(ev => {
    if (!q) return true
    return (
      ev.content.toLowerCase().includes(q) ||
      ev.pubkey.toLowerCase().includes(q) ||
      ev.id.toLowerCase().includes(q) ||
      kindLabel(ev.kind).includes(q)
    )
  })

  const tabStyle = (id: Tab) => ({
    borderBottom: tab === id ? '2px solid #b4f953' : '2px solid transparent',
    color: tab === id ? '#b4f953' : 'var(--color-text-secondary)',
    background: 'transparent',
    padding: '8px 16px',
    fontSize: '14px',
    cursor: 'pointer',
    transition: 'color 0.15s',
  })

  return (
    <div
      ref={overlayRef}
      onClick={handleOverlayClick}
      class="fixed inset-0 z-50 flex items-center justify-center"
      style={{ background: 'rgba(0,0,0,0.75)' }}
    >
      <div
        class="lc-card flex flex-col"
        style={{ width: '92%', maxWidth: '960px', maxHeight: '85vh', overflow: 'hidden', padding: '20px' }}
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div class="flex items-start justify-between gap-4 mb-4">
          <div class="min-w-0">
            <h3 class="text-lg font-bold break-words">{group.name || group.id}</h3>
            <div class="text-xs font-mono mt-0.5 break-all" style={{ color: 'var(--color-text-secondary)' }}>
              {group.id}
            </div>
            <div class="mt-3 flex flex-wrap gap-2">
              <span class="px-2 py-0.5 rounded-full text-xs" style={{ background: 'var(--color-bg-tertiary)', color: 'var(--color-text-secondary)', border: '1px solid var(--color-border)' }}>
                {accessText(group)}
              </span>
              {group.channel_kind && (
                <span class="px-2 py-0.5 rounded-full text-xs" style={{ background: 'var(--color-bg-tertiary)', color: 'var(--color-text-secondary)', border: '1px solid var(--color-border)' }}>
                  {group.channel_kind}
                </span>
              )}
              {group.broadcast && (
                <span class="px-2 py-0.5 rounded-full text-xs" style={{ background: 'rgba(180,249,83,0.10)', color: '#b4f953', border: '1px solid rgba(180,249,83,0.25)' }}>
                  Broadcast
                </span>
              )}
            </div>
          </div>
          <button onClick={onClose} style={{ color: 'var(--color-text-secondary)', fontSize: '20px', lineHeight: 1 }}>✕</button>
        </div>

        {(group.about || group.parent || group.picture || group.banner || group.metadata_tags.length > 0) && (
          <div class="mb-4 p-3 text-xs grid md:grid-cols-2 gap-3" style={{ background: 'var(--color-bg-tertiary)', border: '1px solid var(--color-border)', borderRadius: '8px', color: 'var(--color-text-secondary)' }}>
            {group.about && <div class="md:col-span-2">{group.about}</div>}
            {group.parent && <div><span>Parent: </span><span class="font-mono break-all">{group.parent}</span></div>}
            {group.picture && <div><span>Picture: </span><a href={group.picture} target="_blank" rel="noopener noreferrer" class="font-mono break-all hover:underline" style={{ color: '#b4f953' }}>{group.picture}</a></div>}
            {group.banner && <div><span>Banner: </span><a href={group.banner} target="_blank" rel="noopener noreferrer" class="font-mono break-all hover:underline" style={{ color: '#b4f953' }}>{group.banner}</a></div>}
            {group.metadata_tags.length > 0 && <div>Extra tags: {group.metadata_tags.length}</div>}
          </div>
        )}

        {/* Toast */}
        {toast && (
          <div class="mb-3 px-3 py-2 rounded text-sm" style={{
            background: toast.type === 'ok' ? 'rgba(180,249,83,0.08)' : 'rgba(239,68,68,0.1)',
            color: toast.type === 'ok' ? '#b4f953' : '#f87171',
            border: `1px solid ${toast.type === 'ok' ? 'rgba(180,249,83,0.2)' : 'rgba(239,68,68,0.3)'}`,
          }}>
            {toast.msg}
          </div>
        )}

        {/* Tabs */}
        <div style={{ borderBottom: '1px solid var(--color-border)', marginBottom: '16px', display: 'flex', gap: '4px' }}>
          <button style={tabStyle('events')} onClick={() => setTab('events')}>Events</button>
          <button style={tabStyle('members')} onClick={() => setTab('members')}>
            Members {members.length > 0 ? `(${members.length})` : ''}
          </button>
        </div>

        {/* ── EVENTS TAB ── */}
        {tab === 'events' && (
          <div class="flex flex-col" style={{ flex: 1, overflow: 'hidden', minHeight: 0 }}>
            {/* Search + author filter row */}
            <div class="flex gap-2 mb-3" style={{ flexShrink: 0 }}>
              <div class="admin-search-field flex-1">
                <SearchIcon class="admin-search-icon" />
              <input
                type="text"
                value={search}
                onInput={e => setSearch((e.target as HTMLInputElement).value)}
                placeholder="Search content, pubkey, event ID…"
                class="admin-search-input"
              />
              </div>
              {search && (
                <button
                  onClick={() => setSearch('')}
                  class="px-3 py-2 rounded-lg text-sm"
                  style={{ background: 'var(--color-bg-tertiary)', color: 'var(--color-text-secondary)', border: '1px solid var(--color-border)' }}
                >
                  Clear
                </button>
              )}
            </div>

            {/* Author filter banner */}
            {authorFilter && (
              <div class="mb-3 px-3 py-2 rounded-lg flex items-center justify-between gap-3" style={{ background: 'rgba(180,249,83,0.06)', border: '1px solid rgba(180,249,83,0.15)', flexShrink: 0 }}>
                <div class="text-sm">
                  <span style={{ color: 'var(--color-text-secondary)' }}>Filtering by: </span>
                  <span class="font-mono text-xs">{authorFilter}</span>
                </div>
                <div class="flex items-center gap-2">
                  {confirmWipe ? (
                    <>
                      <span class="text-xs text-red-400">Delete ALL events by this user relay-wide?</span>
                      <button
                        onClick={handleWipeUser}
                        disabled={wipingUser}
                        class="text-xs px-2 py-1 rounded"
                        style={{ background: 'rgba(239,68,68,0.2)', color: '#f87171', border: '1px solid rgba(239,68,68,0.4)' }}
                      >
                        {wipingUser ? 'Wiping…' : 'Confirm wipe'}
                      </button>
                      <button onClick={() => setConfirmWipe(false)} class="text-xs" style={{ color: 'var(--color-text-secondary)' }}>
                        Cancel
                      </button>
                    </>
                  ) : (
                    <button
                      onClick={() => setConfirmWipe(true)}
                      class="text-xs px-2 py-1 rounded"
                      style={{ background: 'rgba(239,68,68,0.15)', color: '#f87171', border: '1px solid rgba(239,68,68,0.3)' }}
                    >
                      Wipe all events (relay-wide)
                    </button>
                  )}
                  <button
                    onClick={() => { setAuthorFilter(null); setConfirmWipe(false) }}
                    class="text-xs"
                    style={{ color: 'var(--color-text-secondary)' }}
                  >
                    ✕ Clear filter
                  </button>
                </div>
              </div>
            )}

            {/* Table */}
            <div style={{ overflowY: 'auto', flex: 1 }}>
              {eventsLoading ? (
                <div class="flex items-center gap-3 p-4" style={{ color: 'var(--color-text-secondary)' }}>
                  <span class="lc-spinner" /> Loading events…
                </div>
              ) : eventsError ? (
                <div class="p-4 text-red-400">{eventsError}</div>
              ) : filteredEvents.length === 0 ? (
                <div class="p-4" style={{ color: 'var(--color-text-secondary)' }}>
                  {events.length === 0 ? 'No events found.' : `No events match "${search}".`}
                </div>
              ) : (
                <table class="w-full text-sm">
                  <thead style={{ position: 'sticky', top: 0, background: 'var(--color-bg-secondary)', zIndex: 1 }}>
                    <tr>
                      <th class="text-left px-3 py-2 font-medium" style={{ color: 'var(--color-text-secondary)' }}>Event ID</th>
                      <th class="text-left px-3 py-2 font-medium" style={{ color: 'var(--color-text-secondary)' }}>Author</th>
                      <th class="text-left px-3 py-2 font-medium" style={{ color: 'var(--color-text-secondary)' }}>Kind</th>
                      <th class="text-left px-3 py-2 font-medium" style={{ color: 'var(--color-text-secondary)' }}>Content</th>
                      <th class="text-left px-3 py-2 font-medium" style={{ color: 'var(--color-text-secondary)' }}>Time</th>
                      <th class="px-3 py-2" />
                    </tr>
                  </thead>
                  <tbody>
                    {filteredEvents.map(ev => (
                      <tr key={ev.id} style={{ borderTop: '1px solid var(--color-border)' }} class="hover:bg-white/[0.02] transition-colors">
                        <td class="px-3 py-2 font-mono text-xs" style={{ color: 'var(--color-text-secondary)' }} title={ev.id}>
                          {short(ev.id)}
                        </td>
                        <td class="px-3 py-2 font-mono text-xs">
                          <button
                            onClick={() => { setAuthorFilter(ev.pubkey); setConfirmWipe(false) }}
                            title={`Filter by ${ev.pubkey}`}
                            style={{ color: authorFilter === ev.pubkey ? '#b4f953' : 'var(--color-text-secondary)', textDecoration: 'underline dotted', cursor: 'pointer', background: 'none', border: 'none', padding: 0 }}
                          >
                            {short(ev.pubkey)}
                          </button>
                        </td>
                        <td class="px-3 py-2">
                          <span class="px-1.5 py-0.5 rounded text-xs" style={{ background: 'var(--color-bg-tertiary)', whiteSpace: 'nowrap' }}>
                            {kindLabel(ev.kind)}
                          </span>
                        </td>
                        <td class="px-3 py-2 text-xs" style={{ maxWidth: '280px', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}
                          title={ev.content}>
                          {ev.content || <span style={{ color: 'var(--color-text-secondary)', fontStyle: 'italic' }}>empty</span>}
                        </td>
                        <td class="px-3 py-2 text-xs whitespace-nowrap" style={{ color: 'var(--color-text-secondary)' }}>
                          {relativeTime(ev.created_at)}
                        </td>
                        <td class="px-3 py-2 text-right">
                          <button
                            onClick={() => handleDeleteEvent(ev.id)}
                            disabled={deletingEvent === ev.id}
                            class="text-xs text-red-400 hover:text-red-300 transition-colors opacity-60 hover:opacity-100"
                          >
                            {deletingEvent === ev.id ? '…' : 'Delete'}
                          </button>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </div>

            <div class="mt-2 text-xs" style={{ color: 'var(--color-text-secondary)', flexShrink: 0 }}>
              {!eventsLoading && !eventsError && (
                filteredEvents.length !== events.length
                  ? `${filteredEvents.length} of ${events.length} events`
                  : `${events.length} event${events.length !== 1 ? 's' : ''}`
              )}
            </div>
          </div>
        )}

        {/* ── MEMBERS TAB ── */}
        {tab === 'members' && (
          <div class="flex flex-col" style={{ flex: 1, overflow: 'hidden', minHeight: 0 }}>
            <div style={{ overflowY: 'auto', flex: 1 }}>
              {membersLoading ? (
                <div class="flex items-center gap-3 p-4" style={{ color: 'var(--color-text-secondary)' }}>
                  <span class="lc-spinner" /> Loading members…
                </div>
              ) : membersError ? (
                <div class="p-4 text-red-400">{membersError}</div>
              ) : members.length === 0 ? (
                <div class="p-4" style={{ color: 'var(--color-text-secondary)' }}>No members.</div>
              ) : (
                <table class="w-full text-sm">
                  <thead style={{ position: 'sticky', top: 0, background: 'var(--color-bg-secondary)', zIndex: 1 }}>
                    <tr>
                      <th class="text-left px-3 py-2 font-medium" style={{ color: 'var(--color-text-secondary)' }}>Pubkey</th>
                      <th class="text-left px-3 py-2 font-medium" style={{ color: 'var(--color-text-secondary)' }}>Roles</th>
                      <th class="px-3 py-2" />
                    </tr>
                  </thead>
                  <tbody>
                    {members.map(m => (
                      <tr key={m.pubkey} style={{ borderTop: '1px solid var(--color-border)' }} class="hover:bg-white/[0.02] transition-colors">
                        <td class="px-3 py-2 font-mono text-xs" title={m.pubkey} style={{ color: 'var(--color-text-secondary)' }}>
                          <button
                            onClick={() => { setTab('events'); setAuthorFilter(m.pubkey) }}
                            title="View events by this member"
                            style={{ color: 'var(--color-text-secondary)', textDecoration: 'underline dotted', cursor: 'pointer', background: 'none', border: 'none', padding: 0, fontFamily: 'monospace', fontSize: '12px' }}
                          >
                            {short(m.pubkey, 12)}
                          </button>
                          <span style={{ marginLeft: '4px', color: 'var(--color-text-secondary)', opacity: 0.5 }}>{m.pubkey.slice(-8)}</span>
                        </td>
                        <td class="px-3 py-2">
                          <div class="flex gap-1 flex-wrap">
                            {m.roles.map(r => (
                              <span key={r} class="px-1.5 py-0.5 rounded text-xs" style={{
                                background: r === 'Admin' ? 'rgba(180,249,83,0.1)' : 'var(--color-bg-tertiary)',
                                color: r === 'Admin' ? '#b4f953' : 'var(--color-text-secondary)',
                              }}>
                                {r}
                              </span>
                            ))}
                          </div>
                        </td>
                        <td class="px-3 py-2 text-right">
                          {confirmRemove === m.pubkey ? (
                            <span class="flex items-center justify-end gap-2">
                              <button
                                onClick={() => handleRemoveMember(m.pubkey)}
                                disabled={removingMember === m.pubkey}
                                class="text-xs text-red-400 hover:text-red-300 transition-colors"
                              >
                                {removingMember === m.pubkey ? '…' : 'Confirm remove'}
                              </button>
                              <button onClick={() => setConfirmRemove(null)} class="text-xs" style={{ color: 'var(--color-text-secondary)' }}>
                                Cancel
                              </button>
                            </span>
                          ) : (
                            <button
                              onClick={() => setConfirmRemove(m.pubkey)}
                              class="text-xs text-red-400 hover:text-red-300 transition-colors opacity-60 hover:opacity-100"
                            >
                              Remove
                            </button>
                          )}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </div>

            <div class="mt-2 text-xs" style={{ color: 'var(--color-text-secondary)', flexShrink: 0 }}>
              {!membersLoading && !membersError && `${members.length} member${members.length !== 1 ? 's' : ''}`}
            </div>
          </div>
        )}
      </div>
    </div>
  )
}
