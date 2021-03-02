use campaign::{Cache, Client};
use market::{
    ad_slot::AdSlotCache,
    ad_unit::{AdTypeCache, AdUnitsCache},
};
use primitives::IPFS;

use self::{
    campaign::ApiClient,
    market::{
        ad_slot::{AdSlotClient, AdSlotOutput},
        ad_unit::{AdTypeClient, AdTypeOutput, AdUnitsClient, AdUnitsOutput},
        ClientLike,
    },
};

pub mod campaign;
pub mod market;

pub type ApiCaches = Caches<ApiClient, AdUnitsClient, AdTypeClient, AdSlotClient, reqwest::Error>;

/// Cloning the Caches is cheap since all of them are wrapped in `std::sync::Arc`!
pub struct Caches<C, AU, AT, AS, E>
where
    C: Client,
    AU: ClientLike<IPFS, Output = AdUnitsOutput<E>>,
    AT: ClientLike<String, Output = AdTypeOutput<E>>,
    AS: ClientLike<IPFS, Output = AdSlotOutput<E>>,
    E: Send + 'static,
{
    pub campaigns: Cache<C>,
    pub ad_units: AdUnitsCache<AU, E>,
    pub ad_type: AdTypeCache<AT, E>,
    pub ad_slot: AdSlotCache<AS, E>,
}

impl<C, AU, AT, AS, E> Clone for Caches<C, AU, AT, AS, E>
where
    C: Client,
    AU: ClientLike<IPFS, Output = AdUnitsOutput<E>>,
    AT: ClientLike<String, Output = AdTypeOutput<E>>,
    AS: ClientLike<IPFS, Output = AdSlotOutput<E>>,
    E: Send + 'static,
{
    fn clone(&self) -> Self {
        Self {
            campaigns: self.campaigns.clone(),
            ad_units: self.ad_units.clone(),
            ad_type: self.ad_type.clone(),
            ad_slot: self.ad_slot.clone(),
        }
    }
}
