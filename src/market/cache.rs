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

#[cfg(test)]
pub mod mock {
    use super::ClientLike;
    use async_trait::async_trait;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    pub type MockResponses = Vec<(String, Result<Option<u8>, String>)>;

    /// simple Mock for mocking single result
    pub struct ClientMock {
        /// The mocked responses that will be returned on each call to `get_fresh`
        responses: Arc<MockResponses>,
        // the current index at which we are
        index: AtomicUsize,
    }

    impl ClientMock {
        pub fn new(responses: MockResponses) -> Self {
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
        use super::*;

        #[tokio::test]
        #[should_panic(expected = "Out of bound index for mocked responses")]
        async fn mock_client_panics_on_out_of_bound() {
            let mock_client = ClientMock::new(vec![]);

            // empty responses should trigger out of bound if we call get_fresh() on any key
            let _result = mock_client.get_fresh("key").await;
        }

        #[tokio::test]
        #[should_panic(
            expected = "The passed key doesn't match with the next in line mocked response"
        )]
        async fn mock_client_panics_on_key_not_matching_next_expected_response() {
            let mock_client = ClientMock::new(vec![("expected".to_string(), Ok(Some(42)))]);

            let _result = mock_client.get_fresh("wrong key").await;
        }
    }
}

#[cfg(test)]
mod test {
    use super::{mock::ClientMock, Cache, CacheLike};

    #[tokio::test]
    async fn it_gets_records_fresh_and_from_cache_on_get() {
        let expires_duration = std::time::Duration::from_millis(50);

        // we expect the mocked client's `get_fresh()` to be called exactly twice!
        // if not - it will panic
        let mock_cache = ClientMock::new(vec![
            ("key1".to_string(), Ok(Some(42))),
            ("key1".to_string(), Ok(Some(84))),
        ]);
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

        tokio::time::sleep(expires_duration).await;

        let newly_fetched = cache
            .get("key1")
            .await
            .expect("Should fetch from Mocked Client");

        assert_eq!(Some(84), newly_fetched);
    }

    #[tokio::test]
    async fn it_gets_a_fresh_record_after_clean() {
        let expires_duration = std::time::Duration::from_millis(50);

        // we expect the mocked client's `get_fresh` to be called exactly twice
        // 1st on get
        // 2nd after calling `clean()`
        let mock_cache = ClientMock::new(vec![
            ("key1".to_string(), Ok(Some(42))),
            ("key1".to_string(), Ok(Some(84))),
        ]);
        let cache = Cache::initialize(expires_duration, mock_cache).unwrap();
        let first_fetched = cache
            .get("key1")
            .await
            .expect("Should fetch from Mocked Client");

        assert_eq!(Some(42), first_fetched);

        tokio::time::sleep(expires_duration + std::time::Duration::from_millis(10)).await;
        cache.clean();

        assert!(
            cache.records.is_empty(),
            "Cache should be empty after the Record has expired and we've called `clean()`"
        );

        let second_fetched = cache
            .get("key1")
            .await
            .expect("Should fetch from Mocked Client");

        assert_eq!(Some(84), second_fetched, "Should fetch a second time after the first Record has expired and has been `clean()`-ed");
    }
}
