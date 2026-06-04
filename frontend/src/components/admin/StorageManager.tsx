import { useEffect, useState } from 'preact/hooks'
import { adminApi, type StorageSettings } from '../../services/AdminApiClient'

const DEFAULT_KINDS = [9, 11, 12]

const formatBytes = (bytes: number) => {
  if (bytes === 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const index = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1)
  return `${(bytes / Math.pow(1024, index)).toFixed(index === 0 ? 0 : 1)} ${units[index]}`
}

const formatUnix = (unix: number) => {
  if (!unix) return 'Never'
  return new Date(unix * 1000).toLocaleString()
}

const parseKinds = (value: string) => (
  value
    .split(',')
    .map(item => Number(item.trim()))
    .filter(kind => Number.isInteger(kind) && kind > 0)
)

export const StorageManager = () => {
  const [settings, setSettings] = useState<StorageSettings | null>(null)
  const [kindText, setKindText] = useState(DEFAULT_KINDS.join(', '))
  const [loading, setLoading] = useState(true)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [toast, setToast] = useState<string | null>(null)

  const load = () => {
    setLoading(true)
    adminApi.getStorageSettings()
      .then(data => {
        setSettings(data)
        setKindText(data.prune_kinds.join(', '))
        setError(null)
      })
      .catch(e => setError(e.message))
      .finally(() => setLoading(false))
  }

  useEffect(() => load(), [])

  const update = (patch: Partial<StorageSettings>) => {
    setSettings(prev => prev ? { ...prev, ...patch } : prev)
  }

  const save = async () => {
    if (!settings) return
    const pruneKinds = parseKinds(kindText)
    setSaving(true)
    setError(null)
    try {
      const next = await adminApi.updateStorageSettings({
        pruning_enabled: settings.configured_pruning_enabled,
        retention_days: settings.retention_days,
        prune_interval_minutes: settings.prune_interval_minutes,
        prune_kinds: pruneKinds,
      })
      setSettings(next)
      setKindText(next.prune_kinds.join(', '))
      setToast('Storage settings saved. Restart relay to apply pruning changes.')
      setTimeout(() => setToast(null), 4000)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to save storage settings')
    } finally {
      setSaving(false)
    }
  }

  return (
    <div>
      <h2 class="text-xl font-bold mb-2">Storage</h2>
      <p class="text-sm mb-6" style={{ color: 'var(--color-text-secondary)' }}>
        Review database usage and configure event pruning. Pruning is disabled by default.
      </p>

      {toast && (
        <div class="mb-4 p-3 rounded-lg text-sm border" style={{ background: 'rgba(180,249,83,0.08)', color: '#b4f953', borderColor: 'rgba(180,249,83,0.2)' }}>
          {toast}
        </div>
      )}

      {error && (
        <div class="mb-4 p-3 rounded-lg text-sm bg-red-500/10 text-red-400 border border-red-500/20">
          {error}
        </div>
      )}

      {loading || !settings ? (
        <div class="space-y-3">
          <div class="lc-skeleton h-28 w-full" />
          <div class="lc-skeleton h-56 w-full" />
        </div>
      ) : (
        <div class="space-y-5">
          <div class="admin-storage-stats">
            <div class="admin-stat-card">
              <span>Database size</span>
              <strong>{formatBytes(settings.db_size_bytes)}</strong>
            </div>
            <div class="admin-stat-card">
              <span>Files</span>
              <strong>{settings.db_file_count}</strong>
            </div>
            <div class="admin-stat-card">
              <span>Pruner status</span>
              <strong>{settings.pruning_enabled ? 'Running' : 'Disabled'}</strong>
            </div>
            <div class="admin-stat-card">
              <span>Events pruned</span>
              <strong>{settings.total_pruned}</strong>
            </div>
          </div>

          <section class="admin-settings-card">
            <div class="admin-settings-card-header">
              <div>
                <h3>Pruning</h3>
                <p>Deletes old event kinds after the configured retention window.</p>
              </div>
              <span class={`admin-status-badge ${settings.pruning_enabled ? 'admin-status-badge-ok' : ''}`}>
                {settings.pruning_enabled ? 'Active' : 'Disabled'}
              </span>
            </div>

            <label class="admin-toggle-row">
              <input
                type="checkbox"
                checked={settings.configured_pruning_enabled}
                onChange={e => update({ configured_pruning_enabled: (e.target as HTMLInputElement).checked })}
              />
              <span>
                <strong>Enable pruning after restart</strong>
                <small>Protected NIP-29 management and group-state kinds are never pruned.</small>
              </span>
            </label>

            {settings.configured_pruning_enabled && (
              <div class="admin-rate-grid mt-4">
                <label>
                  <span>Retention days</span>
                  <input
                    type="number"
                    min="1"
                    value={settings.retention_days}
                    onInput={e => update({ retention_days: Number((e.target as HTMLInputElement).value) })}
                  />
                </label>
                <label>
                  <span>Run every minutes</span>
                  <input
                    type="number"
                    min="1"
                    value={settings.prune_interval_minutes}
                    onInput={e => update({ prune_interval_minutes: Number((e.target as HTMLInputElement).value) })}
                  />
                </label>
                <label>
                  <span>Prune kinds</span>
                  <input
                    type="text"
                    value={kindText}
                    onInput={e => setKindText((e.target as HTMLInputElement).value)}
                  />
                </label>
              </div>
            )}

            <div class="admin-settings-actions">
              <div class="text-sm" style={{ color: 'var(--color-text-secondary)' }}>
                Last run: {formatUnix(settings.last_run_unix)} | Runs: {settings.runs}
              </div>
              <button
                type="button"
                onClick={save}
                disabled={saving}
                class="lc-pill-primary text-sm"
                style={{ borderRadius: '8px', padding: '9px 18px' }}
              >
                {saving ? 'Saving...' : 'Save storage settings'}
              </button>
            </div>
          </section>

          <section class="admin-settings-card">
            <div class="admin-settings-card-header">
              <div>
                <h3>Database Path</h3>
                <p class="font-mono break-all">{settings.db_path}</p>
              </div>
            </div>
          </section>
        </div>
      )}
    </div>
  )
}
