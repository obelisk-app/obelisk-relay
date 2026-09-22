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

  /**
   * Counts per access tier. The whitelist list itself only contains the two
   * enumerable tiers, so without this a relay admitting thousands through the
   * Web of Trust reports only its handful of listed entries.
   */
  async getAccessSources(): Promise<AccessSources> {
    return this.request('/api/admin/whitelist/sources')
  }

  /** Would this pubkey be admitted, and by which tier. */
  async checkAccess(pubkey: string): Promise<AccessCheck> {
    return this.request(`/api/admin/whitelist/check?pubkey=${encodeURIComponent(pubkey)}`)
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
    prune_interval_minutes: number
    /** Retention in days, per kind. */
    retention_days_by_kind: Record<number, number>
  }): Promise<StorageSettings> {
    return this.request('/api/admin/storage-settings', {
      method: 'POST',
      body: JSON.stringify(settings),
    })
  }

  /**
   * Exact count for one kind (~12s on a multi-GB database). One kind at a time
   * by design — a full sweep exceeded ten minutes when measured.
   */
  async getExactKindCount(kind: number, olderThanDays?: number): Promise<ExactCountEnvelope> {
    const q = olderThanDays ? `&older_than_days=${olderThanDays}` : ''
    return this.request(`/api/admin/storage/exact-count?kind=${kind}${q}`)
  }

  /** Disk usage over time. Bounded server-side; the last point is live. */
  async getStorageHistory(): Promise<{ samples: StorageSample[] }> {
    return this.request('/api/admin/storage/history')
  }

  async getStorageStats(refresh = false): Promise<StorageStatsEnvelope> {
    return this.request(`/api/admin/storage/stats${refresh ? '?refresh=true' : ''}`)
  }

  /**
   * Which pubkeys account for one kind, from the sample already taken.
   * Never triggers a scan, so a drilldown is instant and agrees with the
   * kinds table it was opened from.
   */
  async getStorageKindAuthors(kind: number): Promise<StorageKindAuthors> {
    return this.request(`/api/admin/storage/kinds/${kind}/authors`)
  }

  /**
   * What a compaction would reclaim, and what earlier ones did.
   *
   * Measuring walks the whole free list in a child process, so the server
   * caches it for a minute; pass refresh after a prune to see the new slack.
   */
  async getCompactionStatus(refresh = false): Promise<CompactionStatus> {
    return this.request(`/api/admin/storage/compaction${refresh ? '?refresh=true' : ''}`)
  }

  /**
   * Stage a compaction and restart into it.
   *
   * The relay cannot compact the database it is serving from, so this returns
   * as the process is exiting: expect the next few requests to fail while
   * Docker brings it back.
   */
  async compactDatabase(confirm: string): Promise<CompactResult> {
    return this.request('/api/admin/storage/compact', {
      method: 'POST',
      body: JSON.stringify({ confirm }),
    })
  }

  /** WoT admission: whether it is on, whether it works, and who it let in. */
  async getWotStatus(): Promise<WotStatus> {
    return this.request('/api/admin/wot')
  }

  /**
   * Persist WoT settings. Takes effect on the next relay restart — the tier is
   * constructed at startup, so the response always reports restart_required.
   */
  async updateWotSettings(settings: WotSettingsRequest): Promise<{ saved: boolean; restart_required: boolean }> {
    return this.request('/api/admin/wot', {
      method: 'POST',
      body: JSON.stringify(settings),
    })
  }

  /**
   * Delete one pubkey's events, narrowed by kind and an optional date window.
   *
   * Call with dry_run first: the count it returns is what the destructive call
   * removes, because both run off the same filter server-side.
   */
  async pruneEvents(req: PruneRequest): Promise<PruneResult> {
    return this.request('/api/admin/storage/prune', {
      method: 'POST',
      body: JSON.stringify(req),
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

  async getConnectionSettings(): Promise<ConnectionSettings> {
    return this.request('/api/admin/connection-settings')
  }

  async updateConnectionSettings(settings: ConnectionSettingsRequest): Promise<ConnectionSettings> {
    return this.request('/api/admin/connection-settings', {
      method: 'POST',
      body: JSON.stringify(settings),
    })
  }

  async getReports(status: 'open' | 'resolved' | 'all' = 'open'): Promise<ReportsResponse> {
    return this.request(`/api/admin/reports?status=${status}`)
  }

  async resolveReport(req: ResolveReportRequest): Promise<{ resolved: boolean }> {
    return this.request('/api/admin/reports/resolve', {
      method: 'POST',
      body: JSON.stringify(req),
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

  /** What this build is: crate version, commit, build time, image tag. */
  async getVersion(): Promise<VersionInfo> {
    return this.request('/api/admin/version')
  }

  /**
   * Running version, what is published, and whether the host agent that would
   * carry out an update is alive.
   */
  async getUpdateStatus(refresh = false): Promise<UpdateStatus> {
    return this.request(`/api/admin/update${refresh ? '?refresh=true' : ''}`)
  }

  /**
   * Queue an update to a published tag.
   *
   * Returns as soon as the request is written. The relay does not do the work —
   * a host-side agent pulls the image and recreates the container, so expect the
   * relay to go away shortly after this resolves, and poll getVersion() to see
   * what came back.
   */
  async updateRelay(tag: string, confirm: string): Promise<UpdateRelayResult> {
    return this.request('/api/admin/update', {
      method: 'POST',
      body: JSON.stringify({ tag, confirm }),
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

  /**
   * Delete many events in one request. Returns per-id outcomes so partial
   * failures can be reported honestly rather than as a single pass/fail.
   */
  async bulkDeleteEvents(eventIds: string[]): Promise<BulkDeleteResponse> {
    return this.request('/api/admin/events/delete', {
      method: 'POST',
      body: JSON.stringify({ event_ids: eventIds }),
    })
  }

  /**
   * Delete events authored by each pubkey. Protected group-management kinds are
   * never removed, whatever is requested.
   */
  async bulkDeleteUserEvents(pubkeys: string[], kinds?: number[]): Promise<BulkUserResponse> {
    return this.request('/api/admin/users/delete-events', {
      method: 'POST',
      body: JSON.stringify({ pubkeys, kinds }),
    })
  }

  /**
   * Delete events addressed to each pubkey via their `p` tag. The only way to
   * act on gift wraps per user — their authors are one-time keys.
   */
  async bulkDeleteByRecipient(pubkeys: string[], kinds?: number[]): Promise<BulkUserResponse> {
    return this.request('/api/admin/events/delete-by-recipient', {
      method: 'POST',
      body: JSON.stringify({ pubkeys, kinds }),
    })
  }

  /** Public relay info — no auth required; used for branding the console. */
  async getRelayInfo(): Promise<PublicRelayInfo> {
    return this.request('/api/relay-info')
  }

  async getRelayIdentity(): Promise<RelayIdentity> {
    return this.request('/api/admin/relay-identity')
  }

  async updateRelayIdentity(options: {
    relay_name: string
    relay_description: string
    relay_url: string
    relay_icon: string
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

/** One past compaction, as recorded at the startup that ran it. */
export interface CompactionEntry {
  at: number
  /** `ok`, `refused` (nothing was touched) or `failed` (the original was restored). */
  status: string
  before_bytes: number
  after_bytes: number
  duration_ms: number
  requested_by: string | null
  detail: string | null
}

export interface CompactionStatus {
  db_file_bytes: number
  /** Bytes in pages actually in use. Null if the database could not be measured. */
  live_bytes: number | null
  /** Free-list slack: what a compaction hands back to the filesystem. */
  reclaimable_bytes: number | null
  free_disk_bytes: number | null
  required_free_bytes: number
  can_compact: boolean
  /** Why not, when can_compact is false. Show this rather than inventing one. */
  blocked_reason: string | null
  /** A compaction is already staged for the next start. */
  pending: boolean
  measured_at: number
  measure_error: string | null
  /** Newest last. */
  history: CompactionEntry[]
}

export interface VersionInfo {
  package_version: string
  /** Short commit, `-dirty` if built from an uncommitted tree, or `unknown`. */
  git_sha: string
  build_time: string
  /** Null when compose did not pass RELAY_IMAGE_TAG — unknown, not "latest". */
  image_tag: string | null
}

export interface UpdateRequestInfo {
  requested_tag: string
  requested_by: string
  requested_at: number
  nonce: string
}

export interface UpdateResultInfo {
  /** `ok`, `rolled-back`, `rejected` or `failed`. */
  status: string
  requested_tag: string | null
  previous_tag: string | null
  finished_at: number
  nonce: string | null
  detail: string | null
  /** Tail of the agent's log, so a failure is readable without SSH. */
  log: string | null
}

export interface UpdateStatus {
  running: VersionInfo
  image_repository: string
  /** Published tags, newest first. */
  available_tags: string[]
  /** Set when the registry could not be reached — distinct from "none published". */
  tags_error: string | null
  tags_fetched_at: number
  /** Whether the running tag is one the registry publishes. False = local build. */
  running_is_published: boolean
  /** Whether the host agent has checked in recently enough to act. */
  agent_live: boolean
  agent_last_seen: number | null
  pending: UpdateRequestInfo | null
  last_result: UpdateResultInfo | null
  can_update: boolean
  blocked_reason: string | null
}

export interface UpdateRelayResult {
  message: string
  requested_tag: string
  nonce: string
}

export interface CompactResult {
  message: string
  restart_in_ms: number
  expected_reclaim_bytes: number | null
}

export interface AdminPubkeyEntry {
  hex: string
  npub: string
  current_session: boolean
  /** The identity that ran setup — who this relay belongs to. Sorted first. */
  owner: boolean
}

export interface BulkDeleteResponse {
  deleted: number
  failed: number
  results: Array<{ id: string; deleted: boolean; error?: string }>
}

export interface BulkUserResponse {
  /** Total events removed across every listed pubkey. */
  deleted: number
  failed: number
  results: Array<{ pubkey: string; deleted: number; error?: string }>
}

export interface PublicRelayInfo {
  name: string
  description: string
  /** Relay icon (URL or data URI); empty when unset. */
  icon: string
  group_count: number
  supported_nips: number[]
}

export interface RelayIdentity {
  relay_name: string
  /** NIP-11 icon: https:// URL or data: URI. Empty when unset. */
  relay_icon: string
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
  /** Events an operator deleted by hand since process start. */
  admin_deleted_total: number
  runs: number
  last_run_unix: number
  /** Retention seconds per kind currently in force. */
  policies_secs: Record<number, number> | null
  /** Events deleted per kind since process start. */
  deleted_by_kind: Record<number, number> | null
  restart_required: boolean
}

export interface StorageKindStat {
  kind: number
  count: number
  /** Bytes the sampled events of this kind occupy. */
  sampled_bytes: number
  /** Mean bytes per event, divided server-side so this is never NaN. */
  avg_bytes: number
}

export interface StorageSample {
  /** Unix seconds. */
  at: number
  /** LMDB files on disk, including free-list slack. */
  db_bytes: number
  /**
   * Live connections when the sample was taken. Absent on samples written
   * before the relay recorded it, so treat null as "not measured" rather than
   * as zero — charting it as zero would invent a dip that never happened.
   */
  connections?: number | null
}

export interface StorageStats {
  /**
   * Events examined for the kind breakdown — a newest-first SAMPLE, not a
   * total. Exact per-kind counts cost >12s each on a multi-GB database.
   */
  sampled_events: number
  sample_size: number
  /** True when the sample covered every stored event. */
  sample_is_complete: boolean
  kinds: StorageKindStat[]
  /** Busiest gift-wrap recipients in the sample. */
  top_recipients: RecipientStat[]
  /**
   * Heaviest pubkeys across every kind. Mixed attribution — read
   * `attributed_by` per row rather than assuming these are all senders.
   */
  top_authors: StorageAuthorStat[]
  newest_event_unix: number
  oldest_sampled_unix: number
  scope_count: number
  db_size_bytes: number
  db_file_count: number
  /** Events a prune run would delete right now under the configured window. */
  prune_preview: number
  prune_preview_retention_days: number
  prune_preview_kinds: number[]
  computed_at: number
  cached: boolean
}

export interface ExactCountResponse {
  kind: number
  total: number
  older_than?: number
  older_than_days: number | null
  computed_at: number
}

/** Counting a kind outlives an HTTP request, so the client polls. */
export interface ExactCountEnvelope {
  computing: boolean
  result: ExactCountResponse | null
}

export interface RecipientStat {
  pubkey: string
  count: number
}

/**
 * Whether a row's bytes were charged to the key that signed the events, or to
 * the key they were addressed to.
 *
 * Gift wraps (1059) are signed by a throwaway key per wrap, so only the `p` tag
 * identifies anyone. Never render these two the same way: a recipient row shows
 * who is being sent data, not who is sending it.
 */
export type Attribution = 'author' | 'recipient'

/** One pubkey's share of the storage sample. */
export interface StorageAuthorStat {
  pubkey: string
  /** Empty when the pubkey is unparseable — p tags are attacker-controlled. */
  npub: string
  count: number
  sampled_bytes: number
  attributed_by: Attribution
  /** Live at request time, not as of the sample. */
  blacklisted: boolean
}

/** The pubkeys behind a single kind. */
export interface StorageKindAuthors {
  kind: number
  attributed_by: Attribution
  /** Events of this kind in the sample, for computing each row's share. */
  kind_count: number
  kind_sampled_bytes: number
  authors: StorageAuthorStat[]
  computed_at: number
}

/** One pubkey admitted through the Web-of-Trust tier. */
export interface WotAdmittedEntry {
  hex: string
  npub: string
  hops: number
}

export interface AccessSources {
  manual: number
  follow_derived: number
  blacklisted: number
  wot_enabled: boolean
  /** Accounts the follow graph admits; 0 until it has been built. */
  wot_admitted: number
  wot_max_hops: number
  /** No tier restricts anything — an open relay. */
  open_relay: boolean
  /** What each tier may publish per minute, derived from the base budget. */
  budget_ladder: BudgetRung[]
}

export interface BudgetRung {
  tier: string
  label: string
  /** Share of the base per-pubkey budget. */
  percent: number
  events_per_minute: number
}

export interface AccessCheck {
  hex: string
  npub: string
  admitted: boolean
  tier: 'blacklist' | 'manual' | 'follow_sync' | 'web_of_trust' | 'open_relay' | 'none'
  /** Hops from the nearest root, when the graph could place them. */
  hops: number | null
  explanation: string
}

export interface PruneTarget {
  pubkey: string
  /** Must match how the row was attributed — gift wraps only match by p tag. */
  attributed_by: Attribution
}

export interface PruneRequest {
  /** One or more pubkeys cleared in a single action. */
  targets: PruneTarget[]
  kinds: number[]
  /** Unix seconds. Omit for open-ended. */
  since?: number
  until?: number
  /** Count only. Always preview before deleting. */
  dry_run?: boolean
  /** Required for the destructive call. */
  confirm?: string
}

export interface PruneResult {
  matched: number
  deleted: number
  dry_run: boolean
  /** Per-pubkey outcome, so a partial failure is visible. */
  per_target: { pubkey: string; matched: number; error?: string }[]
}

/** The `relay.wot` block as configured on disk. */
export interface WotConfigured {
  enabled: boolean
  /** Compute from this relay's own follow graph rather than an oracle. */
  local: boolean
  oracle_url: string
  fallback_oracle_url: string
  max_hops: number
  /** Hex pubkeys. Empty means "track the reference accounts". */
  roots: string[]
}

export type WotSettingsRequest = WotConfigured

export interface WotStatus {
  enabled: boolean
  /** Plain-language verdict, e.g. "admitting" or "oracle unreachable". */
  status: string
  oracle_url: string
  /** Result of a live probe; null when the tier is off. */
  oracle_reachable: boolean | null
  oracle_error: string | null
  degraded: boolean
  /** Running on the locally built follow graph. */
  local: boolean
  /** Accounts whose follow list the local graph knows. */
  graph_accounts: number
  graph_edges: number
  /** Deepest hop answerable with confidence; below max_hops when truncated. */
  graph_complete_to_hop: number
  /** The contact-list budget ran out, so the outermost hop is a sample. */
  graph_truncated: boolean
  /** How many accounts the graph would admit — not just those seen so far. */
  would_admit_total: number
  max_hops: number
  root_count: number
  /**
   * Keys resolved and admitted so far — not everyone the graph would admit,
   * only those who have actually connected.
   */
  admitted: WotAdmittedEntry[]
  /** What is saved on disk, which after an edit is ahead of what is running. */
  configured: WotConfigured
  /** Saved settings differ from the running tier; a restart would change it. */
  restart_required: boolean
}

export interface StorageStatsEnvelope {
  /** A scan is running; poll again shortly. */
  computing: boolean
  /** Last completed snapshot, or null if none has been computed yet. */
  stats: StorageStats | null
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

/**
 * Capacity and abuse limits. Every field needs a relay restart to take effect,
 * so the response also reports what the running process is enforcing -- that is
 * the only way the screen can tell "saved" apart from "in force".
 */
export interface ConnectionSettings {
  max_connections: number
  max_connections_per_ip: number
  max_connection_duration_minutes: number
  idle_timeout_minutes: number
  max_subscriptions: number
  max_limit: number
  force_public_groups: boolean
  running_force_public_groups: boolean
  running_max_connections: number
  running_max_connections_per_ip: number
  active_connections: number
  restart_required: boolean
}

export interface ConnectionSettingsRequest {
  max_connections: number
  max_connections_per_ip: number
  max_connection_duration_minutes: number
  idle_timeout_minutes: number
  max_subscriptions: number
  max_limit: number
  force_public_groups: boolean
  /** Literal "FORCE PUBLIC"; required only when switching the flag on. */
  force_public_confirm?: string
}

/** One NIP-56 report, as filed. */
export interface Report {
  report_id: string
  reporter: string
  report_type: string
  /** The reporter's own words. Untrusted -- render as text, never as markup. */
  content: string
  created_at: number
}

/** Everything reported about a single target: one row of the queue. */
export interface ReportCase {
  /** Round-trip key, "e:<id>" or "p:<hex>". */
  key: string
  target: { kind: 'event'; id: string } | { kind: 'pubkey'; hex: string }
  reports: Report[]
  /** Distinct reporters, not report count. */
  reporter_count: number
  types: string[]
  last_reported_at: number
  resolution: {
    action: string
    resolved_by: string
    resolved_at: number
    note: string
  } | null
  /** What was actually reported, so it can be judged without a second lookup. */
  reported_content: string | null
  /**
   * The account actions apply to, and only ever a verified one: the author of
   * the stored event, or the pubkey when the report names a person. Null when
   * the reported message is gone -- the relay then does not know whose it was.
   */
  reported_pubkey: string | null
  /** Who the reporter *said* was responsible. Display only, never actionable. */
  claimed_pubkey: string | null
  /**
   * True when the content shown is a snapshot taken at report time because the
   * original has since been deleted or pruned. A moderator should know whether
   * they are reading the message as it stands or as it was when objected to.
   */
  content_from_snapshot: boolean
  /** Present only when the reported event belongs to a group. */
  group_id: string | null
}

export interface ReportsResponse {
  cases: ReportCase[]
  resolved_total: number
}

export interface ResolveReportRequest {
  key: string
  action: 'dismiss' | 'delete_event' | 'remove_from_group' | 'blacklist'
  note?: string
  group_id?: string
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
