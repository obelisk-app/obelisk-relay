import { useState } from 'preact/hooks'
import { adminApi, type ConfigResetResult } from '../../services/AdminApiClient'

type AccessPolicy = 'owner_only' | 'open'

const shortPath = (path: string) => (
  path.length > 64 ? `${path.slice(0, 24)}...${path.slice(-32)}` : path
)

export const RelaySettings = () => {
  const [accessPolicy, setAccessPolicy] = useState<AccessPolicy>('owner_only')
  const [keepOwnerReference, setKeepOwnerReference] = useState(true)
  const [confirm, setConfirm] = useState('')
  const [resetting, setResetting] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [result, setResult] = useState<ConfigResetResult | null>(null)

  const resetConfig = async () => {
    setResetting(true)
    setError(null)
    setResult(null)

    try {
      const response = await adminApi.resetRelayConfig({
        confirm,
        access_policy: accessPolicy,
        keep_owner_reference: keepOwnerReference,
      })
      setResult(response)
      setConfirm('')
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Reset failed')
    } finally {
      setResetting(false)
    }
  }

  return (
    <div class="space-y-6">
      <div>
        <h2 class="text-xl font-bold">Relay Settings</h2>
        <p class="text-sm mt-1" style={{ color: 'var(--color-text-secondary)' }}>
          Reset configuration while keeping relay event data.
        </p>
      </div>

      {error && (
        <div class="p-3 rounded-lg text-sm bg-red-500/10 text-red-400 border border-red-500/20">
          {error}
        </div>
      )}

      {result && (
        <div class="p-4 text-sm border" style={{ background: 'rgba(180,249,83,0.08)', borderColor: 'rgba(180,249,83,0.22)', borderRadius: '8px' }}>
          <div class="font-semibold" style={{ color: '#b4f953' }}>{result.message}</div>
          <div class="mt-3 grid sm:grid-cols-2 gap-3" style={{ color: 'var(--color-text-secondary)' }}>
            <div>Admin: <span class="font-mono">{result.admin_npub}</span></div>
            <div>Access: {result.access_policy === 'owner_only' ? 'Owner only' : 'Open relay'}</div>
            <div>Whitelist: {result.whitelisted_count}</div>
            <div>References: {result.reference_account_count}</div>
            <div class="sm:col-span-2">Backup: <span class="font-mono">{shortPath(result.backup_path)}</span></div>
          </div>
        </div>
      )}

      <section class="lc-card p-5 space-y-5">
        <div>
          <h3 class="text-lg font-semibold">Reset Configuration</h3>
          <p class="text-sm mt-1" style={{ color: 'var(--color-text-secondary)' }}>
            Clears access lists, follow-derived entries, references, blacklist, and runtime admin config. Groups and event history stay in the database.
          </p>
        </div>

        <div class="grid md:grid-cols-2 gap-3">
          <button
            onClick={() => setAccessPolicy('owner_only')}
            class="text-left p-4 transition-colors"
            style={{
              borderRadius: '8px',
              background: accessPolicy === 'owner_only' ? 'rgba(180,249,83,0.10)' : 'var(--color-bg-secondary)',
              border: accessPolicy === 'owner_only' ? '1px solid rgba(180,249,83,0.35)' : '1px solid var(--color-border)',
            }}
          >
            <div class="font-semibold" style={{ color: accessPolicy === 'owner_only' ? '#b4f953' : 'var(--color-text-primary)' }}>Owner only</div>
            <div class="mt-2 text-sm" style={{ color: 'var(--color-text-secondary)' }}>Keep this admin as the only allowed pubkey.</div>
          </button>
          <button
            onClick={() => setAccessPolicy('open')}
            class="text-left p-4 transition-colors"
            style={{
              borderRadius: '8px',
              background: accessPolicy === 'open' ? 'rgba(180,249,83,0.10)' : 'var(--color-bg-secondary)',
              border: accessPolicy === 'open' ? '1px solid rgba(180,249,83,0.35)' : '1px solid var(--color-border)',
            }}
          >
            <div class="font-semibold" style={{ color: accessPolicy === 'open' ? '#b4f953' : 'var(--color-text-primary)' }}>Open relay</div>
            <div class="mt-2 text-sm" style={{ color: 'var(--color-text-secondary)' }}>Clear the whitelist and accept authenticated pubkeys.</div>
          </button>
        </div>

        <label class="flex items-start gap-3 p-4 cursor-pointer" style={{ background: 'var(--color-bg-secondary)', border: '1px solid var(--color-border)', borderRadius: '8px' }}>
          <input
            type="checkbox"
            checked={keepOwnerReference}
            onChange={e => setKeepOwnerReference((e.target as HTMLInputElement).checked)}
            class="mt-1"
          />
          <span>
            <span class="block text-sm font-semibold">Keep owner as reference account</span>
            <span class="block text-sm mt-1" style={{ color: 'var(--color-text-secondary)' }}>Reference accounts are used for follow sync.</span>
          </span>
        </label>

        <div>
          <label class="block text-sm font-medium mb-2">Confirm reset</label>
          <input
            type="text"
            value={confirm}
            onInput={e => setConfirm((e.target as HTMLInputElement).value)}
            placeholder="Type RESET"
            class="w-full px-4 py-2 rounded-lg text-sm"
            style={{ background: 'var(--color-bg-tertiary)', color: 'var(--color-text-primary)', border: '1px solid var(--color-border)' }}
          />
        </div>

        <button
          onClick={resetConfig}
          disabled={confirm !== 'RESET' || resetting}
          class="lc-pill-primary text-sm"
          style={{ padding: '10px 18px', borderRadius: '8px' }}
        >
          {resetting ? 'Resetting...' : 'Reset Config'}
        </button>
      </section>
    </div>
  )
}
