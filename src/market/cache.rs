use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use dashmap::{mapref::entry::Entry, DashMap};
use std::{fmt::Debug, hash::Hash, sync::Arc};

#[async_trait]
pub trait ClientLike<K> {
    type Output;

    async fn get_fresh(&self, key: &K) -> Self::Output;
}

#[derive(Debug, Clone)]
pub struct Cache<K: Eq + Hash + Clone, R: Debug + Clone, C: ClientLike<K>> {
    pub records: Arc<DashMap<K, Cached<R>>>,
    pub expires_duration: Duration,
    pub client: C,
}

impl<K, R, C> Cache<K, R, C>
where
    K: Eq + Hash + Clone,
    R: Debug + Clone,
    C: ClientLike<K>,
{
    // todo: own Error
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

impl<K, R, C, E> Cache<K, R, C>
where
    K: Eq + Hash + Clone,
    R: Debug + Clone,
    C: ClientLike<K, Output = Result<Option<R>, E>>,
{
    // TODO: Change to own Error!
    pub async fn get(&self, key: &K) -> Result<Option<R>, E> {
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

#[derive(Debug)]
pub struct Cached<T> {
    record: T,
    expires: DateTime<Utc>,
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
