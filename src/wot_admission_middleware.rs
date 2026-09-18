//! Resolves a connection's Web-of-Trust standing once NIP-42 auth has happened.
//!
//! The admission check itself lives in [`Whitelist::contains`], which is called
//! from synchronous code (`EventProcessor::verify_filters`, `can_see_event`) and
//! so can only read a cache. This middleware is what fills that cache.
//!
//! Placement matters. `relay_builder` runs the chain as
//! `NostrLogger -> ErrorHandling -> Nip42Middleware -> <ours> -> RelayMiddleware`,
//! so by the time this sees a message the AUTH has already been verified and
//! `authed_pubkey` is set, and `RelayMiddleware` — which invokes the event
//! processor — has not run yet. That gap is the only place an async lookup can
//! happen without blocking the hot path or rejecting a legitimate first REQ.

use crate::whitelist::Whitelist;
use relay_builder::nostr_middleware::{InboundContext, NostrMiddleware};

#[derive(Debug, Clone)]
pub struct WotAdmissionMiddleware {
    whitelist: Whitelist,
}

impl WotAdmissionMiddleware {
    pub fn new(whitelist: Whitelist) -> Self {
        Self { whitelist }
    }
}

impl NostrMiddleware<()> for WotAdmissionMiddleware {
    async fn process_inbound<Next>(
        &self,
        ctx: InboundContext<'_, (), Next>,
    ) -> Result<(), anyhow::Error>
    where
        Next: relay_builder::nostr_middleware::InboundProcessor<()>,
    {
        let authed_pubkey = ctx.state.read().await.authed_pubkey;

        if let Some(pubkey) = authed_pubkey {
            // Almost always false: the tier is off, a cheaper tier already
            // settles this key, or the verdict is cached from an earlier
            // message on this connection.
            if self.whitelist.needs_wot_resolution(&pubkey) {
                self.whitelist.resolve_wot(&pubkey).await;
            }
        }

        ctx.next().await
    }
}
