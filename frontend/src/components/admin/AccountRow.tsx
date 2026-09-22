import type { ComponentChildren } from 'preact'
import { getDisplayName, type NostrProfile } from '../../services/ProfileFetcher'
import { CopyNpubButton } from './ProfileCard'

export interface AccountRowProps {
  hex: string
  npub: string
  profile: NostrProfile | undefined
  /** Opens the profile card. */
  onInspect: () => void
  /** Small text under the name — the tier source, hop count, role. */
  subtitle?: ComponentChildren
  /** Right-hand controls. */
  actions?: ComponentChildren
  /** Bulk-selection checkbox, when the list supports it. */
  selection?: { checked: boolean; onToggle: (shiftKey: boolean) => void; label: string }
}

const truncate = (s: string) => (s.length > 16 ? `${s.slice(0, 8)}...${s.slice(-8)}` : s)

/**
 * One account, rendered the same way everywhere.
 *
 * There were three of these — the allowlist table row, the blacklist card, and
 * the reference-account card — each with its own avatar-with-onError-hide, its
 * own `getDisplayName` call, its own truncation and its own copy button, and
 * three different visual treatments for the same object. An operator moving
 * between Access and References was reading the same thing twice in two shapes.
 *
 * Identity is one line: a name if a profile resolved, otherwise a truncated
 * npub, with the hex available on hover and behind the copy button. Printing
 * name, npub fragment and hex fragment simultaneously — which the allowlist
 * table did — gives three ways to read one account and no way to tell at a
 * glance whether two rows are the same person.
 */
export const AccountRow = ({
  hex,
  npub,
  profile,
  onInspect,
  subtitle,
  actions,
  selection,
}: AccountRowProps) => {
  const name = getDisplayName(profile, npub)

  return (
    <div class="admin-account-row">
      {selection && (
        <input
          type="checkbox"
          aria-label={selection.label}
          checked={selection.checked}
          onClick={e => selection.onToggle((e as MouseEvent).shiftKey)}
        />
      )}

      <button class="admin-account-identity" onClick={onInspect} title={npub || hex}>
        {profile?.picture ? (
          <img
            src={profile.picture}
            alt=""
            referrerpolicy="no-referrer"
            class="admin-account-avatar"
            // A broken avatar must not leave a torn image icon in a table.
            onError={e => ((e.currentTarget as HTMLImageElement).style.display = 'none')}
          />
        ) : (
          <span class="admin-account-avatar admin-account-avatar-blank" aria-hidden="true" />
        )}
        <span class="admin-account-text">
          <span class="admin-account-name">{name}</span>
          {subtitle && <span class="admin-account-sub">{subtitle}</span>}
        </span>
      </button>

      <span class="admin-account-npub" title={hex}>
        {truncate(npub || hex)}
      </span>

      {npub && <CopyNpubButton npub={npub} />}
      {actions && <span class="admin-account-actions">{actions}</span>}
    </div>
  )
}
