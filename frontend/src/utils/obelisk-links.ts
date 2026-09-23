import { nip19 } from 'nostr-tools'
import { ownRelayUrl } from '../constants'

/**
 * Links out of the console to a human-readable view of a Nostr thing.
 *
 * These used to point at njump.me, which meant every "who is this?" in the
 * console handed the reader — and the traffic — to a third party that has no
 * relationship with this relay. Obelisk serves both views itself now
 * (`/notes/<nevent>` and `/p/<npub>`), so they stay inside the project.
 *
 * `OBELISK_ORIGIN` is deliberately the hosted client rather than
 * `window.location.origin`: this relay serves an admin console, not a note
 * viewer, so a self-hosted deployment still has to send the reader somewhere
 * that can render one.
 */
const OBELISK_ORIGIN = 'https://obelisk.ar'

/** `/p/<npub>` — accepts npub, nprofile or bare hex at the other end. */
export function profileUrl(npub: string): string {
  return `${OBELISK_ORIGIN}/p/${npub}`
}

/**
 * `/notes/<nevent>` for one event.
 *
 * The relay hint is this relay, and it is the load-bearing part: a NIP-29
 * group event exists *here* and very likely nowhere else, so an `note1`
 * carrying only an id would send the viewer to look for it on the public
 * relays and find nothing. Falls back to the bare id if encoding fails, which
 * is still a working URL for anything that has propagated.
 */
export function noteUrl(id: string, author?: string | null): string {
  try {
    const nevent = nip19.neventEncode({
      id,
      author: author || undefined,
      relays: [ownRelayUrl()],
    })
    return `${OBELISK_ORIGIN}/notes/${nevent}`
  } catch {
    return `${OBELISK_ORIGIN}/notes/${id}`
  }
}
