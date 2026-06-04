import { useState, useEffect } from 'preact/hooks'
import { ShootingStars } from './ShootingStars'

interface RelayInfo {
  name: string
  description: string
  group_count: number
  supported_nips: number[]
}

export const LandingPage = (_props: { path?: string }) => {
  const [info, setInfo] = useState<RelayInfo | null>(null)
  const [copied, setCopied] = useState(false)
  const [online, setOnline] = useState<boolean | null>(null)

  const wsUrl = `${window.location.protocol === 'https:' ? 'wss:' : 'ws:'}//${window.location.host}`

  useEffect(() => {
    fetch('/api/relay-info')
      .then(r => r.json())
      .then(setInfo)
      .catch(() => {})

    try {
      const ws = new WebSocket(wsUrl)
      ws.onopen = () => { setOnline(true); ws.close() }
      ws.onerror = () => setOnline(false)
      setTimeout(() => ws.close(), 5000)
    } catch {
      setOnline(false)
    }
  }, [])

  const copyUrl = () => {
    navigator.clipboard.writeText(wsUrl).then(() => {
      setCopied(true)
      setTimeout(() => setCopied(false), 2000)
    })
  }

  return (
    <div class="min-h-screen flex flex-col relative overflow-hidden" style={{ background: 'var(--color-bg-primary)', color: 'var(--color-text-primary)' }}>
      {/* Shooting Stars Background */}
      <ShootingStars count={6} />

      {/* Grid Overlay */}
      <div class="fixed inset-0 lc-grid-bg pointer-events-none z-0" />

      {/* Glow backdrop */}
      <div class="fixed top-1/4 left-1/2 -translate-x-1/2 -translate-y-1/2 w-[600px] h-[600px] rounded-full pointer-events-none z-0"
        style={{ background: 'radial-gradient(circle, rgba(180,249,83,0.06) 0%, transparent 70%)' }} />

      {/* Hero */}
      <div class="flex-1 flex flex-col items-center justify-center px-4 py-16 relative z-10">
        <div class="max-w-2xl w-full text-center">
          {/* Status Badge */}
          <div class="mb-8 animate-fade-in-up">
            <div class="inline-flex items-center gap-2 px-4 py-1.5 rounded-full text-sm font-medium"
              style={{ background: 'rgba(180,249,83,0.08)', border: '1px solid rgba(180,249,83,0.2)' }}>
              <span class={`w-2 h-2 rounded-full ${online === true ? 'bg-lc-green' : online === false ? 'bg-red-400' : 'bg-yellow-400'}`}
                style={online === true ? { boxShadow: '0 0 8px rgba(180,249,83,0.6)' } : {}} />
              <span style={{ color: online === true ? '#b4f953' : 'var(--color-text-secondary)' }}>
                {online === true ? 'Online' : online === false ? 'Offline' : 'Checking...'}
              </span>
            </div>
          </div>

          {/* Title */}
          <h1 class="text-5xl md:text-6xl font-extrabold mb-4 lc-glow-text animate-fade-in-up"
            style={{ animationDelay: '0.1s' }}>
            <span style={{ color: '#b4f953' }}>Obelisk</span> Relay
          </h1>
          <p class="text-lg md:text-xl mb-10 animate-fade-in-up" style={{ color: 'var(--color-text-secondary)', animationDelay: '0.2s' }}>
            {info?.description || 'NIP-29 groups relay for Obelisk. Auth-required, whitelisted access.'}
          </p>

          {/* Stats Cards */}
          <div class="grid grid-cols-2 gap-4 mb-10 max-w-md mx-auto animate-fade-in-up" style={{ animationDelay: '0.3s' }}>
            <div class="lc-card p-5 text-center">
              <div class="text-3xl font-bold" style={{ color: '#b4f953' }}>{info?.group_count ?? '—'}</div>
              <div class="text-sm mt-1" style={{ color: 'var(--color-text-secondary)' }}>Groups</div>
            </div>
            <div class="lc-card p-5 text-center">
              <div class="text-3xl font-bold" style={{ color: '#b4f953' }}>{info?.supported_nips?.length ?? '—'}</div>
              <div class="text-sm mt-1" style={{ color: 'var(--color-text-secondary)' }}>Supported NIPs</div>
            </div>
          </div>

          {/* Connection Info */}
          <div class="lc-card p-5 mb-10 animate-fade-in-up" style={{ animationDelay: '0.4s' }}>
            <div class="text-sm mb-3 font-medium" style={{ color: 'var(--color-text-secondary)' }}>Connect your Nostr client:</div>
            <div class="flex items-center gap-3 justify-center flex-wrap">
              <code class="text-sm px-4 py-2 rounded-lg font-mono" style={{ background: 'var(--color-bg-primary)', border: '1px solid var(--color-border)' }}>
                {wsUrl}
              </code>
              <button onClick={copyUrl} class="lc-pill-primary text-sm" style={{ padding: '8px 20px' }}>
                {copied ? 'Copied!' : 'Copy'}
              </button>
            </div>
          </div>

          {/* NIP Badges */}
          {info?.supported_nips && (
            <div class="mb-10 animate-fade-in-up" style={{ animationDelay: '0.5s' }}>
              <div class="text-sm mb-3 font-medium" style={{ color: 'var(--color-text-secondary)' }}>Supported NIPs:</div>
              <div class="flex flex-wrap gap-2 justify-center">
                {info.supported_nips.map(nip => (
                  <span key={nip} class="px-3 py-1 rounded-full text-xs font-mono font-medium"
                    style={{ background: 'rgba(180,249,83,0.08)', border: '1px solid rgba(180,249,83,0.15)', color: '#b4f953' }}>
                    NIP-{nip}
                  </span>
                ))}
              </div>
            </div>
          )}

          {/* CTA Buttons */}
          <div class="flex gap-4 justify-center animate-fade-in-up" style={{ animationDelay: '0.6s' }}>
            <a href="https://dex.obelisk.ar" target="_blank" rel="noopener noreferrer" class="lc-pill-primary text-base">
              Open Chat
            </a>
            <a href="/docs" class="lc-pill-secondary text-base">
              Docs
            </a>
            <a href="/admin" class="lc-pill-secondary text-base">
              Admin Panel
            </a>
          </div>
        </div>
      </div>

      {/* Floating particles */}
      <div class="fixed inset-0 pointer-events-none z-0 overflow-hidden">
        {[...Array(4)].map((_, i) => (
          <div key={i} class="absolute w-1 h-1 rounded-full"
            style={{
              background: '#b4f953',
              opacity: 0.2,
              left: `${20 + i * 20}%`,
              top: `${30 + i * 15}%`,
              animation: `glow-pulse ${3 + i}s ease-in-out infinite`,
              animationDelay: `${i * 0.8}s`,
            }} />
        ))}
      </div>

      {/* Footer */}
      <footer class="py-5 text-center text-sm relative z-10" style={{ color: 'var(--color-text-secondary)', borderTop: '1px solid var(--color-border)' }}>
        Powered by <span style={{ color: '#b4f953' }}>Obelisk</span> &middot; NIP-29 Group Relay
      </footer>
    </div>
  )
}
