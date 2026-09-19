use crate::groups::NON_GROUP_ALLOWED_KINDS;
use nostr_sdk::prelude::*;
use relay_builder::nostr_middleware::{InboundContext, NostrMiddleware};
use tracing::{debug, warn};

use crate::groups::{
    ADDRESSABLE_EVENT_KINDS, KIND_GROUP_ADD_USER_9000, KIND_GROUP_CREATE_9007,
    KIND_GROUP_CREATE_INVITE_9009, KIND_GROUP_DELETE_9008, KIND_GROUP_DELETE_EVENT_9005,
    KIND_GROUP_EDIT_METADATA_9002, KIND_GROUP_REMOVE_USER_9001, KIND_GROUP_SET_ROLES_9006,
    KIND_GROUP_USER_JOIN_REQUEST_9021, KIND_GROUP_USER_LEAVE_REQUEST_9022,
};

/// Largest `content` this relay will store, in bytes.
///
/// Generous for the traffic Obelisk actually carries -- chat messages, group
/// metadata, NIP-60 wallet state and gift wraps all sit far below it -- while
/// still ruling out the megabyte-scale payloads that turn a write into a
/// permanent storage commitment.
const MAX_CONTENT_BYTES: usize = 256 * 1024;

/// Ceiling on tag count. Tags are indexed, so a single event with tens of
/// thousands of them costs far more than its byte size suggests.
const MAX_TAGS: usize = 2_000;

/// How far ahead of the relay's clock an event may claim to be created.
/// Wide enough to absorb genuinely wrong client clocks, narrow enough that an
/// event cannot park itself at the top of every result set indefinitely.
const MAX_FUTURE_DRIFT_SECS: u64 = 15 * 60;

#[derive(Debug, Clone)]
pub struct ValidationMiddleware {
    relay_pubkey: PublicKey,
}

impl ValidationMiddleware {
    pub fn new(relay_pubkey: PublicKey) -> Self {
        Self { relay_pubkey }
    }

    fn validate_event(&self, event: &Event) -> Result<(), &'static str> {
        // Size and shape limits come first, and apply to the relay's own events
        // too -- a bound that the largest writer is exempt from is not a bound.
        //
        // There were none at all before this: no length limit, no tag-count
        // limit, no sanity check on `created_at`. A single 10 MB kind-9 with an
        // `h` tag was accepted and stored, and automatic pruning is switched off
        // on this deployment, so "stored" means permanently. The database had
        // already reached 5.2 GB once and had to be rebuilt offline to recover.
        if event.content.len() > MAX_CONTENT_BYTES {
            return Err("invalid: event content exceeds the size limit");
        }

        if event.tags.len() > MAX_TAGS {
            return Err("invalid: event has too many tags");
        }

        let now = Timestamp::now().as_secs();
        let created_at = event.created_at.as_secs();
        // A far-future timestamp is the interesting direction: events sort by
        // `created_at`, so one dated to 2090 pins itself to the top of every
        // query result for as long as the relay keeps it.
        if created_at > now.saturating_add(MAX_FUTURE_DRIFT_SECS) {
            return Err("invalid: event created_at is too far in the future");
        }

        // If the event is from the relay pubkey and has a 'd' tag, allow it.
        if event.pubkey == self.relay_pubkey && event.tags.find(TagKind::d()).is_some() {
            return Ok(());
        }

        // For all other cases, require an 'h' tag for group events unless the kind is in the non-group allowed set.
        if event.tags.find(TagKind::h()).is_none() && !NON_GROUP_ALLOWED_KINDS.contains(&event.kind)
        {
            return Err("invalid: group events must contain an 'h' tag");
        }

        Ok(())
    }

    // This was too much, may remove it
    #[allow(unused)]
    fn validate_filter(
        &self,
        filter: &Filter,
        authed_pubkey: Option<&PublicKey>,
    ) -> Result<(), &'static str> {
        // If the authed pubkey is the relay's pubkey, skip validation.
        if authed_pubkey == Some(&self.relay_pubkey) {
            debug!("Skipping filter validation for relay pubkey");
            return Ok(());
        }

        // Check if filter has either 'h' or 'd' tag.
        let has_h_tag = filter
            .generic_tags
            .contains_key(&SingleLetterTag::lowercase(Alphabet::H));

        let has_d_tag = filter
            .generic_tags
            .contains_key(&SingleLetterTag::lowercase(Alphabet::D));

        // Check if kinds are supported (if specified).
        let has_valid_kinds = if let Some(kinds) = &filter.kinds {
            kinds.iter().all(|kind| {
                NON_GROUP_ALLOWED_KINDS.contains(kind)
                    || matches!(
                        kind,
                        k if *k == KIND_GROUP_CREATE_9007
                            || *k == KIND_GROUP_DELETE_9008
                            || *k == KIND_GROUP_ADD_USER_9000
                            || *k == KIND_GROUP_REMOVE_USER_9001
                            || *k == KIND_GROUP_EDIT_METADATA_9002
                            || *k == KIND_GROUP_DELETE_EVENT_9005
                            || *k == KIND_GROUP_SET_ROLES_9006
                            || *k == KIND_GROUP_CREATE_INVITE_9009
                            || *k == KIND_GROUP_USER_JOIN_REQUEST_9021
                            || *k == KIND_GROUP_USER_LEAVE_REQUEST_9022
                            || ADDRESSABLE_EVENT_KINDS.contains(k)
                    )
            })
        } else {
            false
        };

        // Filter must either have valid tags or valid kinds.
        if !has_h_tag && !has_d_tag && !has_valid_kinds {
            return Err("invalid: filter must contain either 'h'/'d' tag or supported kinds");
        }

        Ok(())
    }
}

impl NostrMiddleware<()> for ValidationMiddleware {
    async fn process_inbound<Next>(
        &self,
        ctx: InboundContext<'_, (), Next>,
    ) -> Result<(), anyhow::Error>
    where
        Next: relay_builder::nostr_middleware::InboundProcessor<()>,
    {
        let Some(ClientMessage::Event(event)) = &ctx.message else {
            return ctx.next().await;
        };

        debug!(
            "[{}] Validating event kind {} with id {}",
            ctx.connection_id, event.kind, event.id
        );

        if let Err(reason) = self.validate_event(event) {
            warn!(
                "[{}] Event {} validation failed: {}",
                ctx.connection_id, event.id, reason
            );

            // Send error message
            ctx.send_message(RelayMessage::ok(event.id, false, reason))?;

            // Stop the chain here with Ok since we've handled the error
            return Ok(());
        }

        ctx.next().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn signed(content: &str, tags: Vec<Tag>, created_at: Option<Timestamp>) -> Event {
        let keys = Keys::generate();
        let mut builder = EventBuilder::new(Kind::Custom(9), content).tags(tags);
        if let Some(ts) = created_at {
            builder = builder.custom_created_at(ts);
        }
        builder.sign(&keys).await.unwrap()
    }

    fn middleware() -> ValidationMiddleware {
        ValidationMiddleware::new(Keys::generate().public_key())
    }

    #[tokio::test]
    async fn an_ordinary_group_event_is_accepted() {
        let event = signed("hello", vec![Tag::custom(TagKind::h(), ["group"])], None).await;
        assert!(middleware().validate_event(&event).is_ok());
    }

    #[tokio::test]
    async fn an_oversized_event_is_refused() {
        // Nothing bounded content before this, and pruning is off on the live
        // relay, so an accepted 10MB event was a permanent commitment.
        let huge = "x".repeat(MAX_CONTENT_BYTES + 1);
        let event = signed(&huge, vec![Tag::custom(TagKind::h(), ["group"])], None).await;
        assert!(middleware().validate_event(&event).is_err());
    }

    #[tokio::test]
    async fn an_event_at_the_size_limit_is_still_accepted() {
        let at_limit = "x".repeat(MAX_CONTENT_BYTES);
        let event = signed(&at_limit, vec![Tag::custom(TagKind::h(), ["group"])], None).await;
        assert!(
            middleware().validate_event(&event).is_ok(),
            "the limit is inclusive; only what exceeds it is refused"
        );
    }

    #[tokio::test]
    async fn an_event_with_too_many_tags_is_refused() {
        let mut tags = vec![Tag::custom(TagKind::h(), ["group"])];
        for i in 0..=MAX_TAGS {
            tags.push(Tag::custom(TagKind::t(), [i.to_string()]));
        }
        let event = signed("hi", tags, None).await;
        assert!(middleware().validate_event(&event).is_err());
    }

    #[tokio::test]
    async fn an_event_dated_far_in_the_future_is_refused() {
        // Results sort by created_at, so this would otherwise pin itself to the
        // top of every query for as long as it was stored.
        let future = Timestamp::from(Timestamp::now().as_secs() + MAX_FUTURE_DRIFT_SECS + 600);
        let event = signed(
            "hi",
            vec![Tag::custom(TagKind::h(), ["group"])],
            Some(future),
        )
        .await;
        assert!(middleware().validate_event(&event).is_err());
    }

    #[tokio::test]
    async fn a_modest_clock_skew_is_tolerated() {
        let slightly_ahead = Timestamp::from(Timestamp::now().as_secs() + 60);
        let event = signed(
            "hi",
            vec![Tag::custom(TagKind::h(), ["group"])],
            Some(slightly_ahead),
        )
        .await;
        assert!(
            middleware().validate_event(&event).is_ok(),
            "a client with a slightly fast clock must not be refused"
        );
    }

    #[tokio::test]
    async fn an_old_event_is_accepted() {
        // Only the future direction is bounded -- backfill and imports are legal.
        let old = Timestamp::from(Timestamp::now().as_secs() - 86_400 * 365);
        let event = signed("hi", vec![Tag::custom(TagKind::h(), ["group"])], Some(old)).await;
        assert!(middleware().validate_event(&event).is_ok());
    }

    #[tokio::test]
    async fn a_group_event_without_an_h_tag_is_refused() {
        let event = signed("hi", vec![], None).await;
        assert!(middleware().validate_event(&event).is_err());
    }
}
