use crate::{Config, Error, ROUTE_UNITS_FOR_SLOT, cache::{Cache, Campaign, Client}, market::cache::CacheLike, not_found, service_unavailable, status::Status};
use chrono::Utc;
use http::header::{HeaderName, CONTENT_TYPE};
use hyper::{header::USER_AGENT, Body, Request, Response};
use primitives::{AdUnit, IPFS, ValidatorId, market::AdSlotResponse, supermarket::units_for_slot::response, supermarket::units_for_slot::response::Response as UnitsForSlotResponse, targeting::{eval_with_callback, get_pricing_bounds, input, input::Input, Output}};
use slog::{debug, error, warn, Logger};
use url::{form_urlencoded, Url};
use woothee::{parser::Parser, woothee::VALUE_UNKNOWN};

lazy_static::lazy_static! {
    pub(crate) static ref CLOUDFLARE_IPCOUNTY_HEADER: HeaderName = HeaderName::from_static("cf-ipcountry");
}

#[cfg(test)]
#[path = "units_for_slot_test.rs"]
pub mod test;

pub async fn get_units_for_slot<C: Client>(
    logger: &Logger,
    config: &Config,
    req: Request<Body>,
    caches: crate::Caches<C>,
) -> Result<Response<Body>, Error> {
    let ipfs = req.uri().path().trim_start_matches(ROUTE_UNITS_FOR_SLOT);
    if ipfs.is_empty() {
        Ok(not_found())
    } else {
        // todo: fix unwrap!
        let ipfs_r = ipfs.parse::<IPFS>()?;
        let ad_slot_response = match caches.ad_slot.get(ipfs_r).await {
            Ok(Some(response)) => {
                debug!(&logger, "Fetched AdSlot"; "AdSlot" => ipfs);
                response
            }
            Ok(None) => {
                warn!(
                    &logger,
                    "AdSlot ({}) not found in Market",
                    ipfs;
                    "AdSlot" => ipfs
                );
                return Ok(not_found());
            }
            Err(err) => {
                error!(&logger, "Error fetching AdSlot"; "AdSlot" => ipfs, "error" => ?err);

                return Ok(service_unavailable());
            }
        };

        let units = match caches.ad_type.get(ad_slot_response.slot.ad_type.clone()).await {
            Ok(units) => units,
            Err(error) => {
                error!(&logger, "Error fetching AdUnits for AdSlot"; "AdSlot" => ipfs, "error" => ?error);

                return Ok(service_unavailable());
            }
        };

        let accepted_referrers = ad_slot_response.accepted_referrers.clone();
        let fallback_unit: Option<AdUnit> = match &ad_slot_response.slot.fallback_unit {
            Some(unit_ipfs) => {
                let ipfs = unit_ipfs.parse::<IPFS>()?;
                let ad_unit = match caches.ad_units.get(ipfs.clone()).await {
                    Ok(Some(response)) => {
                        debug!(&logger, "Fetched AdUnit"; "AdUnit" => &ipfs);
                        response
                    }
                    Ok(None) => {
                        warn!(
                            &logger,
                            "AdSlot fallback AdUnit ({}) not found in Market",
                            &ipfs;
                            "AdUnit" => &ipfs,
                            "AdSlot" => ad_slot_response.slot.ipfs,
                        );

                        return Ok(not_found());
                    }
                    Err(error) => {
                        error!(&logger,
                            "Error when fetching AdSlot fallback AdUnit ({}) from Market",
                            unit_ipfs;
                            "AdSlot" => ipfs,
                            "Fallback AdUnit" => unit_ipfs,
                            "error" => ?error
                        );

                        return Ok(service_unavailable());
                    }
                };

                Some(ad_unit)
            }
            None => None,
        };

        debug!(&logger, "Fetched {} AdUnits for AdSlot", units.len(); "AdSlot" => ipfs);
        let query = req.uri().query().unwrap_or_default();
        let parsed_query = form_urlencoded::parse(query.as_bytes());

        let deposit_assets: Vec<String> = parsed_query
            .filter_map(|(key, value)| {
                if key == "depositAsset" {
                    Some(value.to_string())
                } else {
                    None
                }
            })
            .collect();
        // For each adUnits apply input
        let ua_parser = Parser::new();
        let user_agent = req
            .headers()
            .get(USER_AGENT)
            .and_then(|h| h.to_str().map(ToString::to_string).ok())
            .unwrap_or_default();
        let parsed = ua_parser.parse(&user_agent);
        // WARNING! This will return only the OS type, e.g. `Linux` and not the actual distribution name e.g. `Ubuntu`
        // By contrast `ua-parser-js` will return `Ubuntu` (distribution) and not the OS type `Linux`.
        // `UAParser(...).os.name` (`ua-parser-js: 0.7.22`)
        let user_agent_os = parsed
            .as_ref()
            .map(|p| {
                if p.os != VALUE_UNKNOWN {
                    Some(p.os.to_string())
                } else {
                    None
                }
            })
            .flatten();

        // Corresponds to `UAParser(...).browser.name` (`ua-parser-js: 0.7.22`)
        let user_agent_browser_family = parsed
            .as_ref()
            .map(|p| {
                if p.name != VALUE_UNKNOWN {
                    Some(p.name.to_string())
                } else {
                    None
                }
            })
            .flatten();

        let country = req
            .headers()
            .get(CLOUDFLARE_IPCOUNTY_HEADER.clone())
            .and_then(|h| h.to_str().map(ToString::to_string).ok());

        let hostname = Url::parse(&ad_slot_response.slot.website.clone().unwrap_or_default())
            .ok()
            .and_then(|url| url.host().map(|h| h.to_string()))
            .unwrap_or_default();

        let publisher_id = ad_slot_response.slot.owner;

        let campaigns_limited_by_earner =
            get_campaigns(&caches.campaigns, config, &deposit_assets, publisher_id).await;

        debug!(&logger, "Fetched Cache campaigns limited by earner (publisher)"; "campaigns" => campaigns_limited_by_earner.len(), "publisher_id" => %publisher_id);

        // We return those in the result (which means AdView would have those) but we don't actually use them
        // we do that in order to have the same variables as the validator, so that the `price` is the same
        let targeting_input_ad_slot = Some(input::AdSlot {
            categories: ad_slot_response.categories.clone(),
            hostname,
            alexa_rank: ad_slot_response.alexa_rank,
        });

        let mut targeting_input_base = Input {
            ad_view: None,
            global: input::Global {
                ad_slot_id: ad_slot_response.slot.ipfs.clone(),
                ad_slot_type: ad_slot_response.slot.ad_type.clone(),
                publisher_id,
                country,
                event_type: "IMPRESSION".to_string(),
                seconds_since_epoch: Utc::now(),
                user_agent_os,
                user_agent_browser_family: user_agent_browser_family.clone(),
            },
            ad_unit_id: None,
            balances: None,
            channel: None,
            ad_slot: None,
        };

        let campaigns = apply_targeting(
            config,
            logger,
            campaigns_limited_by_earner,
            targeting_input_base.clone(),
            ad_slot_response,
        )
        .await;

        targeting_input_base.ad_slot = targeting_input_ad_slot;

        let response = UnitsForSlotResponse {
            targeting_input_base,
            accepted_referrers,
            campaigns,
            fallback_unit: fallback_unit.map(|ad_unit| response::AdUnit::from(&ad_unit)),
        };

        Ok(Response::builder()
            .status(http::StatusCode::OK)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_string(&response)?))
            .expect("Should create response"))
    }
}

async fn get_campaigns<C: Client>(
    cache: &Cache<C>,
    config: &Config,
    deposit_assets: &[String],
    publisher_id: ValidatorId,
) -> Vec<Campaign> {
    let active_campaigns = cache.active.read().await;

    let (mut campaigns_by_earner, rest_of_campaigns): (Vec<&Campaign>, Vec<&Campaign>) =
        active_campaigns
            .iter()
            .filter_map(|(_, campaign)| {
                // The Supermarket has the Active status combining Active & Ready from Market
                if campaign.status == Status::Active
                    && campaign.channel.creator != publisher_id
                    && (deposit_assets.is_empty()
                        || deposit_assets.contains(&campaign.channel.deposit_asset))
                {
                    Some(campaign)
                } else {
                    None
                }
            })
            .partition(|&campaign| campaign.balances.contains_key(&publisher_id));

    if campaigns_by_earner.len() >= config.limits.max_channels_earning_from.into() {
        campaigns_by_earner.into_iter().cloned().collect()
    } else {
        campaigns_by_earner.extend(rest_of_campaigns.iter());

        campaigns_by_earner.into_iter().cloned().collect()
    }
}

async fn apply_targeting(
    config: &Config,
    logger: &Logger,
    campaigns: Vec<Campaign>,
    input_base: Input,
    ad_slot_response: AdSlotResponse,
) -> Vec<response::Campaign> {
    campaigns
        .into_iter()
        .filter_map::<response::Campaign, _>(|campaign| {
            let ad_units = campaign
                .channel
                .spec
                .ad_units
                .iter()
                .filter(|ad_unit| ad_unit.ad_type == ad_slot_response.slot.ad_type)
                .cloned()
                .collect::<Vec<_>>();

            if ad_units.is_empty() {
                None
            } else {
                let targeting_rules = if !campaign.channel.targeting_rules.is_empty() {
                    campaign.channel.targeting_rules.clone()
                } else {
                    campaign.channel.spec.targeting_rules.clone()
                };
                let campaign_input = input_base.clone().with_channel(campaign.channel.clone());

                let matching_units: Vec<response::UnitsWithPrice> = ad_units
                    .into_iter()
                    .filter_map(|ad_unit| {
                        let mut unit_input = campaign_input.clone();
                        unit_input.ad_unit_id = Some(ad_unit.ipfs.clone());

                        let pricing_bounds = get_pricing_bounds(&campaign.channel, "IMPRESSION");
                        let mut output = Output {
                            show: true,
                            boost: 1.0,
                            // only "IMPRESSION" event can be used for this `Output`
                            price: vec![("IMPRESSION".to_string(), pricing_bounds.min.clone())]
                                .into_iter()
                                .collect(),
                        };

                        let on_type_error_campaign = |error, rule| error!(logger, "Rule evaluation error for {:?}", campaign.channel.id; "error" => ?error, "rule" => ?rule);
                        eval_with_callback(&targeting_rules, &unit_input, &mut output, Some(on_type_error_campaign));

                        if !output.show {
                            return None;
                        }

                        let max_price = match output.price.get("IMPRESSION") {
                            Some(output_price) => output_price.min(&pricing_bounds.max).clone(),
                            None => pricing_bounds.max,
                        };
                        let price = pricing_bounds.min.max(max_price);

                        if price < config.limits.global_min_impression_price {
                            return None;
                        }

                        // Execute the adSlot rules after we've taken the price since they're not
                        // allowed to change the price
                        let on_type_error_adslot = |error, rule| error!(logger, "Rule evaluation error AdSlot {:?}", ad_slot_response.slot.ipfs; "error" => ?error, "rule" => ?rule);

                        eval_with_callback(&ad_slot_response.slot.rules, &unit_input, &mut output, Some(on_type_error_adslot));
                        if !output.show {
                            return None;
                        }

                        let ad_unit = response::AdUnit::from(&ad_unit);

                        Some(response::UnitsWithPrice {
                            unit: ad_unit,
                            price,
                        })
                    })
                    .collect();

                if matching_units.is_empty() {
                    None
                } else {
                    Some(response::Campaign {
                        channel: campaign.channel.into(),
                        targeting_rules,
                        units_with_price: matching_units,
                    })
                }
            }
        })
        .collect()
}
