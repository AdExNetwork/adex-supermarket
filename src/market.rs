use primitives::{
    market::{AdSlotResponse, AdUnitResponse, AdUnitsResponse, Campaign, StatusType},
    util::ApiUrl,
    AdSlot, AdUnit,
};
use reqwest::{Client, Error, StatusCode};
use slog::{info, Logger};
use std::fmt;

use crate::Config;

pub use proxy::Proxy;

pub type MarketUrl = ApiUrl;
pub type Result<T> = std::result::Result<T, Error>;

pub mod ad_slot;
pub mod cache;

#[derive(Debug, Clone)]
pub struct MarketApi {
    pub market_url: MarketUrl,
    client: Client,
    logger: Logger,
}

/// Should we query All or only certain statuses
#[derive(Debug)]
pub enum Statuses<'a> {
    All,
    Only(&'a [StatusType]),
}

impl fmt::Display for Statuses<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use Statuses::*;

        match self {
            All => write!(f, "all"),
            Only(statuses) => {
                let statuses = statuses.iter().map(ToString::to_string).collect::<Vec<_>>();

                write!(f, "status={}", statuses.join(","))
            }
        }
    }
}

impl MarketApi {
    /// The limit of Campaigns per page when fetching
    /// Limit the value to MAX(500)
    const MARKET_CAMPAIGNS_LIMIT: u64 = 500;
    /// The limit of AdUnits per page when fetching
    /// It should always be > 1
    const MARKET_AD_UNITS_LIMIT: u64 = 1_000;

    pub fn new(market_url: MarketUrl, config: &Config, logger: Logger) -> Result<Self> {
        // @TODO: maybe add timeout?
        let client = Client::builder()
            .tcp_keepalive(config.market.keep_alive_interval)
            .cookie_store(true)
            .build()?;

        Ok(Self {
            market_url,
            client,
            logger,
        })
    }

    /// ipfs: ipfs hash
    /// Handles the 404 case, returning a None, instead of Error
    pub async fn fetch_slot(&self, ipfs: &str) -> Result<Option<AdSlotResponse>> {
        let url = self
            .market_url
            .join(&format!("slots/{}", ipfs))
            .expect("Wrong Market Url for /slots/{IPFS} endpoint");

        let response = self.client.get(url).send().await?;
        if StatusCode::NOT_FOUND == response.status() {
            Ok(None)
        } else {
            let ad_slot_response = response.json::<AdSlotResponse>().await?;
            Ok(Some(ad_slot_response))
        }
    }

    pub async fn fetch_unit(&self, ipfs: &str) -> Result<Option<AdUnitResponse>> {
        let url = self
            .market_url
            .join(&format!("units/{}", ipfs))
            .expect("Wrong Market Url for /units/{IPFS} endpoint");

        match self.client.get(url).send().await?.error_for_status() {
            Ok(response) => {
                let ad_unit_response = response.json::<AdUnitResponse>().await?;

                Ok(Some(ad_unit_response))
            }
            // if we have a `404 Not Found` error, return None
            Err(err) if err.status() == Some(StatusCode::NOT_FOUND) => Ok(None),
            Err(err) => Err(err),
        }
    }

    pub async fn fetch_units(&self, ad_slot: &AdSlot) -> Result<Vec<AdUnit>> {
        let mut units = Vec::new();
        let mut skip: u64 = 0;
        let limit = Self::MARKET_AD_UNITS_LIMIT;

        loop {
            // if one page fail, simply return the error for now
            let mut page_results = self.fetch_units_page(&ad_slot.ad_type, skip).await?;
            // get the count before appending the page results to all
            let count = page_results.len() as u64;

            // append all received units
            units.append(&mut page_results);
            // add the number of results we need to skip in the next iteration
            skip += count;

            // if the Market returns < market fetch limit
            // we've got all AdSlots from all pages!
            if count < limit {
                // so break out of the loop
                break;
            }
        }

        Ok(units)
    }

    /// `skip` - how many records it should skip (pagination)
    async fn fetch_units_page(&self, ad_type: &str, skip: u64) -> Result<Vec<AdUnit>> {
        let url = self
            .market_url
            .join(&format!(
                "units?limit={}&skip={}&type={}",
                Self::MARKET_AD_UNITS_LIMIT,
                skip,
                ad_type,
            ))
            .expect("Wrong Market Url for /units endpoint");

        let response = self.client.get(url).send().await?;

        let ad_units: AdUnitsResponse = response.json().await?;

        Ok(ad_units.0)
    }

    pub async fn fetch_campaigns(&self, statuses: &Statuses<'_>) -> Result<Vec<Campaign>> {
        let mut campaigns = Vec::new();
        let mut skip: u64 = 0;

        loop {
            // if one page fail, simply return the error for now
            let mut page_results = self.fetch_campaigns_page(&statuses, skip).await?;
            // get the count before appending the page results to all
            let count = page_results.len() as u64;

            // append all received campaigns
            campaigns.append(&mut page_results);
            // add the number of results we need to skip in the next iteration
            skip += count;

            // if the Market returns < market fetch limit
            // we've got all Campaigns from all pages!
            if count < Self::MARKET_CAMPAIGNS_LIMIT {
                // so break out of the loop
                break;
            }
        }

        Ok(campaigns)
    }

    /// `skip` - how many records it should skip (pagination)
    async fn fetch_campaigns_page(
        &self,
        statuses: &Statuses<'_>,
        skip: u64,
    ) -> Result<Vec<Campaign>> {
        let url = self
            .market_url
            .join(&format!(
                "campaigns?{}&limit={}&skip={}",
                statuses,
                Self::MARKET_CAMPAIGNS_LIMIT,
                skip
            ))
            .expect("Wrong Market Url for /campaigns endpoint");

        let response = self.client.get(url.clone()).send().await?;

        let campaigns: Vec<Campaign> = response.json().await?;

        info!(
            &self.logger,
            "{} campaigns fetched from {}",
            campaigns.len(),
            url
        );

        Ok(campaigns)
    }
}

mod proxy {
    use std::sync::Arc;

    use http::{
        header::{HeaderMap, HeaderName, HOST},
        HeaderValue, Method, Request, Response, Uri,
    };
    use hyper::{client::connect::HttpConnector, Body, Client};
    use hyper_tls::HttpsConnector;
    use slog::{debug, Logger};
    use thiserror::Error;

    use crate::Config;

    use super::MarketUrl;

    type HyperClient = Client<HttpsConnector<HttpConnector>>;

    #[derive(Debug, Error)]
    pub enum Error {
        #[error("Proxy request to Market Method: `{uri}` URI: `{method}`")]
        Proxy {
            uri: Uri,
            method: Method,
            source: hyper::Error,
        },
        #[error("Failed to parse URI for request `{uri}`")]
        Uri {
            uri: String,
            source: http::uri::InvalidUri,
        },
    }

    #[derive(Debug, Clone)]
    struct DefaultHeaders {
        /// Headers set on requesting the Market URL
        request: HeaderMap,
        /// Additionally set Headers on the Supermarket response on top of the proxied response Headers
        response: HeaderMap,
    }

    /// Proxy used for proxying all requests intended for the Market.
    /// It's cheap to `clone()` it.
    ///
    /// Internally it uses [`hyper::Client`](hyper::Client) with [`hyper_tls::HttpsConnector`](hyper_tls::HttpsConnector)
    #[derive(Debug, Clone)]
    pub struct Proxy {
        inner: Arc<ProxyInner>,
    }

    #[derive(Debug, Clone)]
    pub struct ProxyInner {
        client: HyperClient,
        default_headers: DefaultHeaders,
        market_url: MarketUrl,
        logger: Logger,
    }

    impl Proxy {
        /// Creates a new Proxy Client with default HTTP Headers:
        ///
        /// For requests to the Market:
        ///
        /// - `HOST` - `market_url` is used to set the `HOST` header of the request (required for Cloudflare)
        ///    This is done because the initial request that we proxy contains a `HOST` header so we need to override it with the correct one - the Market.
        ///
        /// For the response of the Supermarket (before proxied response is returned):
        ////
        /// - `x-served-by` with value `adex-supermarket-proxy`
        ///
        /// Sets the HTTP/1 & HTTP/2 `Keep-Alive` based on the passed [`Config`](crate::Config)
        pub fn new(market_url: MarketUrl, config: &Config, logger: Logger) -> Self {
            // for Cloudflare we need to add a HOST header
            let market_host_header = {
                let url = market_url.to_url();
                let host = url
                    .host_str()
                    .expect("MarketUrl always has a host")
                    .to_string();

                match url.port() {
                    Some(port) => format!("{}:{}", host, port),
                    None => host,
                }
            };

            let host: HeaderValue = market_host_header
                .parse()
                .expect("The MarketUrl should be valid HOST header");

            let client = {
                let mut http = hyper::client::HttpConnector::new();
                http.set_keepalive(Some(config.market.keep_alive_interval));
                // allow of `https://` in URIs
                http.enforce_http(false);

                let https = HttpsConnector::new_with_connector(http);

                // since original request contains a HOST header we need to manually override it and we can't use `.set_host(true)`
                Client::builder()
                    .http2_keep_alive_interval(config.market.keep_alive_interval)
                    .build(https)
            };

            Self {
                inner: Arc::new(ProxyInner {
                    client,
                    default_headers: DefaultHeaders {
                        request: vec![(HOST, host)].into_iter().collect(),
                        response: vec![(
                            HeaderName::from_static("x-served-by"),
                            HeaderValue::from_static("adex-supermarket-proxy"),
                        )]
                        .into_iter()
                        .collect(),
                    },
                    market_url,
                    logger,
                }),
            }
        }

        /// Also sets the default headers like `HOST: marketUrl`
        pub async fn proxy(&self, mut request: Request<Body>) -> Result<Response<Body>, Error> {
            let method = request.method().clone();
            let path_and_query = request
                .uri()
                .path_and_query()
                .map(|p_q| {
                    let string = p_q.to_string();
                    // the MarketUrl (i.e. ApiUrl) always suffixes the path
                    string
                        .strip_prefix('/')
                        .map(ToString::to_string)
                        .unwrap_or(string)
                })
                .unwrap_or_default();

            let uri_string = format!("{}{}", self.inner.market_url, path_and_query);
            let uri = uri_string.parse::<Uri>().map_err(|err| Error::Uri {
                uri: uri_string,
                source: err,
            })?;
            *request.uri_mut() = uri.clone();
            request
                .headers_mut()
                .extend(self.inner.default_headers.request.clone());

            let mut proxy_response =
                self.inner
                    .client
                    .request(request)
                    .await
                    .map_err(|err| Error::Proxy {
                        uri: uri.clone(),
                        method: method.clone(),
                        source: err,
                    })?;

            // add the additional response headers to the Response from the Market
            proxy_response
                .headers_mut()
                .extend(self.inner.default_headers.response.clone());

            debug!(&self.inner.logger, "Proxied request to Market"; "uri" => %uri, "method" => %method);

            Ok(proxy_response)
        }
    }
}
