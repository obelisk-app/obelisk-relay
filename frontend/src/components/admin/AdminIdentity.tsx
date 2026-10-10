import { useEffect, useState } from 'preact/hooks'
import { nip19 } from 'nostr-tools'
import { fetchProfiles, type NostrProfile } from '../../services/ProfileFetcher'
import { LogoutIcon } from './icons'

/** `npub1abcdefgh…uvwxyz`, the same shortening the SFU and agents consoles use. */
function shortNpub(hex: string): string {
  let npub = hex
  try {
    npub = nip19.npubEncode(hex)
  } catch {
    // Not a valid hex key; show what we were given.
  }
  return npub.length > 20 ? `${npub.slice(0, 12)}…${npub.slice(-6)}` : npub
}

/** Round avatar with an initials monogram when there is no picture or it fails. */
function Avatar({ picture, label, size = 36 }: { picture?: string; label: string; size?: number }) {
  const [broken, setBroken] = useState(false)
  const initials = label
    .split(/[-_\s]+/)
    .filter(Boolean)
    .map(word => [...word][0])
    .join('')
    .slice(0, 2)
    .toUpperCase() || '?'
  const safe = picture && /^https?:\/\//i.test(picture) ? picture : undefined

  return (
    <span class="lc-avatar" style={{ width: `${size}px`, height: `${size}px`, borderRadius: '9999px' }}>
      {safe && !broken ? (
        <img
          src={safe}
          alt=""
          loading="lazy"
          referrerpolicy="no-referrer"
          style={{ borderRadius: '9999px' }}
          onError={() => setBroken(true)}
        />
      ) : (
        <span class="lc-avatar-mono" style={{ borderRadius: '9999px', fontSize: `${Math.max(11, Math.round(size * 0.36))}px` }}>
          {initials}
        </span>
      )}
    </span>
  )
}

/**
 * The signed-in admin, bottom-left of the sidebar: avatar, name, NIP-05 (or
 * short npub) and a sign-out button. The same card the SFU and agents consoles
 * carry, so the three Obelisk admin screens identify you the same way.
 */
export function AdminIdentity({ pubkey, onLogout }: { pubkey: string | null; onLogout: () => void }) {
  const [profile, setProfile] = useState<NostrProfile | undefined>()

  useEffect(() => {
    if (!pubkey) return
    let cancelled = false
    fetchProfiles([pubkey])
      .then(map => { if (!cancelled) setProfile(map.get(pubkey)) })
      .catch(() => undefined)
    return () => { cancelled = true }
  }, [pubkey])

  const name = profile?.display_name || profile?.name || 'Admin'
  const handle = profile?.nip05 || (pubkey ? shortNpub(pubkey) : '—')

  return (
    <div class="lc-identity">
      <Avatar picture={profile?.picture} label={name} />
      <div class="min-w-0 flex-1">
        <div class="text-sm font-semibold truncate" style={{ color: 'var(--color-text-primary)' }}>{name}</div>
        <div class="text-[11px] font-mono truncate" style={{ color: 'var(--color-text-secondary)' }} title={pubkey ?? undefined}>
          {handle}
        </div>
      </div>
      <button type="button" class="lc-icon-btn" title="Sign out" aria-label="Sign out" onClick={onLogout}>
        <LogoutIcon class="w-5 h-5" />
      </button>
    </div>
  )
}
