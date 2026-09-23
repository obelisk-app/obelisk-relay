// UI and Timing Constants
export const TIMEOUTS = {
  COMPATIBILITY_CHECK_DELAY: 100, // ms delay before checking recipient compatibility
  NUTZAP_SEND_TIMEOUT: 15000, // ms timeout for nutzap sending operations
  RELAY_CONNECTION_TIMEOUT: 3000, // ms timeout for relay connections
  COPY_FEEDBACK_DURATION: 2000, // ms duration for copy success feedback
  BALANCE_THROTTLE: 1000, // ms throttle for balance calculations
} as const;

export const CACHE_DURATIONS = {
  BALANCE_CACHE_TIMEOUT: 5 * 60 * 1000, // 5 minutes
  TRANSACTION_HISTORY_LIMIT: 100, // max transactions to keep
} as const;

export const COLORS = {
  BITCOIN_ORANGE: '#f7931a',
  BITCOIN_ORANGE_HOVER: '#f68e0a',
  BITCOIN_ORANGE_10: '#f7931a1a',
  BITCOIN_ORANGE_20: '#f7931a33',
} as const;

export const MIN_NUTZAP_AMOUNT = 1; // minimum sats for nutzap compatibility check

/**
 * Rendezvous relays for NIP-46 (remote signer / bunker) login.
 *
 * The browser and the signer app must meet on a relay they can *both* reach.
 * Third-party relays are the weak link in that: `relay.nsec.app` resolves but
 * refuses TCP 443 from at least one of our hosts, so a browser running there
 * publishes its pairing request somewhere the signer will never look, and the
 * login hangs with no error. Earlier, `relay.nsec.app` and `relay.damus.io`
 * were both down at once and the QR would not render at all.
 *
 * This relay is the one host guaranteed reachable by both ends: the admin
 * console is being served *from* it, and the signer is being pointed *at* it.
 * It accepts kind 24133 (ephemeral, so nothing is stored), which was verified
 * against production rather than assumed. obelisk-dex reaches the same
 * conclusion independently — see `LoginModal.tsx`, which passes only
 * `wss://public.obelisk.ar`.
 *
 * `window.location.origin` rather than a hardcoded host, so a self-hosted
 * deployment rendezvouses on its own relay instead of ours.
 *
 * Public relays stay behind it as fallbacks, for a signer that refuses to talk
 * to an unfamiliar host.
 */
export const ownRelayUrl = (): string => {
  if (typeof window === 'undefined') return 'wss://public.obelisk.ar';
  const { protocol, host } = window.location;
  return `${protocol === 'https:' ? 'wss:' : 'ws:'}//${host}`;
};

export const NIP46_RELAYS: readonly string[] = [
  ownRelayUrl(),
  'wss://relay.damus.io',
  'wss://nos.lol',
  'wss://relay.primal.net',
];