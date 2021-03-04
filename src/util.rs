#[cfg(test)]
pub mod test {
    use slog::{o, Discard, Drain, Logger};
    use slog_async::Async;

    use crate::{
        cache::{
            campaign::Client,
            market::{
                ad_slot::{AdSlotCache, AdSlotClient},
                ad_unit::{AdTypeCache, AdTypeClient, AdUnitsCache, AdUnitsClient},
            },
        },
        market::MarketApi,
        Cache, Caches,
    };

    pub fn logger() -> Logger {
        let decorator = slog_term::TermDecorator::new().build();
        let drain = slog_term::FullFormat::new(decorator).build().fuse();
        let drain = slog_async::Async::new(drain).build().fuse();

        Logger::root(drain, o!())
    }

    pub fn discard_logger() -> Logger {
        let drain = Discard.fuse();
        let drain = Async::new(drain).build().fuse();

        Logger::root(drain, o!())
    }

    pub fn caches<C: Client>(
        campaigns: Cache<C>,
        market: MarketApi,
    ) -> Caches<C, AdUnitsClient, AdTypeClient, AdSlotClient, reqwest::Error> {
        let expires_duration = std::time::Duration::from_secs(40);

        let ad_units = AdUnitsCache::initialize(
            expires_duration,
            AdUnitsClient {
                market: market.clone(),
            },
        )
        .unwrap();

        crate::Caches {
            campaigns,
            ad_units: ad_units.clone(),
            ad_type: AdTypeCache::with_units_cache(ad_units, expires_duration).unwrap(),
            ad_slot: AdSlotCache::initialize(
                expires_duration,
                AdSlotClient {
                    market: market.clone(),
                },
            )
            .unwrap(),
        }
    }
}
