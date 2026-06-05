export class AdminApiClient {
  private baseUrl: string

  constructor(baseUrl: string = '') {
    this.baseUrl = baseUrl
  }

  private getToken(): string | null {
    return sessionStorage.getItem('admin_token')
  }

  private setToken(token: string) {
    sessionStorage.setItem('admin_token', token)
  }

  clearToken() {
    sessionStorage.removeItem('admin_token')
  }

  hasToken(): boolean {
    return this.getToken() !== null
  }

  private async request<T>(path: string, options: RequestInit = {}): Promise<T> {
    const token = this.getToken()
    const headers: Record<string, string> = {
      'Content-Type': 'application/json',
      ...(options.headers as Record<string, string> || {}),
    }
    if (token) {
      headers['Authorization'] = `Bearer ${token}`
    }

    const res = await fetch(`${this.baseUrl}${path}`, { ...options, headers })

    if (res.status === 401) {
      this.clearToken()
      throw new Error('Session expired')
    }

    if (!res.ok) {
      const body = await res.json().catch(() => ({ error: res.statusText }))
      throw new Error(body.error || `HTTP ${res.status}`)
    }

    if (res.status === 204) return undefined as T
    return res.json()
  }

  async getChallenge(): Promise<{ challenge: string }> {
    return this.request('/api/admin/challenge')
  }

  async getSetupStatus(): Promise<SetupStatus> {
    return this.request('/api/admin/setup/status')
  }

  async bootstrapSetup(options: {
    signed_event: any
    access_policy: 'owner_only' | 'open'
    add_owner_reference: boolean
  }): Promise<SetupResult> {
    const result = await this.request<SetupResult>('/api/admin/setup', {
      method: 'POST',
      body: JSON.stringify(options),
    })
    this.setToken(result.token)
    return result
  }

  async authenticate(signedEvent: any): Promise<{ token: string }> {
    const result = await this.request<{ token: string }>('/api/admin/auth', {
      method: 'POST',
      body: JSON.stringify({ signed_event: signedEvent }),
    })
    this.setToken(result.token)
    return result
  }

  async checkSession(): Promise<{ valid: boolean; pubkey?: string }> {
    return this.request('/api/admin/session')
  }

  async getWhitelist(): Promise<Array<{ hex: string; npub: string }>> {
    return this.request('/api/admin/whitelist')
  }

  async addToWhitelist(pubkey: string): Promise<{ hex: string; npub: string }> {
    return this.request('/api/admin/whitelist', {
      method: 'POST',
      body: JSON.stringify({ pubkey }),
    })
  }

  async removeFromWhitelist(hex: string): Promise<void> {
    return this.request(`/api/admin/whitelist/${hex}`, { method: 'DELETE' })
  }

  async getGroups(): Promise<Array<{
    id: string
    name: string
    about: string | null
    picture: string | null
    banner: string | null
    parent: string | null
    channel_kind: string | null
    member_count: number
    admin_count: number
    private: boolean
    closed: boolean
    broadcast: boolean
    metadata_tags: string[][]
  }>> {
    return this.request('/api/admin/groups')
  }

  async getStats(): Promise<{
    active_connections: number
    total_groups: number
    total_members: number
    whitelisted_count: number
    uptime_seconds: number
  }> {
    return this.request('/api/admin/stats')
  }

  async getAccessSettings(): Promise<AccessSettings> {
    return this.request('/api/admin/access-settings')
  }

  async updateAccessSettings(settings: {
    access_policy: 'owner_only' | 'open'
    pubkey_rate_limit_per_minute: number
    connection_rate_limit_per_minute: number
    global_rate_limit_per_minute: number
  }): Promise<AccessSettings> {
    return this.request('/api/admin/access-settings', {
      method: 'POST',
      body: JSON.stringify(settings),
    })
  }

  async getStorageSettings(): Promise<StorageSettings> {
    return this.request('/api/admin/storage-settings')
  }

  async updateStorageSettings(settings: {
    pruning_enabled: boolean
    retention_days: number
    prune_interval_minutes: number
    prune_kinds: number[]
  }): Promise<StorageSettings> {
    return this.request('/api/admin/storage-settings', {
      method: 'POST',
      body: JSON.stringify(settings),
    })
  }

  async getObeliskIndexSettings(): Promise<ObeliskIndexSettings> {
    return this.request('/api/admin/obelisk-index-settings')
  }

  async updateObeliskIndexSettings(settings: ObeliskIndexSettingsRequest): Promise<ObeliskIndexSettings> {
    return this.request('/api/admin/obelisk-index-settings', {
      method: 'POST',
      body: JSON.stringify(settings),
    })
  }

  async getReferenceAccounts(): Promise<Array<{ hex: string; npub: string }>> {
    return this.request('/api/admin/reference-accounts')
  }

  async addReferenceAccount(pubkey: string): Promise<{ hex: string; npub: string }> {
    return this.request('/api/admin/reference-accounts', {
      method: 'POST',
      body: JSON.stringify({ pubkey }),
    })
  }

  async removeReferenceAccount(hex: string): Promise<void> {
    return this.request(`/api/admin/reference-accounts/${hex}`, { method: 'DELETE' })
  }

  async syncFollows(): Promise<{ derived_count: number; message: string }> {
    return this.request('/api/admin/reference-accounts/sync', { method: 'POST' })
  }

  async resetRelayConfig(options: {
    confirm: string
  }): Promise<ConfigResetResult> {
    return this.request('/api/admin/config/reset', {
      method: 'POST',
      body: JSON.stringify(options),
    })
  }

  async restartRelay(confirm: string): Promise<RestartRelayResult> {
    return this.request('/api/admin/restart', {
      method: 'POST',
      body: JSON.stringify({ confirm }),
    })
  }

  async getAdminPubkeys(): Promise<AdminPubkeyEntry[]> {
    return this.request('/api/admin/admin-pubkeys')
  }

  async addAdminPubkey(pubkey: string): Promise<AdminPubkeyEntry> {
    return this.request('/api/admin/admin-pubkeys', {
      method: 'POST',
      body: JSON.stringify({ pubkey }),
    })
  }

  async removeAdminPubkey(hex: string): Promise<void> {
    return this.request(`/api/admin/admin-pubkeys/${hex}`, { method: 'DELETE' })
  }

  async getRelayIdentity(): Promise<RelayIdentity> {
    return this.request('/api/admin/relay-identity')
  }

  async updateRelayIdentity(options: {
    relay_name: string
    relay_description: string
    relay_url: string
  }): Promise<RelayIdentity> {
    return this.request('/api/admin/relay-identity', {
      method: 'POST',
      body: JSON.stringify(options),
    })
  }

  async rotateRelayKey(confirm: string): Promise<RotateRelayKeyResult> {
    return this.request('/api/admin/relay-identity/rotate-key', {
      method: 'POST',
      body: JSON.stringify({ confirm }),
    })
  }

  async getConfigBackups(): Promise<BackupEntry[]> {
    return this.request('/api/admin/config/backups')
  }

  async downloadConfigBackup(id: string): Promise<BackupDownload> {
    return this.request(`/api/admin/config/backups/${encodeURIComponent(id)}/download`)
  }

  async restoreConfigBackup(id: string, confirm: string): Promise<RestoreBackupResult> {
    return this.request(`/api/admin/config/backups/${encodeURIComponent(id)}/restore`, {
      method: 'POST',
      body: JSON.stringify({ confirm }),
    })
  }

  async getBlacklist(): Promise<Array<{ hex: string; npub: string }>> {
    return this.request('/api/admin/blacklist')
  }

  async addToBlacklist(pubkey: string): Promise<{ hex: string; npub: string }> {
    return this.request('/api/admin/blacklist', {
      method: 'POST',
      body: JSON.stringify({ pubkey }),
    })
  }

  async removeFromBlacklist(hex: string): Promise<void> {
    return this.request(`/api/admin/blacklist/${hex}`, { method: 'DELETE' })
  }

  async deleteGroup(id: string): Promise<void> {
    return this.request(`/api/admin/groups/${encodeURIComponent(id)}`, { method: 'DELETE' })
  }

  async getGroupEvents(groupId: string, limit?: number, author?: string): Promise<EventInfo[]> {
    const params = new URLSearchParams()
    if (limit) params.set('limit', String(limit))
    if (author) params.set('author', author)
    const q = params.toString() ? `?${params}` : ''
    return this.request(`/api/admin/groups/${encodeURIComponent(groupId)}/events${q}`)
  }

  async getGroupMembers(groupId: string): Promise<MemberInfo[]> {
    return this.request(`/api/admin/groups/${encodeURIComponent(groupId)}/members`)
  }

  async deleteEvent(eventId: string): Promise<void> {
    return this.request(`/api/admin/events/${eventId}`, { method: 'DELETE' })
  }

  async removeGroupMember(groupId: string, pubkey: string): Promise<void> {
    return this.request(
      `/api/admin/groups/${encodeURIComponent(groupId)}/members/${pubkey}`,
      { method: 'DELETE' },
    )
  }

  async deleteUserEvents(pubkey: string): Promise<void> {
    return this.request(`/api/admin/users/${pubkey}/events`, { method: 'DELETE' })
  }
}

export interface EventInfo {
  id: string
  pubkey: string
  kind: number
  content: string
  created_at: number
}

export interface MemberInfo {
  pubkey: string
  roles: string[]
}

export interface SetupStatus {
  needs_setup: boolean
  admin_count: number
  relay_url: string
  whitelisted_count: number
  reference_account_count: number
  setup_owner_pubkey?: string | null
  setup_owner_npub?: string | null
}

export interface SetupResult {
  token: string
  admin_pubkey: string
  admin_npub: string
  whitelisted_owner: boolean
  reference_owner: boolean
}

export interface ConfigResetResult {
  setup_owner_pubkey: string
  setup_owner_npub: string
  needs_setup: boolean
  whitelisted_count: number
  reference_account_count: number
  backup_path: string
  message: string
}

export interface RestartRelayResult {
  message: string
  restart_in_ms: number
}

export interface AdminPubkeyEntry {
  hex: string
  npub: string
  current_session: boolean
}

export interface RelayIdentity {
  relay_name: string
  relay_description: string
  relay_url: string
  relay_pubkey: string
  restart_required: boolean
}

export interface RotateRelayKeyResult {
  relay_pubkey: string
  restart_required: boolean
  message: string
}

export interface BackupEntry {
  id: string
  created_unix: number
  path: string
  file_count: number
  size_bytes: number
  files: string[]
}

export interface BackupDownload {
  id: string
  files: Array<{ name: string; content: string }>
}

export interface RestoreBackupResult {
  id: string
  backup_before_restore_path: string
  admin_count: number
  whitelisted_count: number
  reference_account_count: number
  restart_required: boolean
  message: string
}

export interface AccessSettings {
  access_policy: 'owner_only' | 'open'
  pubkey_rate_limit_per_minute: number
  connection_rate_limit_per_minute: number
  global_rate_limit_per_minute: number
  whitelisted_count: number
  restart_required: boolean
}

export interface StorageSettings {
  db_path: string
  db_size_bytes: number
  db_file_count: number
  pruning_enabled: boolean
  configured_pruning_enabled: boolean
  retention_days: number
  prune_interval_minutes: number
  prune_kinds: number[]
  total_pruned: number
  runs: number
  last_run_unix: number
  restart_required: boolean
}

export interface ObeliskIndexSettings {
  enabled: boolean
  active_enabled: boolean
  recent_per_group: number
  max_bootstrap_groups: number
  max_page_limit: number
  bootstrap_requests_per_minute: number
  message_requests_per_minute: number
  reconcile_interval_minutes: number
  restart_required: boolean
}

export interface ObeliskIndexSettingsRequest {
  enabled: boolean
  recent_per_group: number
  max_bootstrap_groups: number
  max_page_limit: number
  bootstrap_requests_per_minute: number
  message_requests_per_minute: number
  reconcile_interval_minutes: number
}

export interface GroupInfo {
  id: string
  name: string
  about: string | null
  picture: string | null
  banner: string | null
  parent: string | null
  channel_kind: string | null
  member_count: number
  admin_count: number
  private: boolean
  closed: boolean
  broadcast: boolean
  metadata_tags: string[][]
}

export const adminApi = new AdminApiClient()
