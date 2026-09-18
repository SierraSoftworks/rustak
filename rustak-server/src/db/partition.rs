//! A store handle bound to one partition and one value type.
//!
//! Lifted from automate. The point is that a component holding a `Partition`
//! cannot name another partition: the partition string is supplied once, where
//! the handle is made, instead of at every call site where it could be
//! mistyped.

use rustak_core::prelude::*;

use super::{
    cache::Cache,
    kv::KeyValueStore,
    queue::{PeekedMessage, Queue, QueueMessage, QueuedMessage},
};

/// A handle to `db`, scoped to the partition `name` and the value type `T`.
pub struct Partition<D, T> {
    pub db: D,
    pub name: String,
    _marker: std::marker::PhantomData<T>,
}

impl<D, T> Partition<D, T> {
    pub fn new(db: D, name: impl ToString) -> Self {
        Self {
            db,
            name: name.to_string(),
            _marker: std::marker::PhantomData,
        }
    }
}

impl<D: Clone, T> Clone for Partition<D, T> {
    fn clone(&self) -> Self {
        Self {
            db: self.db.clone(),
            name: self.name.clone(),
            _marker: std::marker::PhantomData,
        }
    }
}

impl<D, T> std::fmt::Debug for Partition<D, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Partition")
            .field("name", &self.name)
            .finish()
    }
}

impl<D: KeyValueStore, T: Serialize + DeserializeOwned + Send + 'static> Partition<D, T> {
    /// See [`KeyValueStore::get`].
    ///
    /// # Errors
    ///
    /// As the underlying store.
    pub async fn get(&self, key: String) -> Result<Option<T>, Error> {
        self.db.get(self.name.clone(), key).await
    }

    /// See [`KeyValueStore::set`].
    ///
    /// # Errors
    ///
    /// As the underlying store.
    pub async fn set(&self, key: String, value: T) -> Result<(), Error> {
        self.db.set(self.name.clone(), key, value).await
    }

    /// See [`KeyValueStore::insert`].
    ///
    /// # Errors
    ///
    /// As the underlying store.
    pub async fn insert(&self, key: String, value: T) -> Result<bool, Error> {
        self.db.insert(self.name.clone(), key, value).await
    }

    /// See [`KeyValueStore::remove`].
    ///
    /// # Errors
    ///
    /// As the underlying store.
    pub async fn remove(&self, key: String) -> Result<(), Error> {
        self.db.remove(self.name.clone(), key).await
    }

    /// See [`KeyValueStore::list`].
    ///
    /// # Errors
    ///
    /// As the underlying store.
    pub async fn list(&self) -> Result<Vec<(String, T)>, Error> {
        self.db.list(self.name.clone()).await
    }
}

impl<D: Queue, T: Serialize + DeserializeOwned + Send + 'static> Partition<D, T> {
    /// See [`Queue::enqueue`].
    ///
    /// # Errors
    ///
    /// As the underlying queue.
    pub async fn enqueue(
        &self,
        item: T,
        idempotency_key: Option<std::borrow::Cow<'static, str>>,
        delay: Option<chrono::Duration>,
    ) -> Result<QueuedMessage, Error> {
        self.db
            .enqueue(self.name.clone(), item, idempotency_key, delay)
            .await
    }

    /// See [`Queue::dequeue`].
    ///
    /// # Errors
    ///
    /// As the underlying queue.
    pub async fn dequeue(&self, reserve_for: chrono::Duration) -> Result<QueueMessage<T>, Error> {
        self.db.dequeue(self.name.clone(), reserve_for).await
    }

    /// See [`Queue::complete`].
    ///
    /// # Errors
    ///
    /// As the underlying queue.
    pub async fn complete(&self, msg: QueueMessage<T>) -> Result<(), Error> {
        self.db.complete(self.name.clone(), msg).await
    }

    /// See [`Queue::peek`].
    ///
    /// # Errors
    ///
    /// As the underlying queue.
    pub async fn peek(&self, max_items: usize) -> Result<Vec<PeekedMessage<T>>, Error> {
        self.db.peek(self.name.clone(), max_items).await
    }

    /// See [`Queue::purge`].
    ///
    /// # Errors
    ///
    /// As the underlying queue.
    pub async fn purge(&self, key: String) -> Result<(), Error> {
        self.db.purge(self.name.clone(), key).await
    }
}

impl<D: Cache, T: Serialize + DeserializeOwned + Clone + Send + 'static> Partition<D, T> {
    /// See [`Cache::cached`].
    ///
    /// # Errors
    ///
    /// As the underlying cache, or whatever `builder` reported.
    pub async fn cached<B>(
        &self,
        key: String,
        builder: B,
        ttl: chrono::Duration,
    ) -> Result<T, Error>
    where
        B: FnOnce() -> std::pin::Pin<Box<dyn Future<Output = Result<T, Error>> + Sync + Send>>
            + Sync
            + Send,
    {
        self.db.cached(self.name.clone(), key, builder, ttl).await
    }
}
