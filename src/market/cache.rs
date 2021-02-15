use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
pub use implementation::Cache;
use std::fmt::Debug;

#[derive(Debug)]
pub struct Cached<T> {
    pub record: T,
    pub expires: DateTime<Utc>,
}

impl<R> Cached<R> {
    pub fn new(record: R, expire_after: Duration) -> Self {
        Self {
            record,
            expires: Utc::now() + expire_after,
        }
    }

    pub fn is_expired(&self) -> bool {
        Utc::now() >= self.expires
    }

    pub fn is_valid(&self) -> bool {
        !self.is_expired()
    }
}

#[async_trait(?Send)]
pub trait ClientLike<K> {
    type Output: Send + 'static;

    async fn get_fresh<'a>(&self, key: K) -> Self::Output
    where
        K: 'a;
}

#[async_trait(?Send)]
pub trait CacheLike<K> {
    type Output: Send;

    async fn get<'a>(&self, key: K) -> Self::Output
    where
        K: 'a;
}

pub mod implementation {
    use async_trait::async_trait;
    use chrono::{Duration, Utc};
    use dashmap::{mapref::entry::Entry, DashMap};
    use std::{fmt::Debug, hash::Hash, sync::Arc};

    use super::{CacheLike, Cached, ClientLike};

    #[derive(Debug, Clone)]
    pub struct Cache<K, R, C>
    where
        K: Eq + Hash + Clone + Send,
        R: Debug + Clone + Send,
        C: ClientLike<K>,
    {
        pub records: Arc<DashMap<K, Cached<R>>>,
        pub expires_duration: Duration,
        pub client: C,
    }

    #[async_trait(?Send)]
    impl<K, R, C, E> CacheLike<K> for Cache<K, R, C>
    where
        K: Eq + Hash + Clone + Send,
        R: Debug + Clone + Send + 'static,
        C: ClientLike<K, Output = Result<Option<R>, E>>,
        E: Send + 'static,
    {
        type Output = Result<Option<R>, E>;

        async fn get<'a>(&self, key: K) -> Self::Output
        where
            K: 'a,
        {
            // try fetching it from cache
            match self.records.entry(key.clone()) {
                Entry::Occupied(entry) => {
                    if entry.get().is_expired() {
                        match self.client.get_fresh(key).await {
                            Ok(Some(record)) => {
                                let cached = Cached {
                                    record: record.clone(),
                                    expires: Utc::now() + self.expires_duration,
                                };

                                entry.replace_entry(cached);

                                Ok(Some(record))
                            }
                            Ok(None) => Ok(None),
                            Err(err) => {
                                // remove the expired entry
                                entry.remove_entry();
                                // and then return the error
                                Err(err)
                            }
                        }
                    } else {
                        Ok(Some(entry.get().record.clone()))
                    }
                }
                Entry::Vacant(entry) => match self.client.get_fresh(key).await? {
                    Some(record) => {
                        let cached = Cached {
                            record: record.clone(),
                            expires: Utc::now() + self.expires_duration,
                        };

                        entry.insert(cached);

                        Ok(Some(record))
                    }
                    None => Ok(None),
                },
            }
        }
    }

    impl<K, R, C> Cache<K, R, C>
    where
        K: Eq + Hash + Clone + Send,
        R: Debug + Clone + Send,
        C: ClientLike<K>,
    {
        pub fn initialize(
            expires_duration: std::time::Duration,
            client: C,
        ) -> Result<Self, Box<dyn std::error::Error>> {
            Ok(Self {
                records: Default::default(),
                expires_duration: Duration::from_std(expires_duration)?,
                client,
            })
        }

        /// Cleans expired cache items
        pub fn clean(&self) {
            self.records.retain(|_key, record| record.is_valid())
        }
    }
}

pub mod mock {
    use super::ClientLike;
    use async_trait::async_trait;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    /// simple Mock for mocking single result
    pub struct ClientMock {
        /// The mocked responses that will be returned on each call to `get_fresh`
        responses: Arc<Vec<(String, Result<Option<u8>, String>)>>,
        // the current index at which we are
        index: AtomicUsize,
    }

    impl ClientMock {
        pub fn new(responses: Vec<(String, Result<Option<u8>, String>)>) -> Self {
            Self {
                responses: Arc::new(responses),
                index: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait(?Send)]
    impl<K: AsRef<str> + Send> ClientLike<K> for ClientMock {
        type Output = Result<Option<u8>, String>;

        async fn get_fresh<'a>(&self, key: K) -> Self::Output
        where
            K: 'a + Send,
        {
            let index = self.index.fetch_add(1, Ordering::SeqCst);
            let (resp_key, resp_output) = self
                .responses
                .get(index)
                .expect("Out of bound index for mocked responses");

            if resp_key.as_str().eq(key.as_ref()) {
                resp_output.clone()
            } else {
                panic!("The passed key doesn't match with the next in line mocked response")
            }
        }
    }

    #[cfg(test)]
    mod test {
        use super::super::{Cache, CacheLike};

        use super::*;

        #[tokio::test]
        async fn gets_records_fresh_and_from_cache() {
            let expires_duration = std::time::Duration::from_millis(50);

            // we expect the mocked client `get_fresh` to be called only once.
            let mock_cache = ClientMock::new(vec![("key1".to_string(), Ok(Some(42)))]);
            let cache = Cache::initialize(expires_duration, mock_cache).unwrap();
            let (client_fetched, cache_fetched) = {
                (
                    cache
                        .get("key1")
                        .await
                        .expect("Should fetch from Mocked Client"),
                    cache.get("key1").await.expect("Should fetch from Cache"),
                )
            };

            assert_eq!(Some(42), client_fetched);
            assert_eq!(Some(42), cache_fetched);
        }
    }
}
