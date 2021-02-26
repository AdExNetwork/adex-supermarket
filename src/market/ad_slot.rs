use async_trait::async_trait;
use primitives::{IPFS, market::AdSlotResponse};

use crate::market::MarketApi;

use super::cache::ClientLike;

pub type AdSlotCache = super::cache::Cache<IPFS, AdSlotResponse, AdSlotClient, Result<Option<AdSlotResponse>, reqwest::Error>>;

#[derive(Debug, Clone)]
pub struct AdSlotClient {
    pub market: MarketApi,
}

#[async_trait]
impl ClientLike<IPFS> for AdSlotClient {
    type Output = Result<Option<AdSlotResponse>, reqwest::Error>;

    async fn get_fresh<'a>(&self, key: IPFS) -> Self::Output {
        self.market.fetch_slot(&key.to_string()).await
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use crate::{
        config::DEVELOPMENT,
        market::{
            cache::{Cache, CacheLike},
            MarketApi,
        },
        util::test::discard_logger,
    };
    use chrono::{TimeZone, Utc};
    use primitives::{
        market::AdSlotResponse,
        util::tests::prep_db::{DUMMY_IPFS, IDS},
        AdSlot,
    };
    use tokio::time::sleep;
    use wiremock::{
        matchers::{method, path},
        Mock, MockServer, ResponseTemplate,
    };

    fn mock_ad_slot() -> AdSlot {
        AdSlot {
            ipfs: DUMMY_IPFS[0].to_string(),
            ad_type: "legacy_250x250".to_string(),
            archived: false,
            created: Utc.ymd(2021, 1, 1).and_hms(12, 0, 0),
            description: None,
            fallback_unit: None,
            min_per_impression: None,
            modified: None,
            owner: IDS["publisher"],
            title: Some("AdSlot 1".to_string()),
            website: Some("https://adex.network".to_string()),
            rules: vec![],
        }
    }

    #[tokio::test]
    async fn gets_record_from_market() {
        let server = MockServer::start().await;
        let ad_slot = mock_ad_slot();

        let response = AdSlotResponse {
            slot: ad_slot.clone(),
            accepted_referrers: vec![],
            categories: vec![],
            alexa_rank: None,
        };

        let market = MarketApi::new(
            (server.uri() + "/market/")
                .parse()
                .expect("Wrong Market url"),
            &DEVELOPMENT,
            discard_logger(),
        )
        .expect("Should construct MarketApi");

        Mock::given(method("GET"))
            .and(path(format!("/market/slots/{}", ad_slot.ipfs)))
            .respond_with(ResponseTemplate::new(200).set_body_json(&response))
            .expect(2)
            .mount(&server)
            .await;

        let expires_duration = std::time::Duration::from_millis(50);
        let ad_slot_client = AdSlotClient { market };
        let cache: AdSlotCache =
            Cache::initialize(expires_duration, ad_slot_client).expect("Should initialize Cache");

        // new AdSlot fetched from the Market AND
        // trigger the fetching of a cached AdSlot
        let (new_ad_slot, cached_ad_slot) = {
            (
                cache
                    .get(DUMMY_IPFS[0].clone())
                    .await
                    .expect("Should fetch from Mocked Market"),
                cache
                    .get(DUMMY_IPFS[0].clone())
                    .await
                    .expect("Should fetch from Cache"),
            )
        };

        assert_eq!(Some(&response), new_ad_slot.as_ref());
        assert_eq!(cached_ad_slot.as_ref(), new_ad_slot.as_ref());

        sleep(expires_duration + std::time::Duration::from_millis(10)).await;

        // clean Expired cache records
        cache.clean();
        assert!(
            cache.records.is_empty(),
            "Cache should be empty at this point"
        );

        // trigger the Market call again
        let fresh_ad_slot = cache
            .get(DUMMY_IPFS[0].clone())
            .await
            .expect("Should fetch from Mocked Market");
        // check if it's Some only, no need to check the actual value
        assert!(fresh_ad_slot.is_some());
        cache.clean();
        assert_eq!(1, cache.records.len())
    }
}
