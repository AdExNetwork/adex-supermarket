use crate::{
    cache::campaign::{ActiveCache, Client, FinalizedCache},
    util::test::discard_logger,
};
use async_trait::async_trait;
use primitives::{
    supermarket::{Campaign, Status},
    BalancesMap, ChannelId,
};
use slog::Logger;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use super::Cached;

// Consists of the index of the call and the results
type MockedCall<T> = (usize, Vec<T>);

#[derive(Debug, Clone)]
pub struct MockClient {
    collect_campaigns: Cached<MockedCall<HashMap<ChannelId, Campaign>>>,
    campaign_updates:
        Cached<MockedCall<(HashMap<ChannelId, (Status, BalancesMap)>, FinalizedCache)>>,
    logger: Logger,
}

impl MockClient {
    /// If logger is not set, then a discard logger will be used
    pub async fn init(
        collect_calls: Vec<HashMap<ChannelId, Campaign>>,
        update_calls: Vec<(HashMap<ChannelId, (Status, BalancesMap)>, FinalizedCache)>,
        logger: impl Into<Option<Logger>>,
    ) -> Self {
        Self {
            collect_campaigns: Arc::new(RwLock::new((0, collect_calls))),
            campaign_updates: Arc::new(RwLock::new((0, update_calls))),
            logger: logger.into().unwrap_or_else(discard_logger),
        }
    }
}

#[async_trait]
impl Client for MockClient {
    fn logger(&self) -> Logger {
        self.logger.clone()
    }

    async fn collect_campaigns(&self) -> HashMap<ChannelId, Campaign> {
        let calls = self.collect_campaigns.write().await;

        let (ref mut index, data) = (calls.0, &calls.1);

        assert!(
            *index < data.len(),
            "collect_campaigns was called more that the mocked results"
        );

        let call_data = data[*index].clone();

        // increment the index for the next call
        *index += 1;

        call_data
    }

    async fn fetch_campaign_updates(
        &self,
        active: &ActiveCache,
    ) -> (HashMap<ChannelId, (Status, BalancesMap)>, FinalizedCache) {
        let calls = self.campaign_updates.write().await;

        let (ref mut index, data) = (calls.0, &calls.1);

        assert!(
            *index < data.len(),
            "campaign_updates was called more that the mocked results"
        );

        let call_data = data[*index].clone();

        assert_eq!(call_data.0.len() + call_data.1.len(), active.len(), "fetching updates should always yield the same number of results as it operates on the active campaigns");

        // increment the index for the next call
        *index += 1;

        call_data
    }
}
