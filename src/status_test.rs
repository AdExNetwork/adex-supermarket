use super::*;
use crate::sentry_api::SentryApi;
use chrono::{Duration, Utc};
use primitives::{
    sentry::{
        ApproveStateValidatorMessage, LastApproved, LastApprovedResponse, NewStateValidatorMessage,
    },
    util::tests::prep_db::{DUMMY_CHANNEL, DUMMY_VALIDATOR_FOLLOWER, DUMMY_VALIDATOR_LEADER, IDS},
    validator::{ApproveState, Heartbeat, MessageTypes, NewState},
    BalancesMap, Channel,
};

use httptest::{mappers::*, responders::*, Expectation, Server, ServerPool};

static SERVER_POOL: ServerPool = ServerPool::new(4);

fn get_request_channel(server: &Server) -> Channel {
    let mut channel = DUMMY_CHANNEL.clone();
    let mut leader = DUMMY_VALIDATOR_LEADER.clone();
    leader.url = server.url_str("/leader");

    let mut follower = DUMMY_VALIDATOR_FOLLOWER.clone();
    follower.url = server.url_str("/follower");

    channel.spec.validators = (leader, follower).into();

    channel
}

fn get_heartbeat_msg(recency: Duration, from: ValidatorId) -> HeartbeatValidatorMessage {
    HeartbeatValidatorMessage {
        from,
        received: Utc::now() - recency,
        msg: MessageTypes::Heartbeat(Heartbeat {
            signature: String::new(),
            state_root: String::new(),
            timestamp: Utc::now() - recency,
        }),
    }
}

fn get_approve_state_msg() -> ApproveStateValidatorMessage {
    ApproveStateValidatorMessage {
        from: DUMMY_VALIDATOR_LEADER.id,
        received: Utc::now(),
        msg: MessageTypes::ApproveState(ApproveState {
            state_root: String::from("0x0"),
            signature: String::from("0x0"),
            is_healthy: true,
        }),
    }
}

fn get_new_state_msg() -> NewStateValidatorMessage {
    NewStateValidatorMessage {
        from: DUMMY_VALIDATOR_LEADER.id,
        received: Utc::now(),
        msg: MessageTypes::NewState(NewState {
            signature: String::from("0x0"),
            state_root: String::from("0x0"),
            balances: Default::default(),
        }),
    }
}

mod is_finalized {
    use super::*;

    #[tokio::test]
    async fn it_is_finalized_when_expired() {
        let server = SERVER_POOL.get_server();
        let mut channel = get_request_channel(&server);
        channel.valid_until = Utc::now() - Duration::seconds(5);

        let response = LastApprovedResponse {
            last_approved: None,
            heartbeats: None,
        };

        server.expect(Expectation::matching(any()).respond_with(json_encoded(response)));

        let sentry = SentryApi::new().expect("Should work");

        let actual = is_finalized(&sentry, &channel)
            .await
            .expect("Should query dummy server");

        let expected = IsFinalized::Yes {
            reason: Finalized::Expired,
            balances: Default::default(),
        };

        assert_eq!(expected, actual);
    }

    #[tokio::test]
    async fn it_is_finalized_when_in_withdraw_period() {
        let server = SERVER_POOL.get_server();
        let mut channel = get_request_channel(&server);
        channel.spec.withdraw_period_start = Utc::now() - Duration::seconds(5);

        let response = LastApprovedResponse {
            last_approved: None,
            heartbeats: None,
        };

        server.expect(Expectation::matching(any()).respond_with(json_encoded(response)));

        let sentry = SentryApi::new().expect("Should work");

        let actual = is_finalized(&sentry, &channel)
            .await
            .expect("Should query dummy server");

        let expected = IsFinalized::Yes {
            reason: Finalized::Withdraw,
            balances: Default::default(),
        };

        assert_eq!(expected, actual);
    }

    #[tokio::test]
    async fn it_is_finalized_when_channel_is_exhausted() {
        let server = SERVER_POOL.get_server();
        let channel = get_request_channel(&server);

        let leader = channel.spec.validators.leader().id;
        let follower = channel.spec.validators.follower().id;

        let deposit = channel.deposit_amount.clone();
        let balances: BalancesMap = vec![
            (leader, BigNum::from(400)),
            (follower, deposit - BigNum::from(400)),
        ]
        .into_iter()
        .collect();
        let expected_balances = balances.clone();

        let response = LastApprovedResponse {
            last_approved: Some(LastApproved {
                new_state: Some(NewStateValidatorMessage {
                    from: channel.spec.validators.leader().id,
                    received: Utc::now(),
                    msg: MessageTypes::NewState(NewState {
                        state_root: String::new(),
                        signature: String::new(),
                        balances,
                    }),
                }),
                approve_state: None,
            }),
            heartbeats: None,
        };

        server.expect(Expectation::matching(any()).respond_with(json_encoded(response)));

        let sentry = SentryApi::new().expect("Should work");

        let actual = is_finalized(&sentry, &channel)
            .await
            .expect("Should query dummy server");

        let expected = IsFinalized::Yes {
            reason: Finalized::Exhausted,
            balances: expected_balances,
        };

        assert_eq!(expected, actual);
    }

    #[tokio::test]
    async fn it_is_not_finalized() {
        let server = SERVER_POOL.get_server();
        let channel = get_request_channel(&server);

        let leader = channel.spec.validators.leader().id;
        let follower = channel.spec.validators.follower().id;

        let balances: BalancesMap = vec![(leader, 400.into()), (follower, 1.into())]
            .into_iter()
            .collect();

        let leader_response = LastApprovedResponse {
            last_approved: Some(LastApproved {
                new_state: Some(NewStateValidatorMessage {
                    from: channel.spec.validators.leader().id,
                    received: Utc::now(),
                    msg: MessageTypes::NewState(NewState {
                        state_root: String::new(),
                        signature: String::new(),
                        balances,
                    }),
                }),
                approve_state: None,
            }),
            heartbeats: None,
        };

        server.expect(Expectation::matching(any()).respond_with(json_encoded(&leader_response)));

        let sentry = SentryApi::new().expect("Should work");

        let actual = is_finalized(&sentry, &channel)
            .await
            .expect("Should query dummy server");

        let expected = IsFinalized::No {
            leader: Box::new(leader_response),
        };

        assert_eq!(expected, actual);
    }
}

mod is_offline {
    use super::*;

    use lazy_static::lazy_static;

    lazy_static! {
        static ref RECENCY: Duration = Duration::minutes(4);
    }

    fn get_messages(
        leader: Option<Vec<HeartbeatValidatorMessage>>,
        follower: Option<Vec<HeartbeatValidatorMessage>>,
    ) -> Messages {
        Messages {
            leader: LastApprovedResponse {
                last_approved: None,
                heartbeats: leader,
            },
            follower: LastApprovedResponse {
                last_approved: None,
                heartbeats: follower,
            },
            recency: *RECENCY,
        }
    }

    #[test]
    fn it_is_offline_when_no_heartbeats() {
        let messages = get_messages(Some(vec![]), Some(vec![]));

        assert!(
            is_offline(&messages),
            "Offline: If empty heartbeats for Leader & Follower!"
        );

        let messages = get_messages(None, None);

        assert!(
            is_offline(&messages),
            "Offline: If `None` heartbeats for Leader & Follower!"
        );
    }

    #[test]
    fn it_is_offline_when_it_has_one_recent_heartbeat() {
        let leader = DUMMY_VALIDATOR_LEADER.id;
        let follower = DUMMY_VALIDATOR_FOLLOWER.id;

        let leader_hb = get_heartbeat_msg(Duration::zero(), leader);
        let messages = get_messages(Some(vec![leader_hb]), None);

        assert!(
            is_offline(&messages),
            "Offline: If `None` / empty heartbeats in the Follower!"
        );

        let follower_hb = get_heartbeat_msg(Duration::zero(), follower);

        let messages = get_messages(None, Some(vec![follower_hb]));

        assert!(
            is_offline(&messages),
            "Offline: If `None` / empty heartbeats in the Leader!"
        );
    }

    #[test]
    fn it_is_offline_when_it_has_old_validators_heartbeats() {
        let leader = DUMMY_VALIDATOR_LEADER.id;
        let follower = DUMMY_VALIDATOR_FOLLOWER.id;
        let old_recency = *RECENCY + Duration::minutes(1);
        let old_leader_hb = get_heartbeat_msg(old_recency, leader);
        let old_follower_hb = get_heartbeat_msg(old_recency, follower);

        let messages = get_messages(Some(vec![old_leader_hb]), Some(vec![old_follower_hb]));

        assert!(
            is_offline(&messages),
            "Offline: If it does not have recent heartbeats in both Leader & Follower!"
        );
    }

    #[test]
    fn it_is_not_offline_when_it_has_recent_heartbeats() {
        let leader = DUMMY_VALIDATOR_LEADER.id;
        let follower = DUMMY_VALIDATOR_FOLLOWER.id;
        let recent_leader_hb = get_heartbeat_msg(Duration::zero(), leader);
        let recent_follower_hb = get_heartbeat_msg(Duration::zero(), follower);

        let messages = get_messages(Some(vec![recent_leader_hb]), Some(vec![recent_follower_hb]));

        assert_eq!(
            is_offline(&messages),
            false,
            "Not offline: If it has recent heartbeats in both Leader & Follower!"
        );
    }
}

mod is_date_recent {
    use super::*;

    #[test]
    fn now_date_is_recent() {
        let now = Utc::now();
        let recency = Duration::minutes(4);
        assert!(
            is_date_recent(recency, &now),
            "The present moment is a recent date!"
        )
    }

    #[test]
    fn slightly_past_is_recent() {
        let recency = Duration::minutes(4);
        let on_the_edge = Utc::now() - Duration::minutes(3);
        assert!(
            is_date_recent(recency, &on_the_edge),
            "When date is just as old as the recency limit, it still counts as recent"
        )
    }

    #[test]
    fn old_date_is_not_recent() {
        let recency = Duration::minutes(4);
        let past = Utc::now() - Duration::minutes(10);
        assert_eq!(
            is_date_recent(recency, &past),
            false,
            "Date older than the recency limit is not recent"
        )
    }
}

mod is_initializing {
    use super::*;

    #[test]
    fn two_empty_message_arrays() {
        let messages = Messages {
            leader: LastApprovedResponse {
                last_approved: None,
                heartbeats: Some(vec![]),
            },
            follower: LastApprovedResponse {
                last_approved: None,
                heartbeats: Some(vec![]),
            },
            recency: Duration::minutes(4),
        };

        assert_eq!(
            is_initializing(&messages),
            true,
            "Both leader heartbeats + newstate and follower heatbeats + approvestate pairs are empty arrays"
        )
    }

    #[test]
    fn leader_has_no_messages() {
        let approve_state = get_approve_state_msg();
        let heartbeat = get_heartbeat_msg(Duration::minutes(0), IDS["leader"]);
        let messages = Messages {
            leader: LastApprovedResponse {
                last_approved: None,
                heartbeats: Some(vec![]),
            },
            follower: LastApprovedResponse {
                last_approved: Some(LastApproved {
                    new_state: None,
                    approve_state: Some(approve_state),
                }),
                heartbeats: Some(vec![heartbeat]),
            },
            recency: Duration::minutes(4),
        };

        assert_eq!(
            is_initializing(&messages),
            true,
            "Leader has no new messages but the follower has heartbeats and approvestate"
        )
    }

    #[test]
    fn follower_has_no_messages() {
        let heartbeat = get_heartbeat_msg(Duration::minutes(0), IDS["leader"]);
        let new_state = get_new_state_msg();

        let messages = Messages {
            leader: LastApprovedResponse {
                last_approved: Some(LastApproved {
                    new_state: Some(new_state),
                    approve_state: None,
                }),
                heartbeats: Some(vec![heartbeat]),
            },
            follower: LastApprovedResponse {
                last_approved: None,
                heartbeats: Some(vec![]),
            },
            recency: Duration::minutes(4),
        };

        assert_eq!(
            is_initializing(&messages),
            true,
            "Follower has no new messages but leader has heartbeats and newstate"
        )
    }

    #[test]
    fn both_arrays_have_messages() {
        let leader_heartbeats = vec![get_heartbeat_msg(Duration::minutes(0), IDS["leader"])];
        let follower_heartbeats = vec![get_heartbeat_msg(Duration::minutes(0), IDS["leader"])];
        let new_state = get_new_state_msg();
        let approve_state = get_approve_state_msg();

        let messages = Messages {
            leader: LastApprovedResponse {
                last_approved: Some(LastApproved {
                    new_state: Some(new_state),
                    approve_state: None,
                }),
                heartbeats: Some(leader_heartbeats),
            },
            follower: LastApprovedResponse {
                last_approved: Some(LastApproved {
                    new_state: None,
                    approve_state: Some(approve_state),
                }),
                heartbeats: Some(follower_heartbeats),
            },
            recency: Duration::minutes(4),
        };

        assert_eq!(
            is_initializing(&messages),
            false,
            "Both arrays have messages"
        )
    }
}

mod is_disconnected {
    use super::*;

    #[test]
    fn no_recent_hbs_on_both_sides() {
        let channel = DUMMY_CHANNEL.clone();
        let leader_heartbeats = vec![get_heartbeat_msg(
            Duration::minutes(10),
            channel.spec.validators.leader().id,
        )];
        let follower_heartbeats = vec![get_heartbeat_msg(
            Duration::minutes(10),
            channel.spec.validators.leader().id,
        )];

        let messages = Messages {
            leader: LastApprovedResponse {
                last_approved: Some(LastApproved {
                    new_state: None,
                    approve_state: None,
                }),
                heartbeats: Some(leader_heartbeats),
            },
            follower: LastApprovedResponse {
                last_approved: Some(LastApproved {
                    new_state: None,
                    approve_state: None,
                }),
                heartbeats: Some(follower_heartbeats),
            },
            recency: Duration::minutes(4),
        };

        assert_eq!(
            is_disconnected(&channel, &messages),
            true,
            "Both leader and follower heartbeats have no recent messages"
        )
    }

    #[test]
    fn no_recent_follower_hbs() {
        let channel = DUMMY_CHANNEL.clone();
        let leader_heartbeats = vec![
            get_heartbeat_msg(Duration::minutes(10), channel.spec.validators.leader().id),
            get_heartbeat_msg(Duration::minutes(10), channel.spec.validators.follower().id),
        ];
        let follower_heartbeats = vec![
            get_heartbeat_msg(Duration::minutes(10), channel.spec.validators.leader().id),
            get_heartbeat_msg(Duration::minutes(10), channel.spec.validators.follower().id),
        ];

        let new_state = get_new_state_msg();
        let approve_state = get_approve_state_msg();

        let messages = Messages {
            leader: LastApprovedResponse {
                last_approved: Some(LastApproved {
                    new_state: Some(new_state),
                    approve_state: None,
                }),
                heartbeats: Some(leader_heartbeats),
            },
            follower: LastApprovedResponse {
                last_approved: Some(LastApproved {
                    new_state: None,
                    approve_state: Some(approve_state),
                }),
                heartbeats: Some(follower_heartbeats),
            },
            recency: Duration::minutes(4),
        };

        assert_eq!(
            is_disconnected(&channel, &messages),
            true,
            "No recent heartbeats on both validators"
        )
    }

    #[test]
    fn no_hb_in_leader_where_from_points_to_follower() {
        let channel = DUMMY_CHANNEL.clone();
        let leader_heartbeats = vec![get_heartbeat_msg(
            Duration::minutes(0),
            channel.spec.validators.leader().id,
        )];
        let follower_heartbeats = vec![
            get_heartbeat_msg(Duration::minutes(0), channel.spec.validators.leader().id),
            get_heartbeat_msg(Duration::minutes(0), channel.spec.validators.follower().id),
        ];

        let new_state = get_new_state_msg();
        let approve_state = get_approve_state_msg();

        let messages = Messages {
            leader: LastApprovedResponse {
                last_approved: Some(LastApproved {
                    new_state: Some(new_state),
                    approve_state: None,
                }),
                heartbeats: Some(leader_heartbeats),
            },
            follower: LastApprovedResponse {
                last_approved: Some(LastApproved {
                    new_state: None,
                    approve_state: Some(approve_state),
                }),
                heartbeats: Some(follower_heartbeats),
            },
            recency: Duration::minutes(4),
        };

        assert_eq!(
            is_disconnected(&channel, &messages),
            true,
            "Leader validator heartbeats have no recent messages that came from the follower"
        )
    }

    #[test]
    fn no_hb_in_follower_where_from_points_to_leader() {
        let channel = DUMMY_CHANNEL.clone();
        let leader_heartbeats = vec![
            get_heartbeat_msg(Duration::minutes(0), channel.spec.validators.leader().id),
            get_heartbeat_msg(Duration::minutes(0), channel.spec.validators.leader().id),
        ];
        let follower_heartbeats = vec![get_heartbeat_msg(
            Duration::minutes(0),
            channel.spec.validators.follower().id,
        )];

        let new_state = get_new_state_msg();
        let approve_state = get_approve_state_msg();

        let messages = Messages {
            leader: LastApprovedResponse {
                last_approved: Some(LastApproved {
                    new_state: Some(new_state),
                    approve_state: None,
                }),
                heartbeats: Some(leader_heartbeats),
            },
            follower: LastApprovedResponse {
                last_approved: Some(LastApproved {
                    new_state: None,
                    approve_state: Some(approve_state),
                }),
                heartbeats: Some(follower_heartbeats),
            },
            recency: Duration::minutes(4),
        };

        assert_eq!(
            is_disconnected(&channel, &messages),
            true,
            "Follower validator heartbeats have no recent messages that came from the leader"
        )
    }

    #[test]
    fn recent_hbs_coming_from_both_validators() {
        let channel = DUMMY_CHANNEL.clone();
        let leader_heartbeats = vec![
            get_heartbeat_msg(Duration::minutes(0), channel.spec.validators.leader().id),
            get_heartbeat_msg(Duration::minutes(0), channel.spec.validators.follower().id),
        ];
        let follower_heartbeats = vec![
            get_heartbeat_msg(Duration::minutes(0), channel.spec.validators.leader().id),
            get_heartbeat_msg(Duration::minutes(0), channel.spec.validators.follower().id),
        ];

        let new_state = get_new_state_msg();
        let approve_state = get_approve_state_msg();

        let messages = Messages {
            leader: LastApprovedResponse {
                last_approved: Some(LastApproved {
                    new_state: Some(new_state),
                    approve_state: None,
                }),
                heartbeats: Some(leader_heartbeats),
            },
            follower: LastApprovedResponse {
                last_approved: Some(LastApproved {
                    new_state: None,
                    approve_state: Some(approve_state),
                }),
                heartbeats: Some(follower_heartbeats),
            },
            recency: Duration::minutes(4),
        };

        assert_eq!(
            is_disconnected(&channel, &messages),
            false,
            "Leader hb has recent messages that came from the follower, and the follower has recent messages that came from the leader"
        )
    }
}
