use crate::{
    market::{MarketApi, MarketUrl, Statuses},
    status::{is_finalized, IsFinalized, Status},
    Config, SentryApi,
};
use primitives::{
    market::{Campaign as MarketCampaign, StatusType, StatusType::*},
    validator::MessageTypes,
    BalancesMap, Channel, ChannelId,
};
use reqwest::Error;
use slog::{error, info, warn, Logger};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;

type Cached<T> = Arc<RwLock<T>>;

#[derive(Debug, Clone)]
pub struct Cache {
    pub active: Cached<HashMap<ChannelId, Campaign>>,
    pub finalized: Cached<HashSet<ChannelId>>,
    pub balance_from_finalized: Cached<BalancesMap>,
    market: Arc<MarketApi>,
    logger: Logger,
    sentry: SentryApi,
}

pub enum Action {
    Update,
    Finalize,
}

#[derive(Debug)]
pub struct Campaign {
    channel: Channel,
    status: Status,
    balances: BalancesMap,
}

impl From<MarketCampaign> for Campaign {
    fn from(market_campaign: MarketCampaign) -> Self {
        Self {
            channel: market_campaign.channel,
            status: Status::from(&market_campaign.status),
            balances: market_campaign.status.balances,
        }
    }
}

impl Cache {
    const NON_FINALIZED: [StatusType; 9] = [
        Active,
        Ready,
        Pending,
        Initializing,
        Waiting,
        Offline,
        Disconnected,
        Unhealthy,
        Invalid,
    ];

    /// Fetches all the campaigns from the Market and returns the Cache instance
    pub async fn initialize(
        market_url: MarketUrl,
        logger: Logger,
        config: Config,
    ) -> Result<Self, Error> {
        let market = MarketApi::new(market_url, logger.clone())?;
        let sentry = SentryApi::new(config.timeouts.validator_request)?;

        // we don't need a timeout for the initial fetching of the campaigns
        // while the applicatoin is still starting
        let all_campaigns = market.fetch_campaigns(&Statuses::All).await?;

        let (active, finalized, balances) = all_campaigns.into_iter().fold(
            (
                HashMap::default(),
                HashSet::default(),
                BalancesMap::default(),
            ),
            |(mut active, mut finalized, mut balances), market_campaign: MarketCampaign| {
                let campaign = Campaign::from(market_campaign);
                if let Status::Finalized(..) = &campaign.status {
                    // we don't care if the campaign was already in the set
                    finalized.insert(campaign.channel.id);
                }

                balances =
                    campaign
                        .balances
                        .iter()
                        .fold(balances, |mut acc, (publisher, balance)| {
                            acc.entry(publisher.clone())
                                .and_modify(|current_balance| *current_balance += balance)
                                .or_insert_with(|| balance.clone());

                            acc
                        });

                active.insert(campaign.channel.id, campaign);

                (active, finalized, balances)
            },
        );

        Ok(Self {
            market: Arc::new(market),
            active: Arc::new(RwLock::new(active)),
            finalized: Arc::new(RwLock::new(finalized)),
            balance_from_finalized: Arc::new(RwLock::new(balances)),
            logger,
            sentry,
        })
    }

    /// Will update the campaigns in the Cache, fetching new campaigns from the Market
    pub async fn fetch_new_campaigns(&self) -> Result<(), Error> {
        let statuses = Statuses::Only(&Self::NON_FINALIZED);
        let fetched_campaigns = self.market.fetch_campaigns(&statuses).await?;

        let current_campaigns = self.active.clone();

        let new_cache_campaigns: Vec<(ChannelId, Campaign)> = {
            // we need to release the read lock before writing!
            // Hence the scope of the `filtered` variable
            let read_campaigns = current_campaigns.read().await;

            fetched_campaigns
                .into_iter()
                .filter_map(|market_campaign| {
                    // if the key doesn't exist, add a campaign
                    if !read_campaigns.contains_key(&market_campaign.channel.id) {
                        Some((market_campaign.channel.id, Campaign::from(market_campaign)))
                    } else {
                        None
                    }
                })
                .collect()
        };

        if !new_cache_campaigns.is_empty() {
            let new_count = new_cache_campaigns.len();
            let mut campaigns = current_campaigns.write().await;
            campaigns.extend(new_cache_campaigns.into_iter());
            info!(
                &self.logger,
                "Added {} new Campaigns ({:?}) to the Cache", new_count, statuses
            );
        } else {
            info!(
                &self.logger,
                "No new Campaigns ({:?}) added to the Cache", statuses
            );
        }

        Ok(())
    }

    /// Reads the active campaigns and schedules a list of non-finalized campaigns
    /// for update from the Validators

    /// Checks if campaign is finalized:
    /// If Yes:
    /// - adds it to the Finalized cache
    /// - adds up the latest publishers' Balances
    /// If No:
    /// - updates the latest Balances from the latest leader's NewState
    pub async fn update_campaigns(&self) {
        // we need this scope to drop the Read Lock on `self.active`
        // before performing the finalize & update actions
        let (finalize, update) = {
            let active_campaigns = self.active.read().await;

            let mut finalize = HashMap::new();
            let mut update = HashMap::new();
            for (id, campaign) in active_campaigns.iter() {
                match self.campaign_action(campaign).await {
                    Ok((Action::Finalize, balances)) => {
                        finalize.insert(*id, balances);
                    }
                    Ok((Action::Update, balances)) => {
                        if !balances.is_empty() {
                            update.insert(*id, balances);
                        }
                    }
                    Err(err) => error!(
                        &self.logger,
                        "Error checking if Campaign ({:?}) is finalized: {}", id, err
                    ),
                };
            }

            (finalize, update)
        };

        self.finalize_campaigns(finalize).await;
        self.update_campaigns_balances(update).await;
    }

    /// - Adds the Channel Id to the Finalized cache
    /// - Adds up the latest publishers' Balances (finalized_balances)
    async fn finalize_campaigns(&self, campaigns: HashMap<ChannelId, BalancesMap>) {
        {
            // Put in finalized
            self.finalized.write().await.extend(campaigns.keys());
        }
        let mut active = self.active.write().await;
        // Sum the balances in balances_from_finalized
        let mut finalized_balances = self.balance_from_finalized.write().await;

        for (remove_id, latest_balances) in campaigns {
            for (publisher, value) in latest_balances.into_iter() {
                finalized_balances
                    .entry(publisher)
                    .and_modify(|current_balance| *current_balance += &value)
                    .or_insert(value);
            }

            match active.remove_entry(&remove_id) {
                Some((id, campaign)) => info!(
                    &self.logger,
                    "Campaign ({:?}) successfully removed from the cache. {:#?}", id, campaign
                ),
                None => warn!(
                    &self.logger,
                    "Campaign ({:?}) was not found in the cache and couldn't be deleted!",
                    remove_id
                ),
            }
        }
    }

    async fn update_campaigns_balances(&self, balances: HashMap<ChannelId, BalancesMap>) {
        let mut active = self.active.write().await;

        for (id, new_balance) in balances {
            if let Some(campaign) = active.get_mut(&id) {
                campaign.balances = new_balance;
            }
        }
    }

    /// Calls is_finalized and prepares the campaign for an Action:
    /// - Update
    /// - Finalized
    async fn campaign_action(&self, campaign: &Campaign) -> Result<(Action, BalancesMap), Error> {
        let is_finalized = is_finalized(&self.sentry, &campaign.channel).await?;

        match is_finalized {
            IsFinalized::Yes { balances, .. } => Ok((Action::Finalize, balances)),
            IsFinalized::No { leader } => {
                let new_balances = (*leader)
                    .last_approved
                    .and_then(|last_approved| last_approved.new_state)
                    .and_then(|new_state| match new_state.msg {
                        MessageTypes::NewState(new_state) => Some(new_state.balances),
                        _ => None,
                    });

                Ok((Action::Update, new_balances.unwrap_or_default()))
            }
        }
    }
}
