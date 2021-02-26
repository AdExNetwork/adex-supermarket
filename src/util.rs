#[cfg(test)]
pub mod test {
    use slog::{o, Discard, Drain, Logger};
    use slog_async::Async;

    use crate::{
        cache::Client,
        market::{
            ad_slot::AdSlotCache,
            ad_unit::{AdTypeCache, AdUnitsCache},
            MarketApi,
        },
        Cache,
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

    pub fn caches<C: Client>(campaigns: Cache<C>, market: MarketApi) -> crate::Caches<C> {
        let expires_duration = std::time::Duration::from_secs(40);

        let ad_units = AdUnitsCache::initialize(
            expires_duration,
            crate::market::ad_unit::AdUnitsClient {
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
                crate::market::ad_slot::AdSlotClient {
                    market: market.clone(),
                },
            )
            .unwrap(),
            market,
        }
    }
}
