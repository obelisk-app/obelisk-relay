import { FunctionComponent } from 'preact'

interface ErrorStateProps {
  error: Error | string
  onRetry?: () => void
}

/**
 * The connection-failure screen.
 *
 * It used to render `error.stack` in a `<pre>`. A stack trace is not something a
 * user can act on, it pushes the actual message and the retry button off the
 * fold, and it exposes internal structure to anyone who hits a transient
 * network error. The message carries the useful part -- `toConnectionMessage`
 * in main.tsx is careful to preserve the relay's own words -- and the stack
 * still reaches `console.error` for a bug report.
 *
 * The old styling was light-theme (`text-red-500` on `bg-red-50`) inside a dark
 * app, so the panel rendered as a bright rectangle with barely-legible text.
 */
export const ErrorState: FunctionComponent<ErrorStateProps> = ({ error, onRetry }) => {
  const errorMessage = error instanceof Error ? error.message : error

  return (
    <div role="alert" class="p-5 max-w-lg mx-auto text-center">
      <div class="text-2xl mb-3" aria-hidden="true">⚠️</div>
      <h2 class="text-xl font-bold mb-2 text-[var(--color-text-primary)]">
        Connection Error
      </h2>
      <p class="mb-5 text-sm text-[var(--color-text-secondary)] break-words">
        {errorMessage}
      </p>
      {onRetry && (
        <button
          onClick={onRetry}
          class="px-4 py-2 bg-accent text-white rounded-lg font-medium hover:bg-accent/90 transition-colors"
        >
          Try Again
        </button>
      )}
    </div>
  )
}
