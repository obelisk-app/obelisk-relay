import { Component } from 'preact'
import type { ComponentChildren } from 'preact'

interface ErrorBoundaryProps {
  children: ComponentChildren
  /** Shown above the retry button, to say which part of the app failed. */
  label?: string
}

interface ErrorBoundaryState {
  error: Error | null
}

/**
 * Catches render-time throws so one bad value cannot blank the whole app.
 *
 * There was no boundary anywhere in this codebase, which meant any exception
 * during render unmounted everything and left a white page with nothing to act
 * on -- no message, no retry, and nothing in the UI saying what happened. The
 * concrete case that prompted this was an unguarded `new URL(mint)` on a mint
 * address taken from another user's kind:10019 event: untrusted input, rendered
 * once per message, that could take the page down for everyone who could see
 * that user.
 *
 * Deliberately shows the error text but not the stack. A stack trace tells a
 * user nothing they can use and leaks internal structure; `console.error` still
 * carries the full object for anyone filing a bug.
 */
export class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  state: ErrorBoundaryState = { error: null }

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { error }
  }

  componentDidCatch(error: Error) {
    console.error('Render error caught by boundary:', error)
  }

  private retry = () => {
    this.setState({ error: null })
  }

  render() {
    const { error } = this.state
    if (!error) return this.props.children

    return (
      <div
        role="alert"
        class="flex flex-col items-center justify-center gap-3 p-6 text-center"
      >
        <div class="text-2xl" aria-hidden="true">⚠️</div>
        <p class="text-sm font-medium text-[var(--color-text-primary)]">
          {this.props.label ?? 'Something went wrong here.'}
        </p>
        <p class="text-xs text-[var(--color-text-tertiary)] max-w-md break-words">
          {error.message}
        </p>
        <button
          onClick={this.retry}
          class="mt-1 px-4 py-2 rounded-lg text-sm font-medium border
                 border-[var(--color-border)] text-[var(--color-text-secondary)]
                 hover:border-[var(--color-border-hover)] hover:text-[var(--color-text-primary)]
                 transition-colors"
        >
          Try again
        </button>
      </div>
    )
  }
}
