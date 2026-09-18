//! Read-through caching over the key/value store.
//!
//! Lifted from automate. Every [`KeyValueStore`] is a [`Cache`], because a cache
//! entry is just a stored value carrying its own expiry — there is nothing for a
//! separate table to hold.

use std::{borrow::Cow, pin::Pin};

use rustak_core::prelude::*;

use super::{Partition, kv::KeyValueStore};

/// A stored value and the moment it stops being usable.
#[derive(Serialize, Deserialize)]
struct CacheItem<T> {
    value: T,
    expires_at: chrono::DateTime<chrono::Utc>,
}

/// A store that can answer from a cached value or build a fresh one.
#[async_trait::async_trait]
pub trait Cache {
    /// Returns the cached value, or builds, stores and returns a new one.
    ///
    /// An expired entry is rebuilt rather than served, and a rebuild that fails
    /// leaves the old entry alone — so a cache miss and a failing builder are
    /// the same outcome as far as the caller is concerned.
    async fn cached<P: Into<Cow<'static, str>> + Send, K: Into<Cow<'static, str>> + Send, T, B>(
        &self,
        partition: P,
        key: K,
        builder: B,
        ttl: chrono::Duration,
    ) -> Result<T, Error>
    where
        T: DeserializeOwned + Serialize + Clone + Send + 'static,
        B: FnOnce() -> Pin<Box<dyn Future<Output = Result<T, Error>> + Sync + Send>> + Sync + Send;

    /// A handle bound to one partition and one value type.
    fn partition<T: Serialize + DeserializeOwned + Send + 'static>(
        &self,
        name: impl ToString,
    ) -> Partition<Self, T>
    where
        Self: Sized + Clone,
    {
        Partition::new(self.clone(), name.to_string())
    }
}

#[async_trait::async_trait]
impl<KV: KeyValueStore + Sync + Send + 'static> Cache for KV {
    async fn cached<P: Into<Cow<'static, str>> + Send, K: Into<Cow<'static, str>> + Send, T, B>(
        &self,
        partition: P,
        key: K,
        builder: B,
        ttl: chrono::Duration,
    ) -> Result<T, Error>
    where
        T: DeserializeOwned + Serialize + Clone + Send + 'static,
        B: FnOnce() -> Pin<Box<dyn Future<Output = Result<T, Error>> + Sync + Send>> + Sync + Send,
    {
        let partition = partition.into();
        let key = key.into();

        if let Some(item @ CacheItem::<T> { .. }) = self.get(partition.clone(), key.clone()).await?
            && item.expires_at > chrono::Utc::now()
        {
            return Ok(item.value);
        }

        let value = builder().await?;
        self.set(
            partition,
            key,
            CacheItem {
                value: value.clone(),
                expires_at: chrono::Utc::now() + ttl,
            },
        )
        .await?;

        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;
    use crate::db::Database;

    fn counting_builder(
        calls: Arc<AtomicUsize>,
        value: u64,
    ) -> impl FnOnce() -> Pin<Box<dyn Future<Output = Result<u64, Error>> + Sync + Send>> + Sync + Send
    {
        move || {
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { Ok(value) })
        }
    }

    #[tokio::test]
    async fn a_live_entry_is_served_without_rebuilding() {
        let db = Database::open_in_memory().await.unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let ttl = chrono::TimeDelta::minutes(5);

        let first = db
            .cached("oidc", "jwks", counting_builder(calls.clone(), 1), ttl)
            .await
            .unwrap();
        let second = db
            .cached("oidc", "jwks", counting_builder(calls.clone(), 2), ttl)
            .await
            .unwrap();

        assert_eq!((first, second), (1, 1));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_expired_entry_is_rebuilt() {
        let db = Database::open_in_memory().await.unwrap();
        let calls = Arc::new(AtomicUsize::new(0));

        db.cached(
            "oidc",
            "jwks",
            counting_builder(calls.clone(), 1),
            chrono::TimeDelta::seconds(-1),
        )
        .await
        .unwrap();

        let rebuilt = db
            .cached(
                "oidc",
                "jwks",
                counting_builder(calls.clone(), 2),
                chrono::TimeDelta::minutes(5),
            )
            .await
            .unwrap();

        assert_eq!(rebuilt, 2);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_failing_builder_is_reported_rather_than_cached() {
        let db = Database::open_in_memory().await.unwrap();

        let failed: Result<u64, Error> = db
            .cached(
                "oidc",
                "jwks",
                || Box::pin(async { Err(human_errors::system("no", &["retry"])) }),
                chrono::TimeDelta::minutes(5),
            )
            .await;

        assert!(failed.is_err());
        assert_eq!(
            KeyValueStore::get::<serde_json::Value>(&db, "oidc", "jwks")
                .await
                .unwrap(),
            None
        );
    }
}
