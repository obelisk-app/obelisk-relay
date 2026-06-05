use nostr_sdk::prelude::*;
use relay_builder::nostr_middleware::{InboundContext, NostrMiddleware};

const INDEXED_SEARCH_DISABLED: &str = "unsupported: NIP-50 indexed search is disabled";

#[derive(Debug, Clone)]
pub struct SearchCapabilityMiddleware {
    enabled: bool,
}

impl SearchCapabilityMiddleware {
    pub fn new(enabled: bool) -> Self {
        Self { enabled }
    }
}

impl NostrMiddleware<()> for SearchCapabilityMiddleware {
    async fn process_inbound<Next>(
        &self,
        ctx: InboundContext<'_, (), Next>,
    ) -> Result<(), anyhow::Error>
    where
        Next: relay_builder::nostr_middleware::InboundProcessor<()>,
    {
        if self.enabled {
            return ctx.next().await;
        }

        if let Some(subscription_id) = message_search_subscription_id(ctx.message.as_ref()) {
            ctx.send_message(RelayMessage::closed(
                subscription_id,
                INDEXED_SEARCH_DISABLED,
            ))?;
            return Ok(());
        }

        ctx.next().await
    }
}

fn message_search_subscription_id(
    message: Option<&ClientMessage<'static>>,
) -> Option<SubscriptionId> {
    match message {
        Some(ClientMessage::Req {
            subscription_id,
            filters,
        }) if filters.iter().any(|filter| filter_uses_search(filter)) => {
            Some(subscription_id.as_ref().clone())
        }
        Some(ClientMessage::Count {
            subscription_id,
            filter,
        }) if filter_uses_search(filter) => Some(subscription_id.as_ref().clone()),
        _ => None,
    }
}

fn filter_uses_search(filter: &Filter) -> bool {
    filter.search.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_req_search_filters() {
        let subscription_id = SubscriptionId::new("search-req");
        let message = ClientMessage::req(
            subscription_id.clone(),
            Filter::new().search("indexed messages"),
        );

        assert_eq!(
            message_search_subscription_id(Some(&message)),
            Some(subscription_id)
        );
    }

    #[test]
    fn detects_count_search_filter() {
        let subscription_id = SubscriptionId::new("search-count");
        let message = ClientMessage::count(subscription_id.clone(), Filter::new().search("hello"));

        assert_eq!(
            message_search_subscription_id(Some(&message)),
            Some(subscription_id)
        );
    }

    #[test]
    fn ignores_messages_without_search_filters() {
        let message = ClientMessage::req(SubscriptionId::new("plain"), Filter::new().limit(10));

        assert_eq!(message_search_subscription_id(Some(&message)), None);
    }

    #[test]
    fn treats_empty_search_as_search_usage() {
        let subscription_id = SubscriptionId::new("empty-search");
        let message = ClientMessage::req(subscription_id.clone(), Filter::new().search(""));

        assert_eq!(
            message_search_subscription_id(Some(&message)),
            Some(subscription_id)
        );
    }
}
