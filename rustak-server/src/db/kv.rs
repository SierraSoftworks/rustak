//! The key/value store.
//!
//! Lifted from automate's `KeyValueStore` with its `tenant` column removed.
//! What belongs here is small, opaque state that exactly one component owns and
//! reads back whole: the CA's sealed key material, ACME order state, job
//! watermarks. Anything that is filtered, sorted, joined on or constrained gets
//! its own table and its own repository instead — see `design/01` §4.4.

use std::borrow::Cow;

use rusqlite::OptionalExtension as _;
use rustak_core::prelude::*;

use super::{
    Database, Partition,
    row::{Timestamp, to_json},
};

/// The address of one value in the key/value store.
///
/// Exists so that something keeping state can say *where* it keeps it without
/// the asker having to reconstruct the address — the derivation of a key from a
/// certificate name or a feed URL is only known to be right in the one place
/// that reads and writes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateKey {
    pub partition: Cow<'static, str>,
    pub key: Cow<'static, str>,
}

impl StateKey {
    pub fn new(partition: impl Into<Cow<'static, str>>, key: impl Into<Cow<'static, str>>) -> Self {
        Self {
            partition: partition.into(),
            key: key.into(),
        }
    }
}

/// A partitioned store of JSON values addressed by key.
#[async_trait::async_trait]
pub trait KeyValueStore {
    /// Reads one value, or `None` when the key is not set.
    async fn get<T: DeserializeOwned + Send + 'static>(
        &self,
        partition: impl Into<Cow<'static, str>> + Send,
        key: impl Into<Cow<'static, str>> + Send,
    ) -> Result<Option<T>, Error>;

    /// Reads every key in one partition.
    async fn list<T: DeserializeOwned + Send + 'static>(
        &self,
        partition: impl Into<Cow<'static, str>> + Send,
    ) -> Result<Vec<(String, T)>, Error>;

    /// Writes a value, replacing whatever the key held.
    async fn set<T: Serialize + Send + 'static>(
        &self,
        partition: impl Into<Cow<'static, str>> + Send,
        key: impl Into<Cow<'static, str>> + Send,
        value: T,
    ) -> Result<(), Error>;

    /// Writes a value only if the key is free, reporting whether it was written.
    ///
    /// [`KeyValueStore::set`] overwrites, which is what almost every caller
    /// wants. This is for the callers generating their own random keys, where
    /// quietly overwriting a collision would destroy an unrelated record rather
    /// than reporting it so it can be retried.
    async fn insert<T: Serialize + Send + 'static>(
        &self,
        partition: impl Into<Cow<'static, str>> + Send,
        key: impl Into<Cow<'static, str>> + Send,
        value: T,
    ) -> Result<bool, Error>;

    /// Deletes a key. Deleting a key that is not set is not an error.
    async fn remove(
        &self,
        partition: impl Into<Cow<'static, str>> + Send,
        key: impl Into<Cow<'static, str>> + Send,
    ) -> Result<(), Error>;

    /// Every partition holding at least one value.
    async fn partitions(&self) -> Result<Vec<String>, Error>;

    /// Every value in the store, for diagnostics and export.
    async fn scan<T: DeserializeOwned + Send + 'static>(
        &self,
    ) -> Result<Vec<(String, String, T)>, Error>;

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
impl KeyValueStore for Database {
    #[instrument("db.kv.get", skip_all, err(Display))]
    async fn get<T: DeserializeOwned + Send + 'static>(
        &self,
        partition: impl Into<Cow<'static, str>> + Send,
        key: impl Into<Cow<'static, str>> + Send,
    ) -> Result<Option<T>, Error> {
        let (partition, key) = (partition.into().into_owned(), key.into().into_owned());

        self.read(move |c| {
            c.query_one(
                "SELECT value FROM kv WHERE partition = ?1 AND key = ?2",
                (partition, key),
                |row| super::row::json_col(row, 0),
            )
            .optional()
        })
        .await
    }

    #[instrument("db.kv.list", skip_all, err(Display))]
    async fn list<T: DeserializeOwned + Send + 'static>(
        &self,
        partition: impl Into<Cow<'static, str>> + Send,
    ) -> Result<Vec<(String, T)>, Error> {
        let partition = partition.into().into_owned();

        self.read(move |c| {
            let mut statement =
                c.prepare("SELECT key, value FROM kv WHERE partition = ?1 ORDER BY key ASC")?;
            let rows = statement.query_map([partition], |row| {
                Ok((row.get::<_, String>(0)?, super::row::json_col(row, 1)?))
            })?;

            rows.collect()
        })
        .await
    }

    #[instrument("db.kv.set", skip_all, err(Display))]
    async fn set<T: Serialize + Send + 'static>(
        &self,
        partition: impl Into<Cow<'static, str>> + Send,
        key: impl Into<Cow<'static, str>> + Send,
        value: T,
    ) -> Result<(), Error> {
        let (partition, key) = (partition.into().into_owned(), key.into().into_owned());

        self.write(move |tx| {
            tx.execute(
                "INSERT INTO kv (partition, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT (partition, key) \
                 DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                rusqlite::params![partition, key, to_json(&value)?, Timestamp::now()],
            )
        })
        .await?;

        Ok(())
    }

    #[instrument("db.kv.insert", skip_all, err(Display))]
    async fn insert<T: Serialize + Send + 'static>(
        &self,
        partition: impl Into<Cow<'static, str>> + Send,
        key: impl Into<Cow<'static, str>> + Send,
        value: T,
    ) -> Result<bool, Error> {
        let (partition, key) = (partition.into().into_owned(), key.into().into_owned());

        let written = self
            .write(move |tx| {
                tx.execute(
                    "INSERT INTO kv (partition, key, value, updated_at) VALUES (?1, ?2, ?3, ?4) \
                     ON CONFLICT (partition, key) DO NOTHING",
                    rusqlite::params![partition, key, to_json(&value)?, Timestamp::now()],
                )
            })
            .await?;

        Ok(written > 0)
    }

    #[instrument("db.kv.remove", skip_all, err(Display))]
    async fn remove(
        &self,
        partition: impl Into<Cow<'static, str>> + Send,
        key: impl Into<Cow<'static, str>> + Send,
    ) -> Result<(), Error> {
        let (partition, key) = (partition.into().into_owned(), key.into().into_owned());

        self.write(move |tx| {
            tx.execute(
                "DELETE FROM kv WHERE partition = ?1 AND key = ?2",
                (partition, key),
            )
        })
        .await?;

        Ok(())
    }

    #[instrument("db.kv.partitions", skip_all, err(Display))]
    async fn partitions(&self) -> Result<Vec<String>, Error> {
        self.read(|c| {
            let mut statement =
                c.prepare("SELECT DISTINCT partition FROM kv ORDER BY partition ASC")?;

            statement.query_map([], |row| row.get(0))?.collect()
        })
        .await
    }

    #[instrument("db.kv.scan", skip_all, err(Display))]
    async fn scan<T: DeserializeOwned + Send + 'static>(
        &self,
    ) -> Result<Vec<(String, String, T)>, Error> {
        self.read(|c| {
            let mut statement =
                c.prepare("SELECT partition, key, value FROM kv ORDER BY partition ASC, key ASC")?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    super::row::json_col(row, 2)?,
                ))
            })?;

            rows.collect()
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Watermark {
        seen: u64,
    }

    async fn store() -> Database {
        Database::open_in_memory().await.unwrap()
    }

    #[tokio::test]
    async fn a_value_reads_back_as_it_was_written() {
        let db = store().await;

        assert_eq!(db.get::<Watermark>("pki", "missing").await.unwrap(), None);

        db.set("pki", "root_ca", Watermark { seen: 3 })
            .await
            .unwrap();
        assert_eq!(
            db.get::<Watermark>("pki", "root_ca").await.unwrap(),
            Some(Watermark { seen: 3 })
        );
    }

    #[tokio::test]
    async fn set_overwrites_and_insert_refuses_to() {
        let db = store().await;

        db.set("pki", "k", Watermark { seen: 1 }).await.unwrap();
        db.set("pki", "k", Watermark { seen: 2 }).await.unwrap();
        assert_eq!(
            db.get::<Watermark>("pki", "k").await.unwrap().unwrap().seen,
            2
        );

        assert!(!db.insert("pki", "k", Watermark { seen: 9 }).await.unwrap());
        assert_eq!(
            db.get::<Watermark>("pki", "k").await.unwrap().unwrap().seen,
            2
        );
        assert!(
            db.insert("pki", "fresh", Watermark { seen: 9 })
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn listing_and_scanning_are_ordered_and_partitioned() {
        let db = store().await;

        db.set("b", "two", Watermark { seen: 2 }).await.unwrap();
        db.set("a", "one", Watermark { seen: 1 }).await.unwrap();
        db.set("a", "zero", Watermark { seen: 0 }).await.unwrap();

        let listed: Vec<String> = db
            .list::<Watermark>("a")
            .await
            .unwrap()
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        assert_eq!(listed, vec!["one".to_string(), "zero".to_string()]);

        assert_eq!(db.partitions().await.unwrap(), vec!["a", "b"]);
        assert_eq!(db.scan::<Watermark>().await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn removing_an_absent_key_is_not_an_error() {
        let db = store().await;

        db.remove("pki", "never-set").await.unwrap();

        db.set("pki", "k", Watermark { seen: 1 }).await.unwrap();
        db.remove("pki", "k").await.unwrap();
        assert_eq!(db.get::<Watermark>("pki", "k").await.unwrap(), None);
    }

    #[tokio::test]
    async fn the_stored_value_is_json_the_database_can_inspect() {
        let db = store().await;
        db.set("pki", "k", Watermark { seen: 7 }).await.unwrap();

        let seen: i64 = db
            .read(|c| {
                c.query_one(
                    "SELECT json_extract(value, '$.seen') FROM kv WHERE key = 'k'",
                    [],
                    |row| row.get(0),
                )
            })
            .await
            .unwrap();

        assert_eq!(seen, 7);
    }

    #[tokio::test]
    async fn a_partition_handle_addresses_one_partition() {
        let db = store().await;
        let pki = KeyValueStore::partition::<Watermark>(&db, "pki");

        pki.set("k".into(), Watermark { seen: 4 }).await.unwrap();

        assert_eq!(pki.get("k".into()).await.unwrap().unwrap().seen, 4);
        assert_eq!(pki.list().await.unwrap().len(), 1);
    }
}
