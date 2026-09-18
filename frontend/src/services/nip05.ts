/**
 * Resolve a NIP-05 identifier (`name@domain`) to a hex pubkey.
 *
 * Shared rather than per-screen: this used to live inside WhitelistManager, so
 * the Access field accepted `alice@example.com` while the otherwise identical
 * Reference Accounts field silently rejected it. An operator has no way to know
 * which inputs are the clever ones.
 */
export async function resolveNip05(identifier: string): Promise<string> {
  const [name, domain] = identifier.split('@')
  if (!name || !domain) throw new Error('Invalid NIP-05 format')

  const url = `https://${domain}/.well-known/nostr.json?name=${encodeURIComponent(name)}`
  const res = await fetch(url)
  if (!res.ok) throw new Error(`NIP-05 lookup failed: ${res.status}`)

  const data = await res.json()
  const hex = data?.names?.[name]
  if (!hex) throw new Error(`No pubkey found for ${identifier}`)
  return hex
}

/** Does this look like a NIP-05 address rather than an npub or hex key? */
export const looksLikeNip05 = (value: string) =>
  value.includes('@') && !value.startsWith('npub')

/**
 * Accept an npub, a hex key, or a NIP-05 address, returning whatever the admin
 * API should be given.
 */
export async function resolvePubkeyInput(value: string): Promise<string> {
  const trimmed = value.trim()
  return looksLikeNip05(trimmed) ? resolveNip05(trimmed) : trimmed
}
