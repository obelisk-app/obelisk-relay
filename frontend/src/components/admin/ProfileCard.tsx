import { useEffect } from 'preact/hooks'
import type { NostrProfile } from '../../services/ProfileFetcher'

interface ProfileCardProps {
  profile: NostrProfile | undefined
  hex: string
  npub: string
  onClose: () => void
}

const CopyButton = ({ text, label }: { text: string; label: string }) => {
  const handleCopy = () => {
    navigator.clipboard.writeText(text).then(() => {
      const btn = document.getElementById(`copy-${label}`)
      if (btn) {
        btn.textContent = 'Copied!'
        setTimeout(() => { btn.textContent = label }, 1500)
      }
    })
  }

  return (
    <button
      id={`copy-${label}`}
      onClick={handleCopy}
      class="text-xs px-2 py-1 rounded transition-colors flex-shrink-0"
      style={{ background: 'var(--color-bg-tertiary)', color: 'var(--color-text-secondary)', border: '1px solid var(--color-border)' }}
    >
      {label}
    </button>
  )
}

export const ProfileCard = ({ profile, hex, npub, onClose }: ProfileCardProps) => {
  useEffect(() => {
    const handleEsc = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose()
    }
    window.addEventListener('keydown', handleEsc)
    return () => window.removeEventListener('keydown', handleEsc)
  }, [onClose])

  return (
    <div
      class="fixed inset-0 z-50 flex items-center justify-center"
      style={{ background: 'rgba(0,0,0,0.6)', backdropFilter: 'blur(4px)' }}
      onClick={(e) => { if (e.target === e.currentTarget) onClose() }}
    >
      <div class="lc-card p-6 w-full max-w-md mx-4" style={{ maxHeight: '90vh', overflow: 'auto' }}>
        {/* Header with close */}
        <div class="flex justify-end mb-4">
          <button
            onClick={onClose}
            class="text-sm px-2 py-1 rounded transition-colors"
            style={{ color: 'var(--color-text-secondary)' }}
          >
            Close
          </button>
        </div>

        {/* Profile picture */}
        <div class="flex flex-col items-center mb-6">
          {profile?.picture ? (
            <img
              src={profile.picture}
              alt=""
              class="w-20 h-20 rounded-full object-cover mb-3"
              style={{ border: '3px solid rgba(180,249,83,0.3)' }}
              onError={(e) => { (e.target as HTMLImageElement).style.display = 'none' }}
            />
          ) : (
            <div
              class="w-20 h-20 rounded-full flex items-center justify-center text-2xl font-bold mb-3"
              style={{ background: 'rgba(180,249,83,0.1)', color: '#b4f953', border: '3px solid rgba(180,249,83,0.3)' }}
            >
              {(profile?.name || npub.slice(5, 7) || '??').slice(0, 2).toUpperCase()}
            </div>
          )}
          {profile?.display_name && (
            <div class="text-lg font-bold">{profile.display_name}</div>
          )}
          {profile?.name && (
            <div class="text-sm" style={{ color: 'var(--color-text-secondary)' }}>@{profile.name}</div>
          )}
          {profile?.nip05 && (
            <div class="text-xs mt-1" style={{ color: '#b4f953' }}>{profile.nip05}</div>
          )}
        </div>

        {/* npub */}
        <div class="mb-3">
          <div class="text-xs font-medium mb-1" style={{ color: 'var(--color-text-secondary)' }}>npub</div>
          <div class="flex items-center gap-2">
            <div
              class="flex-1 text-xs font-mono p-2 rounded overflow-hidden"
              style={{ background: 'var(--color-bg-tertiary)', wordBreak: 'break-all' }}
            >
              {npub}
            </div>
            <CopyButton text={npub} label="Copy npub" />
          </div>
        </div>

        {/* hex */}
        <div class="mb-4">
          <div class="text-xs font-medium mb-1" style={{ color: 'var(--color-text-secondary)' }}>hex</div>
          <div class="flex items-center gap-2">
            <div
              class="flex-1 text-xs font-mono p-2 rounded overflow-hidden"
              style={{ background: 'var(--color-bg-tertiary)', wordBreak: 'break-all' }}
            >
              {hex}
            </div>
            <CopyButton text={hex} label="Copy hex" />
          </div>
        </div>

        {/* External link */}
        <a
          href={`https://njump.me/${npub}`}
          target="_blank"
          rel="noopener noreferrer"
          class="block w-full text-center text-sm py-2 rounded-lg transition-colors"
          style={{ background: 'rgba(180,249,83,0.1)', color: '#b4f953', border: '1px solid rgba(180,249,83,0.2)' }}
        >
          View on njump.me
        </a>
      </div>
    </div>
  )
}

export const CopyNpubButton = ({ npub }: { npub: string }) => {
  const handleCopy = (e: Event) => {
    e.stopPropagation()
    const target = e.currentTarget as HTMLButtonElement
    navigator.clipboard.writeText(npub).then(() => {
      target.innerHTML = '&#10003;'
      setTimeout(() => {
        target.innerHTML = '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>'
      }, 1500)
    })
  }

  return (
    <button
      onClick={handleCopy}
      class="inline-flex items-center justify-center rounded transition-colors ml-2 flex-shrink-0"
      style={{ width: '22px', height: '22px', background: 'var(--color-bg-tertiary)', color: 'var(--color-text-secondary)', border: '1px solid var(--color-border)' }}
      title="Copy npub"
      dangerouslySetInnerHTML={{ __html: '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>' }}
    />
  )
}
