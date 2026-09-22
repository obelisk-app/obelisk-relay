// Dev-only. These were unconditional imports, so every production bundle shipped
// preact/debug -- ~33KB, but more importantly it wraps vnode creation with
// per-render validation, which is a cost paid on every render by every user.
// A static `import` cannot be conditional, hence the dynamic one.
if (import.meta.env.DEV) {
  await import("preact/debug")
  await import("preact/devtools")
}
import "@nostr-wot/ui/styles.css"
import { ErrorBoundary } from "./components/ErrorBoundary"
import { render } from "preact"
import { useEffect } from "preact/hooks"
import Router from "preact-router"
import { NostrSessionProvider } from "@nostr-wot/ui"
import { LandingPage } from "./components/LandingPage.tsx"
import { DocsPage } from "./components/DocsPage.tsx"
import { AdminPanel } from "./components/admin/AdminPanel.tsx"
import "./style.css"

/**
 * `/app` is not a page this relay serves.
 *
 * It used to be a second, complete Nostr client bundled into the relay: its own
 * login, its own session handling, its own group UI and wallet. That is the
 * canonical client's job, and running a fork of it here meant every relay
 * deployment shipped and had to maintain a client that lagged the real one --
 * while the admin console, three lines away, already linked users to
 * obelisk.ar with this relay as a parameter.
 *
 * So the route stays and the client goes: an existing bookmark still works, it
 * just lands on the maintained client pointed at whichever relay served the
 * link. `window.location.host` rather than a fixed hostname, so public,
 * lacrypta and any other deployment each send people to themselves.
 */
const clientUrl = () =>
  `https://obelisk.ar/app?relay=${encodeURIComponent(window.location.host)}`

const ClientRedirect = (_props: { path?: string }) => {
  useEffect(() => {
    // `replace`, not `assign`: landing here and pressing Back should return to
    // wherever you came from, not bounce through the redirect again.
    window.location.replace(clientUrl())
  }, [])

  return (
    <div class="lc-page-center">
      <p>
        Opening the Obelisk client for this relay…{" "}
        <a href={clientUrl()}>Continue</a> if nothing happens.
      </p>
    </div>
  )
}

const Root = () => {
  return (
    // Outermost boundary: whatever throws, the user gets a message and a retry
    // instead of a blank page.
    <ErrorBoundary label="The app hit an unexpected error.">
      <NostrSessionProvider theme="la-crypta" autoRestore>
        <Router>
          <LandingPage path="/" />
          <DocsPage path="/docs" />
          <ClientRedirect path="/app" />
          <AdminPanel path="/admin" />
        </Router>
      </NostrSessionProvider>
    </ErrorBoundary>
  )
}

render(<Root />, document.getElementById("app")!)
