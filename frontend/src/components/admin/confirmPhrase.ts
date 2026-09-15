/**
 * Matching for typed confirmations ("type DELETE to confirm").
 *
 * Case-insensitive on purpose. The friction that makes this control worth
 * having is that the operator must type a specific word rather than click
 * once — requiring a particular *case* adds no safety, and silently rejecting
 * `delete` reads as the button being broken. That is worse than useless here:
 * an operator who believes the UI is flaky retries, and retrying destructive
 * actions is exactly the habit this gate exists to prevent.
 *
 * Surrounding whitespace is ignored too — it survives a copy/paste and carries
 * no intent.
 */
export const confirmMatches = (typed: string, phrase: string): boolean =>
  typed.trim().toLowerCase() === phrase.toLowerCase()
