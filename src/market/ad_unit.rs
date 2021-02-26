use async_trait::async_trait;
use primitives::{AdUnit, IPFS};

use super::{MarketApi, cache::{Cache, Cached, ClientLike}};

pub type AdUnitsCache = Cache<IPFS, AdUnit, AdUnitsClient, Result<Option<AdUnit>, reqwest::Error>>;
pub type AdTypeCache = Cache<String, Vec<AdUnit>, AdTypeClient, Result<Vec<AdUnit>, reqwest::Error>>;

// TODO: Probably better to do it in with a builder
impl Cache<String, Vec<AdUnit>, AdTypeClient, Result<Vec<AdUnit>, reqwest::Error>> {
    pub fn with_units_cache(cache: AdUnitsCache, expires_duration: std::time::Duration) -> Result<Self, Box<dyn std::error::Error>> {
        let client = AdTypeClient {
            market: cache.client.market.clone(),
            ad_units_cache: Some(cache),
        };
        
        Self::initialize(expires_duration, client)
    }
}

#[derive(Debug, Clone)]
pub struct AdUnitsClient {
    pub market: MarketApi,
}

#[async_trait]
impl ClientLike<IPFS> for AdUnitsClient {
    type Output = Result<Option<AdUnit>, reqwest::Error>;

    async fn get_fresh<'a>(&self, ipfs: IPFS) -> Self::Output {
        match self.market.fetch_unit(&ipfs.to_string()).await? {
            Some(response) => Ok(Some(response.unit)),
            None => Ok(None),
        }
    }
 }

#[derive(Debug, Clone)]
pub struct AdTypeClient {
    market: MarketApi,
    /// Having the `AdUnit`s cache here allows us to extend it with the newly fetched records
    /// overriding any previous AdUnits (with new expiration time) and inserting new ones.
    ad_units_cache: Option<AdUnitsCache>,
}

#[async_trait]
impl ClientLike<String> for AdTypeClient {
    type Output = Result<Vec<AdUnit>, reqwest::Error>;

    /// Adds the fetched by ad_type `AdUnit`s to the `AdUnit`s Cache
    async fn get_fresh<'a>(&self, ad_type: String) -> Self::Output {
        let units = self.market.fetch_units(&ad_type).await?;

        if let Some(ref units_cache) = self.ad_units_cache {
            for ad_unit in units.iter() {
                let cached_record = Cached::new(ad_unit.clone(), units_cache.expires_duration);

                units_cache.records.insert(ad_unit.ipfs.clone(), cached_record);
            }
        }

        Ok(units)
    }
}
