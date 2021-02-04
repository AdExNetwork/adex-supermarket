use crate::market::MarketApi;
use async_trait::async_trait;
use primitives::{AdSlot, IPFS};

use super::cache::{Cache, ClientLike};

pub type AdSlotCache = Cache<IPFS, AdSlot, AdSlotClient>;

#[derive(Debug, Clone)]
pub struct AdSlotClient {
    market: MarketApi,
}

#[async_trait]
impl ClientLike<IPFS> for AdSlotClient {
    type Output = Result<Option<AdSlot>, reqwest::Error>;

    async fn get_fresh(&self, key: &IPFS) -> Self::Output {
        let response = self.market.fetch_slot(&key.to_string()).await?;

        Ok(response.map(|response| response.slot))
    }
}
