use crate::status::Status;
use async_trait::async_trait;
use primitives::{BalancesMap, ChannelId};
use slog::{info, Logger};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;

mod api_client;
#[cfg(test)]
pub mod mock_client;

pub use api_client::ApiClient;
#[cfg(test)]
pub use mock_client::MockClient;

// Re-export the Campaign
pub use primitives::supermarket::Campaign;

pub(crate) type Cached<T> = Arc<RwLock<T>>;

pub type ActiveCache = HashMap<ChannelId, Campaign>;
pub type FinalizedCache = HashSet<ChannelId>;

#[derive(Debug)]
pub enum ActiveAction {
    /// Update exiting Campaigns in the Cache
    Update(HashMap<ChannelId, (Status, BalancesMap)>),
    /// Add new and/or replace (if Campaign exist already) Campaigns to the Cache
    New(ActiveCache),
}
#[async_trait]
pub trait Client: core::fmt::Debug + Clone {
    fn logger(&self) -> Logger;
    /// Collects all the Campaigns
    async fn collect_campaigns(&self) -> HashMap<ChannelId, Campaign>;
    /// Collects updates on the active Campaigns passed to it
    async fn fetch_campaign_updates(
        &self,
        active: &ActiveCache,
    ) -> (HashMap<ChannelId, (Status, BalancesMap)>, FinalizedCache);
}

#[derive(Debug, Clone)]
pub struct Cache<C: Client> {
    pub active: Cached<ActiveCache>,
    pub finalized: Cached<FinalizedCache>,
    client: C,
    logger: Logger,
}

impl<C> Cache<C>
where
    C: Client,
{
    /// Fetches the new campaigns on initialization.
    pub async fn initialize(client: C) -> Self {
        let logger = client.logger().clone();
        info!(&logger, "Initialize Cache with Client"; "client" => ?&client);

        let cache = Self {
            active: Default::default(),
            finalized: Default::default(),
            logger,
            client,
        };

        // collect and initialize the active campaigns
        cache.fetch_new_campaigns().await;

        cache
    }

    /// Updates the full Cache with the new values:
    ///
    /// 1. Updates Active cache:
    /// - If we have new Campaigns it extends the Active Campaigns with the new ones (incl. updates)
    /// - If we have update for Status & BalancesMap it finds the Campaigns and updates them
    /// - Remove the Finalized `ChannelId`s from the Active Campaigns
    ///
    /// 2. Updates Finalized cache
    /// - Extend the Finalized `ChannelId`s with the new ones
    async fn update(&self, new_active: ActiveAction, new_finalized: FinalizedCache) {
        // Updates Active cache
        // - Extend the Active Campaigns with the new ones
        // - Remove the Finalized `ChannelId`s from the Active Campaigns
        {
            let mut active = self.active.write().await;
            // Log and extend active cache
            // only log messages if there are actions to take on campaigns
            match new_active {
                // This will replace existing Campaigns and it will add the newly found ones
                ActiveAction::New(new_active) if !new_active.is_empty() => {
                    info!(
                        &self.logger,
                        "Adding New / Updating {} Active Campaigns",
                        new_active.len()
                    );

                    // extend the Active Cache with new active campaigns
                    active.extend(new_active)
                }

                ActiveAction::Update(update_active) if !update_active.is_empty() => {
                    info!(
                        &self.logger,
                        "Updating {} Active Campaigns",
                        update_active.len()
                    );

                    for (channel_id, (new_status, new_balances)) in update_active {
                        active
                            .entry(channel_id)
                            .and_modify(|campaign: &mut Campaign| {
                                campaign.status = new_status;
                                campaign.balances = new_balances;
                            });
                    }
                }
                _ => {}
            }

            if !new_finalized.is_empty() {
                info!(
                    &self.logger,
                    "Finalize {} Campaigns in the Active Cache",
                    new_finalized.len()
                );
                for id in new_finalized.iter() {
                    // remove from active campaigns
                    // in practice we don't care if this value was in the active cache
                    // this is why we don't handle the `Option` returned from `remove()`
                    active.remove(id);
                }
            }
        } // Active cache - release of RwLockWriteGuard

        // Updates Finalized cache
        // - Extend the Finalized `ChannelId`s with the new ones
        if !new_finalized.is_empty() {
            info!(
                &self.logger,
                "Extend with {} Campaigns the Finalized Cache",
                new_finalized.len()
            );

            self.finalized.write().await.extend(new_finalized);
        } // Finalized cache - release of RwLockWriteGuard
    }

    /// # Update the Campaigns in the Cache
    /// - New Campaigns
    /// - New Finalized Campaigns
    pub async fn fetch_new_campaigns(&self) {
        let campaigns = self.client.collect_campaigns().await;

        let (active, finalized) = campaigns.into_iter().fold(
            (HashMap::new(), HashSet::new()),
            |(mut active, mut finalized), (id, campaign)| {
                // we don't need to check if the ChannelId is already in either active or finalized,
                // since the Campaigns (ChannelId) cannot repeat in the HashMap
                match &campaign.status {
                    Status::Finalized(_) => {
                        finalized.insert(id);
                    }
                    _ => {
                        active.insert(id, campaign);
                    }
                }
                (active, finalized)
            },
        );

        self.update(ActiveAction::New(active), finalized).await
    }

    /// Reads the active campaigns and schedules a list of non-finalized campaigns for update
    pub async fn fetch_campaign_updates(&self) {
        let (active, finalized) = self
            .client
            .fetch_campaign_updates(&*self.active.read().await)
            .await;

        self.update(ActiveAction::Update(active), finalized).await
    }
}

#[cfg(test)]
mod test {
    use super::*;

    use crate::{config::DEVELOPMENT, util::test::discard_logger};
    use crate::{
        status::test::{get_approve_state_msg, get_heartbeat_msg, get_new_state_msg},
        SentryApi,
    };
    use chrono::{Duration, Utc};
    use primitives::{
        sentry::{
            ChannelListResponse, LastApproved, LastApprovedResponse, NewStateValidatorMessage,
            ValidatorMessage, ValidatorMessageResponse,
        },
        util::{
            api::ApiUrl,
            tests::prep_db::{DUMMY_CHANNEL, DUMMY_VALIDATOR_FOLLOWER, DUMMY_VALIDATOR_LEADER},
        },
        validator::{MessageTypes, NewState},
        Channel,
    };
    use wiremock::{
        matchers::{method, path, query_param},
        Mock, MockServer, ResponseTemplate,
    };

    fn setup_cache(
        active: HashMap<ChannelId, Campaign>,
        finalized: HashSet<ChannelId>,
        validators: HashSet<ApiUrl>,
    ) -> Result<Cache<ApiClient>, Box<dyn std::error::Error>> {
        let sentry = SentryApi::new(std::time::Duration::from_secs(60))?;
        let logger = discard_logger();

        let client = ApiClient {
            logger,
            sentry,
            validators,
        };

        Ok(Cache {
            active: Arc::new(RwLock::new(active)),
            finalized: Arc::new(RwLock::new(finalized)),
            logger: client.logger().clone(),
            client,
        })
    }

    fn setup_channel(leader_url: &ApiUrl, follower_url: &ApiUrl) -> Channel {
        let mut channel = DUMMY_CHANNEL.clone();
        let mut leader = DUMMY_VALIDATOR_LEADER.clone();
        leader.url = leader_url.to_string();

        let mut follower = DUMMY_VALIDATOR_FOLLOWER.clone();
        follower.url = follower_url.to_string();

        channel.spec.validators = (leader, follower).into();

        channel
    }

    #[tokio::test]
    async fn cache_initializes() {
        let mock_server = MockServer::start().await;

        let leader_url = format!("{}/leader", mock_server.uri())
            .parse()
            .expect("Valid URL");
        let follower_url = format!("{}/follower", mock_server.uri())
            .parse()
            .expect("Valid URL");

        let channel = setup_channel(&leader_url, &follower_url);
        let channel_id = channel.id;
        let leader_id = channel.spec.validators.leader().id;
        let follower_id = channel.spec.validators.follower().id;

        let expected_balances: BalancesMap =
            vec![(leader_id, 10.into()), (follower_id, 100.into())]
                .into_iter()
                .collect();

        let mut config = DEVELOPMENT.clone();
        config.validators = vec![leader_url, follower_url].into_iter().collect();

        let leader_channels = ChannelListResponse {
            channels: vec![channel.clone()],
            total_pages: 1,
            total: 1,
            page: 0,
        };

        let leader_new_state = NewStateValidatorMessage {
            from: leader_id,
            received: Utc::now(),
            msg: MessageTypes::NewState(NewState {
                signature: String::from("0x0"),
                state_root: String::from("0x0"),
                balances: expected_balances.clone(),
                exhausted: false,
            }),
        };

        let leader_last_approved = LastApprovedResponse {
            last_approved: Some(LastApproved {
                new_state: Some(leader_new_state),
                approve_state: None,
            }),
            heartbeats: Some(vec![
                get_heartbeat_msg(Duration::zero(), follower_id),
                get_heartbeat_msg(Duration::zero(), leader_id),
            ]),
        };
        let leader_latest_new_state = ValidatorMessageResponse {
            validator_messages: vec![ValidatorMessage {
                from: follower_id,
                received: Utc::now(),
                msg: MessageTypes::NewState(NewState {
                    signature: String::from("0x0"),
                    state_root: String::from("0x0"),
                    balances: expected_balances.clone(),
                    exhausted: false,
                }),
            }],
        };

        let follower_channels = ChannelListResponse {
            channels: vec![channel.clone()],
            total_pages: 1,
            total: 1,
            page: 0,
        };

        let follower_last_approved = LastApprovedResponse {
            last_approved: Some(LastApproved {
                new_state: None,
                approve_state: Some(get_approve_state_msg(true)),
            }),
            heartbeats: Some(vec![
                get_heartbeat_msg(Duration::zero(), follower_id),
                get_heartbeat_msg(Duration::zero(), leader_id),
            ]),
        };

        Mock::given(method("GET"))
            .and(path("/leader/channel/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&leader_channels))
            .expect(1_u64)
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path(format!(
                "/leader/channel/{}/last-approved",
                channel_id
            )))
            .and(query_param("withHeartbeat", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&leader_last_approved))
            .expect(2_u64)
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path(format!(
                "/leader/channel/{}/validator-messages/{}/NewState",
                channel_id, leader_id
            )))
            .and(query_param("limit", "1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&leader_latest_new_state))
            .expect(2_u64)
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path("/follower/channel/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&follower_channels))
            .expect(1_u64)
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path(format!(
                "/follower/channel/{}/last-approved",
                channel_id
            )))
            .and(query_param("withHeartbeat", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&follower_last_approved))
            .expect(2_u64)
            .mount(&mock_server)
            .await;

        let client = ApiClient::init(discard_logger(), config)
            .await
            .expect("Should initialize");
        let cache = Cache::initialize(client).await;

        let active_cache = cache.active.read().await;
        let active_campaign = active_cache
            .get(&channel.id)
            .expect("This Campaign should exist and should be active");

        assert_eq!(Status::Active, active_campaign.status);
        assert_eq!(expected_balances, active_campaign.balances);
    }

    #[tokio::test]
    async fn cache_updates_campaign_with_new_status_and_balances_map() {
        let mock_server = MockServer::start().await;

        let leader_url = format!("{}/leader", mock_server.uri())
            .parse()
            .expect("Valid URL");
        let follower_url = format!("{}/follower", mock_server.uri())
            .parse()
            .expect("Valid URL");

        let channel = setup_channel(&leader_url, &follower_url);
        let channel_id = channel.id;
        let leader_id = channel.spec.validators.leader().id;
        let follower_id = channel.spec.validators.follower().id;

        let campaign = Campaign {
            channel,
            status: Status::Waiting,
            balances: Default::default(),
        };

        let expected_balances: BalancesMap =
            vec![(leader_id, 10.into()), (follower_id, 100.into())]
                .into_iter()
                .collect();

        let leader_new_state = NewStateValidatorMessage {
            from: leader_id,
            received: Utc::now(),
            msg: MessageTypes::NewState(NewState {
                signature: String::from("0x0"),
                state_root: String::from("0x0"),
                balances: expected_balances.clone(),
                exhausted: false,
            }),
        };

        let leader_last_approved = LastApprovedResponse {
            last_approved: Some(LastApproved {
                new_state: Some(leader_new_state),
                approve_state: None,
            }),
            heartbeats: Some(vec![
                get_heartbeat_msg(Duration::zero(), leader_id),
                get_heartbeat_msg(Duration::zero(), follower_id),
            ]),
        };

        // No ApproveState & No Heartbeats means that we are still Status::Initializing
        let follower_last_approved = LastApprovedResponse {
            last_approved: Some(LastApproved {
                new_state: None,
                approve_state: None,
            }),
            heartbeats: None,
        };

        Mock::given(method("GET"))
            .and(path(format!(
                "/leader/channel/{}/last-approved",
                channel_id
            )))
            .and(query_param("withHeartbeat", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&leader_last_approved))
            .expect(1_u64)
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path(format!(
                "/follower/channel/{}/last-approved",
                channel_id
            )))
            .and(query_param("withHeartbeat", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&follower_last_approved))
            .expect(1_u64)
            .mount(&mock_server)
            .await;

        let validators = vec![leader_url, follower_url].into_iter().collect();

        let init_active = vec![(channel_id, campaign)].into_iter().collect();

        let cache =
            setup_cache(init_active, Default::default(), validators).expect("Should create Cache");

        cache.fetch_campaign_updates().await;

        let active = cache.active.read().await;
        let cache_channel = active
            .get(&channel_id)
            .expect("Campaign should be in Active Cache");

        assert_eq!(expected_balances, cache_channel.balances);
        assert_eq!(Status::Initializing, cache_channel.status);

        assert!(cache.finalized.read().await.is_empty());
    }

    #[tokio::test]
    async fn cache_finalizes_campaign() {
        let mock_server = MockServer::start().await;

        let leader_url = format!("{}/leader", mock_server.uri())
            .parse()
            .expect("Valid URL");
        let follower_url = format!("{}/follower", mock_server.uri())
            .parse()
            .expect("Valid URL");

        let mut channel = setup_channel(&leader_url, &follower_url);
        // if `valid_until < Now` the Campaign is `Expired`
        channel.valid_until = Utc::now() - Duration::minutes(1);
        let channel_id = channel.id;
        let leader_id = channel.spec.validators.leader().id;
        let follower_id = channel.spec.validators.follower().id;

        let campaign = Campaign {
            channel,
            status: Status::Waiting,
            balances: Default::default(),
        };

        let leader_last_approved = LastApprovedResponse {
            last_approved: Some(LastApproved {
                new_state: Some(get_new_state_msg()),
                approve_state: None,
            }),
            heartbeats: Some(vec![
                get_heartbeat_msg(Duration::zero(), follower_id),
                get_heartbeat_msg(Duration::zero(), leader_id),
            ]),
        };

        Mock::given(method("GET"))
            .and(path(format!(
                "/leader/channel/{}/last-approved",
                channel_id
            )))
            .and(query_param("withHeartbeat", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&leader_last_approved))
            .expect(1_u64)
            .mount(&mock_server)
            .await;

        let validators = vec![leader_url, follower_url].into_iter().collect();

        let init_active = vec![(channel_id, campaign)].into_iter().collect();

        let cache =
            setup_cache(init_active, Default::default(), validators).expect("Should create Cache");

        cache.fetch_campaign_updates().await;

        assert!(cache.active.read().await.is_empty());
        let finalized = cache.finalized.read().await;

        assert_eq!(1, finalized.len());
        assert_eq!(Some(&channel_id), finalized.get(&channel_id));
    }

    #[tokio::test]
    async fn cache_fetches_new_campaigns_and_updates_a_campaign_in_active_cache() {
        let mock_server = MockServer::start().await;

        let leader_url = format!("{}/leader", mock_server.uri())
            .parse()
            .expect("Valid URL");
        let follower_url = format!("{}/follower", mock_server.uri())
            .parse()
            .expect("Valid URL");

        let channel = setup_channel(&leader_url, &follower_url);
        let channel_id = channel.id;
        let leader_id = channel.spec.validators.leader().id;
        let follower_id = channel.spec.validators.follower().id;

        let campaign = Campaign {
            channel: channel.clone(),
            status: Status::Waiting,
            balances: Default::default(),
        };

        let expected_balances: BalancesMap =
            vec![(leader_id, 10.into()), (follower_id, 100.into())]
                .into_iter()
                .collect();

        let leader_channels = ChannelListResponse {
            channels: vec![channel.clone()],
            total_pages: 1,
            total: 1,
            page: 0,
        };

        let leader_new_state = NewStateValidatorMessage {
            from: leader_id,
            received: Utc::now(),
            msg: MessageTypes::NewState(NewState {
                signature: String::from("0x0"),
                state_root: String::from("0x0"),
                balances: expected_balances.clone(),
                exhausted: false,
            }),
        };

        let leader_last_approved = LastApprovedResponse {
            last_approved: Some(LastApproved {
                new_state: Some(leader_new_state),
                approve_state: None,
            }),
            heartbeats: Some(vec![
                get_heartbeat_msg(Duration::zero(), follower_id),
                get_heartbeat_msg(Duration::zero(), leader_id),
            ]),
        };

        let follower_channels = ChannelListResponse {
            channels: vec![channel.clone()],
            total_pages: 1,
            total: 1,
            page: 0,
        };

        // No ApproveState & No Heartbeats means that we are still Status::Initializing
        let follower_last_approved = LastApprovedResponse {
            last_approved: Some(LastApproved {
                new_state: None,
                approve_state: None,
            }),
            heartbeats: None,
        };

        Mock::given(method("GET"))
            .and(path("/leader/channel/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&leader_channels))
            .expect(1_u64)
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path(format!(
                "/leader/channel/{}/last-approved",
                channel_id
            )))
            .and(query_param("withHeartbeat", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&leader_last_approved))
            // The second time we call is from the Follower Validator to get up to date Status of the Campaign
            .expect(2_u64)
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path("/follower/channel/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&follower_channels))
            .expect(1_u64)
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path(format!(
                "/follower/channel/{}/last-approved",
                channel_id
            )))
            .and(query_param("withHeartbeat", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&follower_last_approved))
            .expect(2_u64)
            .mount(&mock_server)
            .await;

        let validators = vec![leader_url, follower_url].into_iter().collect();

        let init_active = vec![(channel_id, campaign)].into_iter().collect();

        let cache =
            setup_cache(init_active, Default::default(), validators).expect("Should create Cache");

        cache.fetch_new_campaigns().await;

        let active = cache.active.read().await;
        let cache_channel = active
            .get(&channel_id)
            .expect("Campaign should be in Active Cache");

        assert_eq!(expected_balances, cache_channel.balances);
        assert_eq!(Status::Initializing, cache_channel.status);

        assert!(cache.finalized.read().await.is_empty());
    }

    #[tokio::test]
    /// Leader:
    /// - Single Channel with Waiting status in Cache
    ///     - validUntil is set ot < now - this will lead to get_status() returning Finalized::Expired
    /// - NewState with expected BalancesMap
    /// - Leader Heartbeat
    /// - Follower Heartbeat
    ///
    /// Follower:
    /// - Single Channel from Follower `/channel/list`
    ///
    /// Final status: Finalized::Expired
    async fn cache_fetches_new_campaigns_and_cache_finalizes_campaign() {
        let mock_server = MockServer::start().await;

        let leader_url = format!("{}/leader", mock_server.uri())
            .parse()
            .expect("Valid URL");
        let follower_url = format!("{}/follower", mock_server.uri())
            .parse()
            .expect("Valid URL");

        let mut channel = setup_channel(&leader_url, &follower_url);
        // if `valid_until < Now` the Campaign is `Expired`
        channel.valid_until = Utc::now() - Duration::minutes(1);
        let channel_id = channel.id;
        let leader_id = channel.spec.validators.leader().id;
        let follower_id = channel.spec.validators.follower().id;

        let campaign = Campaign {
            channel: channel.clone(),
            status: Status::Waiting,
            balances: Default::default(),
        };

        let leader_channels = ChannelListResponse {
            channels: vec![channel.clone()],
            total_pages: 1,
            total: 1,
            page: 0,
        };

        Mock::given(method("GET"))
            .and(path("/leader/channel/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&leader_channels))
            .expect(1_u64)
            .mount(&mock_server)
            .await;

        let follower_channels = ChannelListResponse {
            channels: vec![channel.clone()],
            total_pages: 1,
            total: 1,
            page: 0,
        };

        Mock::given(method("GET"))
            .and(path("/follower/channel/list"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&follower_channels))
            .expect(1_u64)
            .mount(&mock_server)
            .await;

        let leader_last_approved = LastApprovedResponse {
            last_approved: Some(LastApproved {
                new_state: Some(get_new_state_msg()),
                approve_state: None,
            }),
            heartbeats: Some(vec![
                get_heartbeat_msg(Duration::zero(), follower_id),
                get_heartbeat_msg(Duration::zero(), leader_id),
            ]),
        };

        Mock::given(method("GET"))
            .and(path(format!(
                "/leader/channel/{}/last-approved",
                channel_id
            )))
            .and(query_param("withHeartbeat", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&leader_last_approved))
            // The second time we call is from the Follower Validator to get up to date Status of the Campaign
            .expect(2_u64)
            .mount(&mock_server)
            .await;

        let validators = vec![leader_url, follower_url].into_iter().collect();

        let init_active = vec![(channel_id, campaign)].into_iter().collect();

        let cache =
            setup_cache(init_active, Default::default(), validators).expect("Should create Cache");

        cache.fetch_new_campaigns().await;

        let active = cache.active.read().await;

        assert!(active.is_empty());

        let finalized = cache.finalized.read().await;

        assert_eq!(1, finalized.len());
        assert_eq!(Some(&channel_id), finalized.get(&channel_id));
    }
}
