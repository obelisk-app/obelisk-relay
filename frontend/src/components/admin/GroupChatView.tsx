import type { EventInfo } from '../../services/AdminApiClient'

/**
 * Chat-shaped rendering of a group's events for moderation.
 *
 * A flat log of rows is unworkable for deciding what to delete — you cannot see
 * who said what to whom. Content events render as conversation bubbles grouped
 * by author; everything else (joins, role changes, metadata edits) renders as an
 * inline system line, so moderation actions read in context rather than being
 * interleaved as look-alike rows.
 */

/** Kinds that are group *content* — the actual conversation. */
const CONTENT_KINDS = new Set([9, 10, 11, 12, 1111])

const SYSTEM_LABELS: Record<number, string> = {
  9000: 'added a user',
  9001: 'removed a user',
  9002: 'edited group metadata',
  9003: 'created a thread',
  9004: 'updated a thread',
  9005: 'deleted an event',
  9006: 'set roles',
  9007: 'created the group',
  9008: 'deleted the group',
  9009: 'created an invite',
  9021: 'requested to join',
  9022: 'left the group',
  39000: 'group metadata updated',
  39001: 'group admins updated',
  39002: 'group members updated',
  39003: 'group roles updated',
  5: 'requested a deletion',
  7: 'reacted',
}

const systemLabel = (kind: number) => SYSTEM_LABELS[kind] ?? `kind ${kind} event`

const short = (s: string, n = 8) => `${s.slice(0, n)}…`

/** Deterministic colour per author so the eye can track speakers. */
const authorHue = (pubkey: string) => {
  let h = 0
  for (let i = 0; i < pubkey.length; i += 1) h = (h * 31 + pubkey.charCodeAt(i)) % 360
  return h
}

const timeOf = (ts: number) =>
  new Date(ts * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })

const dayOf = (ts: number) => new Date(ts * 1000).toDateString()

const dayLabel = (ts: number) => {
  const today = new Date().toDateString()
  const yesterday = new Date(Date.now() - 86400_000).toDateString()
  const d = dayOf(ts)
  if (d === today) return 'Today'
  if (d === yesterday) return 'Yesterday'
  return new Date(ts * 1000).toLocaleDateString()
}

interface Props {
  /** Newest-first, as returned by the API. */
  events: EventInfo[]
  selected: Set<string>
  onToggle: (id: string, index: number, shiftKey: boolean) => void
  onFilterAuthor: (pubkey: string) => void
  activeAuthor: string | null
}

export const GroupChatView = ({
  events,
  selected,
  onToggle,
  onFilterAuthor,
  activeAuthor,
}: Props) => {
  // Oldest at the top, newest at the bottom — how a conversation reads.
  const ordered = [...events].sort((a, b) => a.created_at - b.created_at)

  return (
    <div class="flex flex-col gap-0.5 py-2">
      {ordered.map((ev, i) => {
        const prev = ordered[i - 1]
        const newDay = !prev || dayOf(prev.created_at) !== dayOf(ev.created_at)
        const isContent = CONTENT_KINDS.has(ev.kind)
        const isSelected = selected.has(ev.id)
        // Collapse consecutive messages from the same author.
        const sameAuthorRun =
          !newDay &&
          prev &&
          prev.pubkey === ev.pubkey &&
          CONTENT_KINDS.has(prev.kind) &&
          isContent &&
          ev.created_at - prev.created_at < 300

        // `index` is the position in the rendered order, which is what a
        // shift-click range has to be computed against.
        const row = (children: preact.ComponentChildren) => (
          <div
            key={ev.id}
            class="group flex items-start gap-2 px-2 rounded"
            style={{ background: isSelected ? 'rgba(180,249,83,0.07)' : undefined }}
          >
            <input
              type="checkbox"
              class="mt-1.5 flex-shrink-0"
              checked={isSelected}
              aria-label={`Select event ${ev.id}`}
              onClick={e => onToggle(ev.id, i, (e as MouseEvent).shiftKey)}
            />
            <div class="min-w-0 flex-1">{children}</div>
          </div>
        )

        return (
          <>
            {newDay && (
              <div class="flex items-center gap-3 px-2 py-3">
                <div class="flex-1" style={{ borderTop: '1px solid var(--color-border)' }} />
                <span class="text-xs" style={{ color: 'var(--color-text-secondary)' }}>
                  {dayLabel(ev.created_at)}
                </span>
                <div class="flex-1" style={{ borderTop: '1px solid var(--color-border)' }} />
              </div>
            )}

            {isContent
              ? row(
                  <div class={sameAuthorRun ? '' : 'mt-2'}>
                    {!sameAuthorRun && (
                      <div class="flex items-baseline gap-2 flex-wrap">
                        <button
                          type="button"
                          onClick={() => onFilterAuthor(ev.pubkey)}
                          class="text-sm font-semibold"
                          title={`Filter by ${ev.pubkey}`}
                          style={{
                            color: `hsl(${authorHue(ev.pubkey)}, 65%, 68%)`,
                            background: 'none',
                            border: 'none',
                            padding: 0,
                            cursor: 'pointer',
                            textDecoration: activeAuthor === ev.pubkey ? 'underline' : undefined,
                          }}
                        >
                          {short(ev.pubkey, 10)}
                        </button>
                        <span class="text-xs" style={{ color: 'var(--color-text-secondary)' }}>
                          {timeOf(ev.created_at)}
                        </span>
                        {ev.kind !== 9 && (
                          <span
                            class="text-xs px-1.5 rounded"
                            style={{ background: 'var(--color-bg-tertiary)', color: 'var(--color-text-secondary)' }}
                          >
                            {ev.kind === 11 ? 'thread' : ev.kind === 12 ? 'reply' : `kind ${ev.kind}`}
                          </span>
                        )}
                      </div>
                    )}
                    <div
                      class="text-sm whitespace-pre-wrap break-words"
                      style={{ color: 'var(--color-text-primary)' }}
                      title={ev.id}
                    >
                      {ev.content || (
                        <span style={{ color: 'var(--color-text-secondary)', fontStyle: 'italic' }}>
                          empty
                        </span>
                      )}
                    </div>
                  </div>,
                )
              : row(
                  <div class="flex items-baseline gap-2 flex-wrap py-0.5">
                    <span class="text-xs" style={{ color: 'var(--color-text-secondary)' }}>
                      <button
                        type="button"
                        onClick={() => onFilterAuthor(ev.pubkey)}
                        class="font-mono"
                        style={{
                          color: `hsl(${authorHue(ev.pubkey)}, 45%, 60%)`,
                          background: 'none',
                          border: 'none',
                          padding: 0,
                          cursor: 'pointer',
                        }}
                      >
                        {short(ev.pubkey)}
                      </button>
                      {' '}
                      {systemLabel(ev.kind)}
                      {ev.content ? ` — ${ev.content.slice(0, 80)}` : ''}
                    </span>
                    <span class="text-xs" style={{ color: 'var(--color-text-secondary)', opacity: 0.6 }}>
                      {timeOf(ev.created_at)}
                    </span>
                  </div>,
                )}
          </>
        )
      })}
    </div>
  )
}
