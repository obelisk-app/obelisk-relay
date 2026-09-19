import { Component } from 'preact'

export interface FlashMessageProps {
  message: string | null
  type?: 'success' | 'error' | 'info'
  onDismiss: () => void
}

/**
 * The app's only global feedback channel.
 *
 * The dismiss timer used to be armed solely in `componentDidUpdate`, gated on a
 * `null -> non-null` transition. But the parent renders this conditionally
 * (`{flashMessage && <FlashMessage …/>}`), so the component *mounts* with
 * `message` already set and that transition never happens inside it. The timer
 * never fired: every toast the app has ever shown stayed on screen until the
 * user clicked the ×, and a second message silently replaced the first.
 *
 * So arm on mount as well, and re-arm whenever the message text changes rather
 * than only when it appears from nothing -- back-to-back messages each deserve
 * their own full dwell, and the old timer must not cut the new one short.
 */
const DISMISS_AFTER_MS = 5000

export class FlashMessage extends Component<FlashMessageProps> {
  private dismissTimer: ReturnType<typeof setTimeout> | null = null

  componentDidMount() {
    this.armDismiss()
  }

  componentDidUpdate(prevProps: FlashMessageProps) {
    if (this.props.message !== prevProps.message) {
      this.armDismiss()
    }
  }

  componentWillUnmount() {
    this.clearDismiss()
  }

  private armDismiss() {
    this.clearDismiss()
    if (!this.props.message) return
    this.dismissTimer = setTimeout(() => {
      this.dismissTimer = null
      this.props.onDismiss()
    }, DISMISS_AFTER_MS)
  }

  private clearDismiss() {
    if (this.dismissTimer !== null) {
      clearTimeout(this.dismissTimer)
      this.dismissTimer = null
    }
  }

  render() {
    const { message, type = 'info' } = this.props
    if (!message) return null

    const styles = {
      success: 'bg-green-500/20 text-green-600 border-green-500/30',
      error: 'bg-red-500/20 text-red-600 border-red-500/30',
      info: 'bg-accent/20 text-accent border-accent/30'
    }[type]

    return (
      <div class="fixed top-4 left-1/2 -translate-x-1/2 z-50 w-full max-w-xl mx-auto px-4">
        {/* Errors interrupt; success and info wait for a pause. Without this the
            only feedback the app gives is invisible to a screen reader. */}
        <div
          role={type === 'error' ? 'alert' : 'status'}
          aria-live={type === 'error' ? 'assertive' : 'polite'}
          class={`${styles} px-4 py-3 rounded-lg shadow-xl border backdrop-blur-sm flex items-center justify-between`}
        >
          <span class="text-sm font-medium">{message}</span>
          <button
            onClick={this.props.onDismiss}
            class="ml-3 text-current opacity-70 hover:opacity-100 transition-opacity"
            aria-label="Dismiss message"
          >
            ×
          </button>
        </div>
      </div>
    )
  }
}
