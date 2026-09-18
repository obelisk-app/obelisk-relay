import {
  SIGNER_STORAGE_KEY_NIP46,
  clearPersistedNip46,
  clearPersistedNsec,
} from '@nostr-wot/ui'
import type { NostrSigner } from '@nostr-wot/signers'
import { BunkerSigner, parseBunkerInput } from 'nostr-tools/nip46'
import { nip19 } from 'nostr-tools'

export const SIGNER_TIMEOUT_MS = 15000

interface StoredNip46Session {
  kind?: 'bunker' | 'nostrconnect'
  uri?: string
  bunkerPubkey?: string
  relays?: string[]
  clientNsec?: string
}

export const withTimeout = async <T,>(
  operation: Promise<T>,
  message: string,
  timeoutMs = SIGNER_TIMEOUT_MS,
): Promise<T> => {
  let timeoutId: ReturnType<typeof setTimeout> | undefined

  try {
    return await Promise.race([
      operation,
      new Promise<T>((_, reject) => {
        timeoutId = setTimeout(() => reject(new Error(message)), timeoutMs)
      }),
    ])
  } finally {
    if (timeoutId) clearTimeout(timeoutId)
  }
}

export const relayAuthUrl = () => window.location.origin.replace(/^http/, 'ws')

export const signAdminAuthEvent = async (
  signer: NostrSigner,
  challenge: string,
) => {
  const pubkey = await signer.getPublicKey()
  return signer.signEvent({
    kind: 22242,
    pubkey,
    created_at: Math.floor(Date.now() / 1000),
    tags: [
      ['relay', relayAuthUrl()],
      ['challenge', challenge],
    ],
    content: '',
  } as Parameters<NostrSigner['signEvent']>[0])
}

const nsecToBytes = (nsec: string): Uint8Array | null => {
  try {
    const decoded = nip19.decode(nsec)
    return decoded.type === 'nsec' ? decoded.data : null
  } catch {
    return null
  }
}

const storedNip46Session = (): StoredNip46Session | null => {
  if (typeof localStorage === 'undefined') return null
  const raw = localStorage.getItem(SIGNER_STORAGE_KEY_NIP46)
  if (!raw) return null

  try {
    return JSON.parse(raw) as StoredNip46Session
  } catch {
    return null
  }
}

export const restoreNip46SignerWithoutConnectReplay = async (): Promise<NostrSigner | null> => {
  const stored = storedNip46Session()
  if (!stored?.clientNsec) return null

  const clientSecret = nsecToBytes(stored.clientNsec)
  if (!clientSecret) return null

  const bp = stored.kind === 'bunker' && stored.uri
    ? await parseBunkerInput(stored.uri)
    : stored.bunkerPubkey && stored.relays?.length
      ? {
        pubkey: stored.bunkerPubkey,
        relays: stored.relays,
        secret: null,
      }
      : null

  if (!bp?.pubkey || bp.relays.length === 0) return null

  const signer = BunkerSigner.fromBunker(clientSecret, bp, {
    onauth: (url) => {
      if (typeof window !== 'undefined') {
        window.open(url, '_blank', 'width=600,height=700')
      }
    },
  })

  if (bp.secret) {
    await signer.connect()
  } else {
    await signer.getPublicKey()
  }

  return signer as unknown as NostrSigner
}

export const clearStoredSigners = async (signer?: NostrSigner | null) => {
  // Closing a signer that is already closed throws; that must not stop us
  // clearing the persisted session, which is the whole point of this call.
  try {
    await signer?.close?.()
  } catch {
    // Already gone.
  }
  await clearPersistedNip46()
  await clearPersistedNsec()
}

/**
 * Is this the error nostr-tools throws when a BunkerSigner's relay
 * subscription has gone away?
 *
 * A restored NIP-46 session points at a bunker connection that may no longer
 * exist -- the remote signer was closed, or the relay carrying the session
 * restarted. Every call then fails with "this signer is not open anymore,
 * create a new one", which is accurate but useless as a dead end: the stored
 * session keeps being restored on each load, so the same error returns until
 * someone knows to clear it by hand. Detecting it lets us drop the session and
 * fall back to the login widget, which is what "create a new one" means.
 */
export const isDeadSignerError = (e: unknown): boolean => {
  const message = e instanceof Error ? e.message : String(e ?? '')
  return /not open anymore|signer is closed|no longer open/i.test(message)
}
