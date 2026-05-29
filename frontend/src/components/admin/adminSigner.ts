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
  await signer?.close?.()
  await clearPersistedNip46()
  await clearPersistedNsec()
}
