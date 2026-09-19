import { Component } from 'preact'
import { NostrClient, NostrGroupError } from '../api/nostr_client'
import type { Group } from '../types'
import { UserDisplay } from './UserDisplay'
import type { Proof } from '@cashu/cashu-ts'

interface JoinRequestSectionProps {
  group: Group
  client: NostrClient
  showMessage: (message: string, type: 'success' | 'error' | 'info') => void
  cashuProofs?: Proof[]
  mints?: string[]
  onNutzapSent?: () => void
}

interface JoinRequestSectionState {
  showJoinForm: boolean
  inviteCode: string
  isSubmitting: boolean
  /** Pubkey currently being accepted, so its row can't be double-submitted. */
  accepting: string | null
  /** Pubkey currently being rejected. */
  rejecting: string | null
}

export class JoinRequestSection extends Component<JoinRequestSectionProps, JoinRequestSectionState> {
  state: JoinRequestSectionState = {
    showJoinForm: false,
    inviteCode: '',
    isSubmitting: false,
    accepting: null,
    rejecting: null
  }

  getCurrentUserPubkey = (): string | null => {
    try {
      const signer = this.props.client.ndkInstance?.signer;
      if (!signer) return null;
      // Get the user synchronously if possible
      const user = (signer as any)._user;
      return user?.pubkey || null;
    } catch {
      return null;
    }
  }

  private showError = (prefix: string, error: unknown) => {
    console.error(prefix, error)
    const message = error instanceof NostrGroupError ? error.displayMessage : String(error)
    this.props.showMessage(`${prefix}: ${message}`, 'error')
  }

  handleAcceptRequest = async (pubkey: string) => {
    if (this.state.accepting) return
    this.setState({ accepting: pubkey })
    try {
      await this.props.client.acceptJoinRequest(this.props.group.id, pubkey)
      this.props.showMessage('Join request accepted successfully', 'success')
    } catch (error) {
      this.showError('Failed to accept join request', error)
    } finally {
      this.setState({ accepting: null })
    }
  }

  /**
   * Reject by sending kind 9001 (remove user), which is what actually clears the
   * pending request: the relay drops the pubkey from `join_requests` whenever it
   * removes them from the group.
   *
   * This previously called `deleteEvent(groupId, pubkey)`, which publishes a
   * 9005 with the *pubkey* sitting in the `e` (event id) tag. The relay found no
   * event with that id, deleted nothing, and the UI reported success anyway. It
   * was also never wired to a button, so admins had no reject path at all.
   */
  handleRejectRequest = async (pubkey: string) => {
    if (this.state.rejecting) return
    this.setState({ rejecting: pubkey })
    try {
      await this.props.client.removeMember(this.props.group.id, pubkey)
      this.props.showMessage('Join request rejected', 'success')
    } catch (error) {
      this.showError('Failed to reject join request', error)
    } finally {
      this.setState({ rejecting: null })
    }
  }

  handleSubmitJoinRequest = async (e: Event) => {
    e.preventDefault()
    if (!this.state.inviteCode.trim()) return

    this.setState({ isSubmitting: true })
    try {
      await this.props.client.sendJoinRequest(this.props.group.id, this.state.inviteCode)
      this.setState({ inviteCode: '', showJoinForm: false })
      this.props.showMessage('Join request submitted successfully', 'success')
    } catch (error) {
      this.showError('Failed to submit join request', error)
    } finally {
      this.setState({ isSubmitting: false })
    }
  }

  render() {
    const { group, client } = this.props

    // Get wallet state from client
    const cashuProofs = client.getCashuProofs()
    const mints = client.getWalletMints()

    return (
      <div class="space-y-4">
        {group.joinRequests.length > 0 ? (
          <div class="space-y-2">
            {group.joinRequests.map(pubkey => (
              <div
                key={pubkey}
                class="flex items-center justify-between gap-2 p-4 bg-[var(--color-bg-primary)]
                       rounded-lg border border-[var(--color-border)] hover:border-[var(--color-border-hover)]
                       transition-colors"
              >
                <div class="flex items-center gap-2">
                  <UserDisplay
                    pubkey={this.props.client.pubkeyToNpub(pubkey)}
                    client={client}
                    showCopy={false}
                    cashuProofs={cashuProofs}
                    mints={mints}
                    onSendNutzap={() => {
                      this.props.showMessage('Nutzap sent successfully!', 'success');
                      if (this.props.onNutzapSent) this.props.onNutzapSent();
                    }}
                    hideNutzap={pubkey === this.getCurrentUserPubkey() && !window.location.search.includes('selfnutzap')}
                  />
                  <button
                    onClick={() => this.handleAcceptRequest(pubkey)}
                    disabled={this.state.accepting === pubkey || this.state.rejecting === pubkey}
                    class="shrink-0 px-4 py-2 bg-accent text-white rounded-lg text-sm font-medium
                           hover:bg-accent/90 transition-colors flex items-center gap-2
                           disabled:opacity-50 disabled:cursor-not-allowed"
                  >
                    {this.state.accepting === pubkey ? 'Accepting…' : 'Accept'}
                  </button>
                  <button
                    onClick={() => this.handleRejectRequest(pubkey)}
                    disabled={this.state.accepting === pubkey || this.state.rejecting === pubkey}
                    class="shrink-0 px-4 py-2 rounded-lg text-sm font-medium border
                           border-[var(--color-border)] text-[var(--color-text-secondary)]
                           hover:border-[var(--color-border-hover)] hover:text-[var(--color-text-primary)]
                           transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                  >
                    {this.state.rejecting === pubkey ? 'Rejecting…' : 'Reject'}
                  </button>
                </div>
              </div>
            ))}
          </div>
        ) : (
          <div class="text-center py-12">
            <div class="mb-3 text-2xl">🤝</div>
            <p class="text-sm text-[var(--color-text-tertiary)]">No pending join requests</p>
            <p class="text-xs text-[var(--color-text-tertiary)] mt-1">
              Share the group invite code to let others join
            </p>
          </div>
        )}
      </div>
    )
  }
}