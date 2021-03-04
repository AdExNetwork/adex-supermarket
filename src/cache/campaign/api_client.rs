use super::*;
use crate::{
    status::{get_status, Status},
    Config, Error, SentryApi,
};
use async_trait::async_trait;
use futures::future::{join_all, FutureExt};
use primitives::{util::ApiUrl, Channel, ChannelId};
use slog::{error, info, Logger};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct ApiClient {
    pub(crate) validators: HashSet<ApiUrl>,
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

        Ok(Self {
            validators: config.validators,
            logger,
            sentry,
        })
    }
}

#[async_trait]
impl Client for ApiClient {
    /// Collects all the campaigns from all the Validators and computes their Statuses
    /// Fetching the channel from each of the validators results to more calls to the validators,
    /// however the overhead is ok, since this will only be done every couple of minutes.
    /// If there is an invalid Channel / ChannelId the status will reflect the current state of the Channel,
    /// since it's computed based on all validators.
    async fn collect_campaigns(&self) -> HashMap<ChannelId, Campaign> {
        let mut campaigns = HashMap::new();

        for channel in get_all_channels(&self.logger, &self.sentry, &self.validators).await {
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
        }

        campaigns
    }

    /// Uses the active campaigns to schedule a list of non-finalized campaigns
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
    validators: &HashSet<ApiUrl>,
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
