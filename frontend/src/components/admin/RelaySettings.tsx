import { useEffect, useState } from 'preact/hooks'
import {
  adminApi,
  type AdminPubkeyEntry,
  type BackupEntry,
  type ConfigResetResult,
  type RelayIdentity,
} from '../../services/AdminApiClient'

type SettingsSection = 'whitelist' | 'storage' | 'groups'

interface RelaySettingsProps {
  onResetToSetup: (result: ConfigResetResult) => void
  onNavigate?: (section: SettingsSection) => void
}

const shortKey = (value: string) => (
  value.length > 20 ? `${value.slice(0, 10)}...${value.slice(-10)}` : value
)

const formatBytes = (bytes: number) => {
  if (bytes === 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  const index = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1)
  return `${(bytes / Math.pow(1024, index)).toFixed(index === 0 ? 0 : 1)} ${units[index]}`
}

const formatUnix = (unix: number) => {
  if (!unix) return 'Unknown'
  return new Date(unix * 1000).toLocaleString()
}

export const RelaySettings = ({ onResetToSetup, onNavigate }: RelaySettingsProps) => {
  const [identity, setIdentity] = useState<RelayIdentity | null>(null)
  const [identityForm, setIdentityForm] = useState({
    relay_name: '',
    relay_description: '',
    relay_url: '',
  })
  const [admins, setAdmins] = useState<AdminPubkeyEntry[]>([])
  const [newAdminPubkey, setNewAdminPubkey] = useState('')
  const [backups, setBackups] = useState<BackupEntry[]>([])
  const [confirmRemoveAdmin, setConfirmRemoveAdmin] = useState<string | null>(null)
  const [restoreTarget, setRestoreTarget] = useState<string | null>(null)
  const [restoreConfirm, setRestoreConfirm] = useState('')
  const [restartConfirm, setRestartConfirm] = useState('')
  const [rotateConfirm, setRotateConfirm] = useState('')
  const [resetConfirm, setResetConfirm] = useState('')
  const [loading, setLoading] = useState(true)
  const [busy, setBusy] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [toast, setToast] = useState<string | null>(null)

  const showToast = (message: string) => {
    setToast(message)
    setTimeout(() => setToast(null), 5000)
  }

  const loadSettings = async () => {
    setLoading(true)
    setError(null)
    try {
      const [identityData, adminData, backupData] = await Promise.all([
        adminApi.getRelayIdentity(),
        adminApi.getAdminPubkeys(),
        adminApi.getConfigBackups(),
      ])
      setIdentity(identityData)
      setIdentityForm({
        relay_name: identityData.relay_name,
        relay_description: identityData.relay_description,
        relay_url: identityData.relay_url,
      })
      setAdmins(adminData)
      setBackups(backupData)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to load relay settings')
    } finally {
      setLoading(false)
    }
  }

  useEffect(() => {
    void loadSettings()
  }, [])

  const saveIdentity = async () => {
    setBusy('identity')
    setError(null)
    try {
      const response = await adminApi.updateRelayIdentity(identityForm)
      setIdentity(response)
      showToast('Relay identity saved. Restart the relay to apply it.')
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to save relay identity')
    } finally {
      setBusy(null)
    }
  }

  const rotateRelayKey = async () => {
    setBusy('rotate-key')
    setError(null)
    try {
      const response = await adminApi.rotateRelayKey(rotateConfirm)
      setRotateConfirm('')
      setIdentity(prev => prev ? { ...prev, relay_pubkey: response.relay_pubkey, restart_required: true } : prev)
      showToast(response.message)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to rotate relay key')
    } finally {
      setBusy(null)
    }
  }

  const addAdmin = async () => {
    if (!newAdminPubkey.trim()) return
    setBusy('add-admin')
    setError(null)
    try {
      const entry = await adminApi.addAdminPubkey(newAdminPubkey.trim())
      setAdmins(prev => [...prev.filter(item => item.hex !== entry.hex), entry])
      setNewAdminPubkey('')
      showToast('Admin pubkey added')
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to add admin')
    } finally {
      setBusy(null)
    }
  }

  const removeAdmin = async (hex: string) => {
    setBusy(`remove-admin-${hex}`)
    setError(null)
    try {
      await adminApi.removeAdminPubkey(hex)
      setAdmins(prev => prev.filter(item => item.hex !== hex))
      setConfirmRemoveAdmin(null)
      showToast('Admin pubkey removed')
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to remove admin')
    } finally {
      setBusy(null)
    }
  }

  const downloadBackup = async (backup: BackupEntry) => {
    setBusy(`download-${backup.id}`)
    setError(null)
    try {
      const payload = await adminApi.downloadConfigBackup(backup.id)
      const blob = new Blob([JSON.stringify(payload, null, 2)], { type: 'application/json' })
      const url = URL.createObjectURL(blob)
      const a = document.createElement('a')
      a.href = url
      a.download = `${backup.id}.json`
      a.click()
      URL.revokeObjectURL(url)
      showToast('Backup downloaded')
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to download backup')
    } finally {
      setBusy(null)
    }
  }

  const restoreBackup = async (backup: BackupEntry) => {
    setBusy(`restore-${backup.id}`)
    setError(null)
    try {
      const response = await adminApi.restoreConfigBackup(backup.id, restoreConfirm)
      setRestoreConfirm('')
      setRestoreTarget(null)
      showToast(response.message)
      await loadSettings()
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to restore backup')
    } finally {
      setBusy(null)
    }
  }

  const restartRelay = async () => {
    setBusy('restart')
    setError(null)
    try {
      const response = await adminApi.restartRelay(restartConfirm)
      setRestartConfirm('')
      showToast(`${response.message}. The admin page may disconnect briefly.`)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Failed to restart relay')
      setBusy(null)
    }
  }

  const resetConfig = async () => {
    setBusy('reset')
    setError(null)

    try {
      const response = await adminApi.resetRelayConfig({ confirm: resetConfirm })
      setResetConfirm('')
      onResetToSetup(response)
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Reset failed')
    } finally {
      setBusy(null)
    }
  }

  return (
    <div class="space-y-6">
      <div>
        <h2 class="text-xl font-bold">Relay Settings</h2>
        <p class="text-sm mt-1" style={{ color: 'var(--color-text-secondary)' }}>
          Operational controls for identity, admins, backups, restart, and recovery.
        </p>
      </div>

      {toast && (
        <div class="p-3 rounded-lg text-sm border" style={{ background: 'rgba(180,249,83,0.08)', color: '#b4f953', borderColor: 'rgba(180,249,83,0.2)' }}>
          {toast}
        </div>
      )}

      {error && (
        <div class="p-3 rounded-lg text-sm bg-red-500/10 text-red-400 border border-red-500/20">
          {error}
        </div>
      )}

      <section class="admin-settings-card">
        <div class="admin-settings-card-header">
          <div>
            <h3>Where Settings Live</h3>
            <p>Use these sections for routine access, storage, and moderation changes.</p>
          </div>
        </div>

        <div class="admin-settings-link-grid mt-4">
          <button type="button" onClick={() => onNavigate?.('whitelist')} class="admin-mode-option">
            <span class="admin-mode-title">Access</span>
            <span class="admin-mode-copy">Open relay mode, whitelist enforcement, rate limits, allowed pubkeys, and blocked pubkeys.</span>
          </button>
          <button type="button" onClick={() => onNavigate?.('storage')} class="admin-mode-option">
            <span class="admin-mode-title">Storage</span>
            <span class="admin-mode-copy">Database size, pruning status, retention window, prune interval, and prune event kinds.</span>
          </button>
          <button type="button" onClick={() => onNavigate?.('groups')} class="admin-mode-option">
            <span class="admin-mode-title">Groups</span>
            <span class="admin-mode-copy">Group metadata, members, moderation actions, event browsing, and group deletion.</span>
          </button>
        </div>
      </section>

      {loading ? (
        <div class="space-y-3">
          <div class="lc-skeleton h-36 w-full" />
          <div class="lc-skeleton h-36 w-full" />
          <div class="lc-skeleton h-36 w-full" />
        </div>
      ) : (
        <>
          <section class="admin-settings-card">
            <div class="admin-settings-card-header">
              <div>
                <h3>Relay Identity</h3>
                <p>Controls NIP-11 name, description, advertised URL, and relay signing identity.</p>
              </div>
              {identity?.restart_required && <span class="admin-status-badge admin-status-badge-warn">Restart required</span>}
            </div>

            <div class="admin-rate-grid mt-4">
              <label>
                <span>Relay name</span>
                <input
                  type="text"
                  value={identityForm.relay_name}
                  onInput={e => setIdentityForm(prev => ({ ...prev, relay_name: (e.target as HTMLInputElement).value }))}
                />
              </label>
              <label>
                <span>Relay URL</span>
                <input
                  type="text"
                  value={identityForm.relay_url}
                  onInput={e => setIdentityForm(prev => ({ ...prev, relay_url: (e.target as HTMLInputElement).value }))}
                />
              </label>
              <label>
                <span>Active relay pubkey</span>
                <input type="text" value={identity?.relay_pubkey ?? ''} readonly />
              </label>
            </div>

            <label class="admin-textarea-field">
              <span>Relay description</span>
              <textarea
                value={identityForm.relay_description}
                onInput={e => setIdentityForm(prev => ({ ...prev, relay_description: (e.target as HTMLTextAreaElement).value }))}
              />
            </label>

            <div class="admin-settings-actions">
              <div class="text-sm" style={{ color: 'var(--color-text-secondary)' }}>
                Identity changes affect NIP-11 metadata and auth URL validation after restart.
              </div>
              <button
                type="button"
                onClick={saveIdentity}
                disabled={busy === 'identity'}
                class="lc-pill-primary text-sm"
                style={{ borderRadius: '8px', padding: '9px 18px' }}
              >
                {busy === 'identity' ? 'Saving...' : 'Save identity'}
              </button>
            </div>

            <div class="admin-danger-card">
              <div>
                <h4>Rotate Relay Key</h4>
                <p>Generates a new relay secret key and stores it in config. Existing relay-signed state uses the old pubkey until you restart.</p>
              </div>
              <div class="admin-danger-controls">
                <input
                  type="text"
                  value={rotateConfirm}
                  onInput={e => setRotateConfirm((e.target as HTMLInputElement).value)}
                  placeholder="Type ROTATE"
                  aria-label="Confirm key rotation by typing ROTATE"
                />
                <button
                  type="button"
                  onClick={rotateRelayKey}
                  disabled={rotateConfirm !== 'ROTATE' || busy === 'rotate-key'}
                  class="admin-danger-button"
                >
                  {busy === 'rotate-key' ? 'Rotating...' : 'Rotate key'}
                </button>
              </div>
            </div>
          </section>

          <section class="admin-settings-card">
            <div class="admin-settings-card-header">
              <div>
                <h3>Admin Pubkeys</h3>
                <p>Admins can sign into this panel and perform privileged relay operations.</p>
              </div>
              <span class="admin-status-badge">{admins.length}</span>
            </div>

            <div class="admin-access-input-row">
              <input
                type="text"
                value={newAdminPubkey}
                onInput={e => setNewAdminPubkey((e.target as HTMLInputElement).value)}
                onKeyDown={e => e.key === 'Enter' && addAdmin()}
                placeholder="npub1... or hex pubkey"
                class="admin-access-input"
              />
              <button
                type="button"
                onClick={addAdmin}
                disabled={!newAdminPubkey.trim() || busy === 'add-admin'}
                class="admin-access-button admin-access-button-allow"
              >
                {busy === 'add-admin' ? 'Adding...' : 'Add admin'}
              </button>
            </div>

            <div class="admin-list mt-4">
              {admins.map(admin => (
                <div class="admin-list-row" key={admin.hex}>
                  <div>
                    <strong>{shortKey(admin.npub || admin.hex)}</strong>
                    <p>{admin.current_session ? 'Current session' : admin.hex}</p>
                  </div>
                  {confirmRemoveAdmin === admin.hex ? (
                    <div class="admin-row-actions">
                      <button type="button" onClick={() => removeAdmin(admin.hex)} class="admin-text-danger">Confirm</button>
                      <button type="button" onClick={() => setConfirmRemoveAdmin(null)}>Cancel</button>
                    </div>
                  ) : (
                    <button
                      type="button"
                      onClick={() => setConfirmRemoveAdmin(admin.hex)}
                      disabled={admin.current_session}
                      class="admin-text-danger"
                    >
                      Remove
                    </button>
                  )}
                </div>
              ))}
            </div>
          </section>

          <section class="admin-settings-card">
            <div class="admin-settings-card-header">
              <div>
                <h3>Config Backups</h3>
                <p>Download or restore timestamped config backups created before reset or restore operations.</p>
              </div>
              <span class="admin-status-badge">{backups.length}</span>
            </div>

            {backups.length === 0 ? (
              <div class="mt-4 text-sm" style={{ color: 'var(--color-text-secondary)' }}>
                No config backups found.
              </div>
            ) : (
              <div class="admin-list mt-4">
                {backups.map(backup => (
                  <div class="admin-list-row admin-list-row-tall" key={backup.id}>
                    <div>
                      <strong>{backup.id}</strong>
                      <p>{formatUnix(backup.created_unix)} | {backup.file_count} files | {formatBytes(backup.size_bytes)}</p>
                      <p class="font-mono">{backup.path}</p>
                    </div>
                    <div class="admin-row-actions">
                      <button type="button" onClick={() => downloadBackup(backup)}>Download</button>
                      {restoreTarget === backup.id ? (
                        <>
                          <input
                            type="text"
                            value={restoreConfirm}
                            onInput={e => setRestoreConfirm((e.target as HTMLInputElement).value)}
                            placeholder="RESTORE"
                            class="admin-inline-input"
                          />
                          <button
                            type="button"
                            onClick={() => restoreBackup(backup)}
                            disabled={restoreConfirm !== 'RESTORE'}
                            class="admin-text-danger"
                          >
                            Restore
                          </button>
                          <button type="button" onClick={() => { setRestoreTarget(null); setRestoreConfirm('') }}>Cancel</button>
                        </>
                      ) : (
                        <button type="button" onClick={() => setRestoreTarget(backup.id)} class="admin-text-danger">
                          Restore
                        </button>
                      )}
                    </div>
                  </div>
                ))}
              </div>
            )}
          </section>

          <section class="admin-settings-card">
            <div class="admin-settings-card-header">
              <div>
                <h3>Restart Relay</h3>
                <p>Stops the relay process so Docker can restart it. Use after startup-only settings change.</p>
              </div>
              <span class="admin-status-badge admin-status-badge-warn">Disconnects clients</span>
            </div>

            <div class="admin-danger-card">
              <div>
                <h4>Confirm Restart</h4>
                <p>The admin panel may disconnect while the container exits and starts again.</p>
              </div>
              <div class="admin-danger-controls">
                <input
                  type="text"
                  value={restartConfirm}
                  onInput={e => setRestartConfirm((e.target as HTMLInputElement).value)}
                  placeholder="Type RESTART"
                  aria-label="Confirm restart by typing RESTART"
                />
                <button
                  type="button"
                  onClick={restartRelay}
                  disabled={restartConfirm !== 'RESTART' || busy === 'restart'}
                  class="admin-danger-button"
                >
                  {busy === 'restart' ? 'Restarting...' : 'Restart relay'}
                </button>
              </div>
            </div>
          </section>
        </>
      )}

      <section class="admin-settings-card">
        <div class="admin-settings-card-header">
          <div>
            <h3>Recovery Reset</h3>
            <p>Use this only when the relay access/admin configuration is broken or you want to rerun first setup.</p>
          </div>
          <span class="admin-status-badge admin-status-badge-warn">Destructive config change</span>
        </div>

        <div class="admin-reset-flow">
          <div class="admin-reset-step">
            <span>1</span>
            <strong>Back up current config</strong>
            <small>Creates a timestamped backup folder under the relay config directory.</small>
          </div>
          <div class="admin-reset-step">
            <span>2</span>
            <strong>Clear access runtime state</strong>
            <small>Clears sessions, runtime admins, manual whitelist, follow-derived whitelist, references, and blacklist.</small>
          </div>
          <div class="admin-reset-step">
            <span>3</span>
            <strong>Reopen first-run setup</strong>
            <small>Keeps the current owner pubkey as the only identity allowed to finish setup.</small>
          </div>
        </div>

        <div class="admin-reset-impact">
          <div>
            <div class="font-semibold">Kept</div>
            <p>Relay event history, group data, relay secret key, database contents, and current owner pubkey.</p>
          </div>
          <div>
            <div class="font-semibold">Cleared</div>
            <p>Admin sessions, runtime admins, whitelist entries, follow-derived entries, reference accounts, and blocked pubkeys.</p>
          </div>
        </div>

        <div class="admin-danger-card">
          <div>
            <h4>Confirm Recovery Reset</h4>
            <p>After reset, setup asks you to choose whitelist enforcement or open relay mode again.</p>
          </div>
          <div class="admin-danger-controls">
            <input
              type="text"
              value={resetConfirm}
              onInput={e => setResetConfirm((e.target as HTMLInputElement).value)}
              placeholder="Type RESET"
              aria-label="Confirm reset by typing RESET"
            />
            <button
              type="button"
              onClick={resetConfig}
              disabled={resetConfirm !== 'RESET' || busy === 'reset'}
              class="admin-danger-button"
            >
              {busy === 'reset' ? 'Resetting...' : 'Reset and reopen setup'}
            </button>
          </div>
        </div>
      </section>
    </div>
  )
}
