import { useState, useEffect, useRef } from 'preact/hooks'
import { adminApi, EventInfo, MemberInfo, type GroupInfo } from '../../services/AdminApiClient'
import { SearchIcon } from './SearchIcon'
import { GroupChatView } from './GroupChatView'
import { useRowSelection } from './useRowSelection'

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
  // Multi-select for bulk moderation. lastIndex anchors shift-click ranges.
  const [bulkDeleting, setBulkDeleting] = useState(false)
  const [confirmBulk, setConfirmBulk] = useState(false)
  const [wipingUser, setWipingUser] = useState(false)
  const [confirmWipe, setConfirmWipe] = useState(false)

  // Members state
  const [members, setMembers] = useState<MemberInfo[]>([])
  const [membersLoading, setMembersLoading] = useState(false)
  const [membersError, setMembersError] = useState<string | null>(null)
  const [removingMember, setRemovingMember] = useState<string | null>(null)
  const [confirmRemove, setConfirmRemove] = useState<string | null>(null)
  // Bulk member moderation. Same selection mechanics as the events tab.
  const memberIds = members.map(m => m.pubkey)
  const memberSel = useRowSelection(memberIds)
  const [memberAction, setMemberAction] = useState<'remove' | 'wipe' | null>(null)
  const [memberConfirmText, setMemberConfirmText] = useState('')
  const [memberBusy, setMemberBusy] = useState(false)

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

  const runMemberBulk = async () => {
    const pubkeys = [...memberSel.selected]
    if (pubkeys.length === 0 || !memberAction) return
    setMemberBusy(true)
    try {
      if (memberAction === 'remove') {
        // No batch endpoint for membership; sequential keeps per-member
        // failures visible instead of aborting the whole set.
        let removed = 0
        const failures: string[] = []
        for (const pk of pubkeys) {
          try {
            await adminApi.removeGroupMember(group.id, pk)
            removed += 1
          } catch {
            failures.push(pk)
          }
        }
        const gone = new Set(pubkeys.filter(pk => !failures.includes(pk)))
        setMembers(prev => prev.filter(m => !gone.has(m.pubkey)))
        showToast(
          failures.length === 0
            ? `Removed ${removed} member${removed !== 1 ? 's' : ''} from the group`
            : `Removed ${removed}, ${failures.length} failed`,
          failures.length === 0 ? 'ok' : 'err',
        )
      } else {
        const res = await adminApi.bulkDeleteUserEvents(pubkeys)
        showToast(
          res.failed === 0
            ? `Deleted ${res.deleted} event${res.deleted !== 1 ? 's' : ''} from ${pubkeys.length} user${pubkeys.length !== 1 ? 's' : ''}`
            : `Deleted ${res.deleted}, ${res.failed} user${res.failed !== 1 ? 's' : ''} failed`,
          res.failed === 0 ? 'ok' : 'err',
        )
        loadEvents(authorFilter)
      }
      memberSel.clear()
      setMemberAction(null)
      setMemberConfirmText('')
    } catch (e) {
      showToast(e instanceof Error ? e.message : 'Bulk action failed', 'err')
    } finally {
      setMemberBusy(false)
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

  // The chat view renders oldest-first, so range selection must be computed
  // against that order, not the newest-first API order.
  const chatOrdered = [...filteredEvents].sort((a, b) => a.created_at - b.created_at)

  // Selection mechanics are shared with the members tab.
  const orderedIds = chatOrdered.map(e => e.id)
  const eventSel = useRowSelection(orderedIds)
  const selected = eventSel.selected
  const toggleSelect = eventSel.toggle
  const selectAllVisible = eventSel.selectAll
  const selectAllFromAuthor = (pubkey: string) =>
    eventSel.selectMatching(chatOrdered.filter(e => e.pubkey === pubkey).map(e => e.id))
  const clearSelection = () => { eventSel.clear(); setConfirmBulk(false) }

  const handleBulkDelete = async () => {
    const ids = [...selected]
    if (ids.length === 0) return
    setBulkDeleting(true)
    try {
      const res = await adminApi.bulkDeleteEvents(ids)
      const gone = new Set(res.results.filter(r => r.deleted).map(r => r.id))
      setEvents(prev => prev.filter(e => !gone.has(e.id)))
      clearSelection()
      showToast(
        res.failed === 0
          ? `Deleted ${res.deleted} event${res.deleted !== 1 ? 's' : ''}`
          : `Deleted ${res.deleted}, ${res.failed} failed`,
        res.failed === 0 ? 'ok' : 'err',
      )
    } catch (e) {
      showToast(e instanceof Error ? e.message : 'Bulk delete failed', 'err')
    } finally {
      setBulkDeleting(false)
    }
  }

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
                <GroupChatView
                  events={filteredEvents}
                  selected={selected}
                  onToggle={toggleSelect}
                  onFilterAuthor={pk => { setAuthorFilter(pk); setConfirmWipe(false) }}
                  activeAuthor={authorFilter}
                />
              )}
            </div>

            {/* Selection bar. Sits below the list so it never covers content,
                and states the exact count before anything is deleted. */}
            {selected.size > 0 && (
              <div
                class="mt-2 px-3 py-2 rounded-lg flex items-center gap-3 flex-wrap"
                style={{ background: 'rgba(180,249,83,0.07)', border: '1px solid rgba(180,249,83,0.2)', flexShrink: 0 }}
              >
                <span class="text-sm font-semibold">{selected.size} selected</span>
                <button type="button" onClick={selectAllVisible} class="text-xs underline" style={{ color: 'var(--color-text-secondary)' }}>
                  Select all {orderedIds.length}
                </button>
                {authorFilter && (
                  <button type="button" onClick={() => selectAllFromAuthor(authorFilter)} class="text-xs underline" style={{ color: 'var(--color-text-secondary)' }}>
                    Select all from this author
                  </button>
                )}
                <button type="button" onClick={clearSelection} class="text-xs underline" style={{ color: 'var(--color-text-secondary)' }}>
                  Clear
                </button>
                <div class="flex-1" />
                {confirmBulk ? (
                  <>
                    <span class="text-xs text-red-400">
                      Permanently delete {selected.size} event{selected.size !== 1 ? 's' : ''}?
                    </span>
                    <button
                      type="button"
                      onClick={handleBulkDelete}
                      disabled={bulkDeleting}
                      class="text-xs px-2 py-1 rounded"
                      style={{ background: 'rgba(239,68,68,0.2)', color: '#f87171', border: '1px solid rgba(239,68,68,0.4)' }}
                    >
                      {bulkDeleting ? 'Deleting…' : 'Confirm delete'}
                    </button>
                    <button type="button" onClick={() => setConfirmBulk(false)} class="text-xs" style={{ color: 'var(--color-text-secondary)' }}>
                      Cancel
                    </button>
                  </>
                ) : (
                  <button
                    type="button"
                    onClick={() => setConfirmBulk(true)}
                    class="text-xs px-2 py-1 rounded"
                    style={{ background: 'rgba(239,68,68,0.15)', color: '#f87171', border: '1px solid rgba(239,68,68,0.3)' }}
                  >
                    Delete selected
                  </button>
                )}
              </div>
            )}

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
                      <th class="px-3 py-2 w-8">
                        <input
                          type="checkbox"
                          aria-label="Select all members"
                          checked={memberSel.selected.size > 0 && memberSel.selected.size === members.length}
                          onChange={() => (
                            memberSel.selected.size === members.length
                              ? memberSel.clear()
                              : memberSel.selectAll()
                          )}
                        />
                      </th>
                      <th class="text-left px-3 py-2 font-medium" style={{ color: 'var(--color-text-secondary)' }}>Pubkey</th>
                      <th class="text-left px-3 py-2 font-medium" style={{ color: 'var(--color-text-secondary)' }}>Roles</th>
                      <th class="px-3 py-2" />
                    </tr>
                  </thead>
                  <tbody>
                    {members.map((m, i) => (
                      <tr
                        key={m.pubkey}
                        style={{
                          borderTop: '1px solid var(--color-border)',
                          background: memberSel.selected.has(m.pubkey) ? 'rgba(180,249,83,0.07)' : undefined,
                        }}
                        class="hover:bg-white/[0.02] transition-colors"
                      >
                        <td class="px-3 py-2">
                          <input
                            type="checkbox"
                            aria-label={`Select member ${m.pubkey}`}
                            checked={memberSel.selected.has(m.pubkey)}
                            onClick={e => memberSel.toggle(m.pubkey, i, (e as MouseEvent).shiftKey)}
                          />
                        </td>
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

            {memberSel.selected.size > 0 && (
              <div
                class="mt-2 px-3 py-2 rounded-lg flex flex-col gap-2"
                style={{ background: 'rgba(180,249,83,0.07)', border: '1px solid rgba(180,249,83,0.2)', flexShrink: 0 }}
              >
                <div class="flex items-center gap-3 flex-wrap">
                  <span class="text-sm font-semibold">{memberSel.selected.size} selected</span>
                  <button type="button" onClick={memberSel.selectAll} class="text-xs underline" style={{ color: 'var(--color-text-secondary)' }}>
                    Select all {members.length}
                  </button>
                  <button
                    type="button"
                    onClick={() => { memberSel.clear(); setMemberAction(null); setMemberConfirmText('') }}
                    class="text-xs underline"
                    style={{ color: 'var(--color-text-secondary)' }}
                  >
                    Clear
                  </button>
                  <div class="flex-1" />
                  <button
                    type="button"
                    onClick={() => { setMemberAction('remove'); setMemberConfirmText('') }}
                    class="text-xs px-2 py-1 rounded"
                    style={{ background: 'var(--color-bg-tertiary)', border: '1px solid var(--color-border)' }}
                  >
                    Remove from group
                  </button>
                  <button
                    type="button"
                    onClick={() => { setMemberAction('wipe'); setMemberConfirmText('') }}
                    class="text-xs px-2 py-1 rounded"
                    style={{ background: 'rgba(239,68,68,0.15)', color: '#f87171', border: '1px solid rgba(239,68,68,0.3)' }}
                  >
                    Delete their events
                  </button>
                </div>

                {memberAction && (
                  <div class="p-3 rounded" style={{ background: 'rgba(0,0,0,0.25)' }}>
                    <p class="text-sm mb-2" style={{ color: memberAction === 'wipe' ? '#fca5a5' : 'var(--color-text-primary)' }}>
                      {memberAction === 'remove' ? (
                        <>Remove {memberSel.selected.size} member{memberSel.selected.size !== 1 ? 's' : ''} from this group? Their events stay on the relay.</>
                      ) : (
                        <>
                          Permanently delete every event authored by {memberSel.selected.size}{' '}
                          user{memberSel.selected.size !== 1 ? 's' : ''}, across all groups on this
                          relay. Group metadata, membership and roles are never deleted. There is
                          no undo. Type <strong>DELETE</strong> to confirm.
                        </>
                      )}
                    </p>
                    {memberAction === 'wipe' && (
                      <input
                        type="text"
                        class="mb-2"
                        value={memberConfirmText}
                        placeholder="DELETE"
                        aria-label="Type DELETE to confirm deleting these users' events"
                        onInput={e => setMemberConfirmText((e.target as HTMLInputElement).value)}
                      />
                    )}
                    <div class="flex items-center gap-2">
                      <button
                        type="button"
                        onClick={runMemberBulk}
                        disabled={memberBusy || (memberAction === 'wipe' && memberConfirmText.trim() !== 'DELETE')}
                        class="text-xs px-2 py-1 rounded"
                        style={{ background: 'rgba(239,68,68,0.2)', color: '#f87171', border: '1px solid rgba(239,68,68,0.4)' }}
                      >
                        {memberBusy ? 'Working…' : memberAction === 'remove' ? 'Confirm remove' : 'Confirm delete'}
                      </button>
                      <button
                        type="button"
                        onClick={() => { setMemberAction(null); setMemberConfirmText('') }}
                        class="text-xs"
                        style={{ color: 'var(--color-text-secondary)' }}
                      >
                        Cancel
                      </button>
                    </div>
                  </div>
                )}
              </div>
            )}

            <div class="mt-2 text-xs" style={{ color: 'var(--color-text-secondary)', flexShrink: 0 }}>
              {!membersLoading && !membersError && `${members.length} member${members.length !== 1 ? 's' : ''}`}
            </div>
          </div>
        )}
      </div>
    </div>
  )
}
