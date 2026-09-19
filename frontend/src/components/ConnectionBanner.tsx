import { Component } from 'preact'
import { NostrClient } from '../api/nostr_client'

interface ConnectionBannerProps {
  client: NostrClient
}

interface ConnectionBannerState {
  relayOnline: boolean
  browserOnline: boolean
}

/**
 * Tells the user when the app has stopped talking to the relay.
 *
 * Nothing did this before: a dropped relay connection produced a console line
 * and silence in the UI. The app looked the same as a quiet room, so people went
 * on reading a stale view and publishing into a socket that was gone.
 *
 * Two independent signals, because they call for different wording. The browser
 * being offline is something the user can act on; the relay being unreachable
 * while the network is fine is not, and saying "check your connection" in that
 * case would be actively misleading.
 */
export class ConnectionBanner extends Component<ConnectionBannerProps, ConnectionBannerState> {
  state: ConnectionBannerState = {
    relayOnline: true,
    browserOnline: typeof navigator === 'undefined' ? true : navigator.onLine,
  }

  private unsubscribeRelay: (() => void) | null = null

  componentDidMount() {
    this.unsubscribeRelay = this.props.client.onConnectionChange(relayOnline => {
      this.setState({ relayOnline })
    })
    window.addEventListener('online', this.handleBrowserOnline)
    window.addEventListener('offline', this.handleBrowserOffline)
  }

  componentWillUnmount() {
    this.unsubscribeRelay?.()
    this.unsubscribeRelay = null
    window.removeEventListener('online', this.handleBrowserOnline)
    window.removeEventListener('offline', this.handleBrowserOffline)
  }

  private handleBrowserOnline = () => this.setState({ browserOnline: true })
  private handleBrowserOffline = () => this.setState({ browserOnline: false })

  render() {
    const { relayOnline, browserOnline } = this.state
    if (relayOnline && browserOnline) return null

    const message = !browserOnline
      ? "You're offline. Messages will not send or arrive until your connection returns."
      : 'Lost the connection to the relay. Reconnecting…'

    return (
      <div
        role="status"
        aria-live="polite"
        class="w-full px-4 py-2 text-center text-sm font-medium
               bg-yellow-500/15 text-yellow-300 border-b border-yellow-500/30"
      >
        {message}
      </div>
    )
  }
}
