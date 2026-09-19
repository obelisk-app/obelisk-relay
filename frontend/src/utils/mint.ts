/**
 * Mint URLs arrive from other people's kind:10019 events, so they are untrusted
 * input and are not always parseable. `new URL(mint)` on a malformed one throws,
 * and a throw inside render unmounts the whole tree -- UserDisplay renders once
 * per message, per member and per join request, so a single peer with a broken
 * mint URL could blank the app for everyone who could see them.
 *
 * This was already implemented correctly inside WalletDisplay; it lives here so
 * there is one copy and every caller gets the guarded version.
 */
export const getMintHostname = (mint: string): string => {
  try {
    // A bare host like "mint.example.com" is not a valid URL on its own.
    const urlString = mint.includes('://') ? mint : `https://${mint}`
    return new URL(urlString).hostname
  } catch {
    // Unparseable: show the operator what they actually configured rather than
    // hiding it behind a placeholder.
    return mint
  }
}
