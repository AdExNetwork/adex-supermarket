use super::*;
use crate::{
    status::{get_status, Status},
    Config, SentryApi,
};
use async_trait::async_trait;
use futures::future::{join_all, FutureExt};
use primitives::{Channel, ChannelId};
use reqwest::Error;
use slog::{error, info, Logger};
use std::collections::{HashMap, HashSet};
use url::Url;

#[derive(Debug, Clone)]
pub struct ApiClient {
    pub(crate) validators: HashSet<Url>,
    pub(crate) logger: Logger,
    pub(crate) sentry: SentryApi,
}

impl ApiClient {
    pub async fn init(logger: Logger, config: Config) -> Result<Self, Error> {
        info!(
            &logger,
            "Initialize Cache ApiClient"; "validators" => format_args!("{:?}", &config.validators)
        );
        let sentry = SentryApi::new(config.timeouts.validator_request)?;

        let validators = config.validators.clone();

        Ok(Self {
            validators,
            logger,
            sentry,
        })
    }
}

#[async_trait]
impl Client for ApiClient {
    async fn collect_campaigns(&self) -> HashMap<ChannelId, Campaign> {
        let mut campaigns = HashMap::new();

        for channel in get_all_channels(&self.logger, &self.sentry, &self.validators).await {
            /*
                @TODO: Issue #23 Check ChannelId and the received Channel hash
                We need to figure out a way to distinguish between Channels, check if they are the same and to remove incorrect ones
                For now just check if the channel is already inside the fetched channels and log if it is
            */
            // if campaigns.contains_key(&channel.id) {
            //     info!(
            //         logger,
            //         "Skipping Campaign ({:?}) because it's already fetched from another Validator",
            //         &channel.id
            //     )
            // } else {
            match get_status(&self.sentry, &channel).await {
                Ok((status, balances)) => {
                    let channel_id = channel.id;
                    campaigns
                        .entry(channel_id)
                        .and_modify(|campaign: &mut Campaign| {
                            campaign.status = status.clone();
                            campaign.balances = balances.clone();
                        })
                        .or_insert_with(|| Campaign::new(channel, status, balances));
                }
                Err(err) => error!(
                    self.logger,
                    "Failed to fetch Campaign ({:?}) status from Validator", channel.id; "error" => ?err
                ),
            }
            // }
        }

        campaigns
    }

    /// Reads the active campaigns and schedules a list of non-finalized campaigns
    /// for update from the Validators
    ///
    /// Checks the Campaign status:
    /// If Finalized:
    /// - Add to the Finalized cache
    /// Other statuses:
    /// - Update the Status & Balances from the latest Leader NewState
    async fn fetch_campaign_updates(
        &self,
        active: &ActiveCache,
    ) -> (HashMap<ChannelId, (Status, BalancesMap)>, FinalizedCache) {
        let mut update = HashMap::new();
        let mut finalize = HashSet::new();
        for (id, campaign) in active.iter() {
            match get_status(&self.sentry, &campaign.channel).await {
                Ok((Status::Finalized(_), _balances)) => {
                    finalize.insert(*id);
                }
                Ok((new_status, new_balances)) => {
                    update.insert(*id, (new_status, new_balances));
                }
                Err(err) => error!(
                    &self.logger,
                    "Error getting Campaign ({:?}) status", id; "error" => ?err
                ),
            };
        }

        (update, finalize)
    }

    fn logger(&self) -> Logger {
        self.logger.clone()
    }
}

/// Retrieves all channels from all Validator URLs
async fn get_all_channels<'a>(
    logger: &Logger,
    sentry: &SentryApi,
    validators: &HashSet<Url>,
) -> Vec<Channel> {
    let futures = validators.iter().map(|validator| {
        sentry
            .get_validator_channels(validator)
            .map(move |result| (validator, result))
    });

    join_all(futures)
    .await
    .into_iter()
    .filter_map(|(validator, result)| match result {
            Ok(channels) => {
                info!(logger, "Fetched {} active Channels from Validator ({})", channels.len(), validator);

                Some(channels)
            },
            Err(err) => {
                error!(logger, "Failed to fetch Channels from Validator ({})", validator; "error" => ?err);

                None
            }
        })
    .flatten()
    .collect()
}