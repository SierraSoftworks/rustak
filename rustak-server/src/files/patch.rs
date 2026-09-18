//! Changing a stored file's metadata from the admin API.
//!
//! The Marti surface can only change four things about a resource — `tool`,
//! `mimetype`, `keywords` and `expiration` — because those are the only path
//! segments TAK's own API accepts, and letting a client rename or re-channel
//! somebody else's upload through it would be a way around the visibility rule.
//! An administrator's page is not a client, so it may also set the name, the
//! channels and whether the package ships with an enrolment.
//!
//! # The keywords decide what a package *is*
//!
//! `missionpackage` is what puts a file in a client's data-package browser, and
//! `resources.is_mission_package` is the indexed copy of that fact. They are
//! written together here, so a package that has had the keyword taken away
//! cannot stay in the browser because a column still says it belongs there.
//!
//! # Every row with the hash changes
//!
//! A hash may back several rows — the same photograph attached to two map
//! items, the same package uploaded by two people — and the Marti metadata
//! writes already work that way. Keeping one row's name in step with another's
//! is the operator's business, not a reason for this to be the one write that
//! addresses a single row.

use rusqlite::types::Value;
use rustak_api::PackageUpdate;
use rustak_core::prelude::*;

use crate::db::Database;

use super::upload::MISSION_PACKAGE;

/// Applies a change to every live row with this hash.
///
/// Answers how many rows were written, which is zero for a hash that is not
/// stored — the caller has already looked the resource up, so zero here means
/// it went between the read and the write.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if a write fails.
pub async fn apply(db: &Database, hash: &str, change: &PackageUpdate) -> Result<usize, Error> {
    if let Some(keywords) = &change.keywords {
        db.resources().set_keywords(hash, keywords.clone()).await?;
    }

    let mut sets: Vec<String> = Vec::new();
    let mut binds: Vec<Value> = vec![Value::Text(hash.to_string())];

    let mut set = |column: &str, value: Value, binds: &mut Vec<Value>| {
        binds.push(value);
        sets.push(format!("{column} = ?{}", binds.len()));
    };

    if let Some(name) = &change.name {
        set("name", Value::Text(name.clone()), &mut binds);
    }

    if let Some(tool) = &change.tool {
        set("tool", Value::Text(tool.clone()), &mut binds);
    }

    if let Some(groups) = &change.groups {
        let encoded = serde_json::to_string(groups).unwrap_or_else(|_| "[]".to_string());
        set("groups", Value::Text(encoded), &mut binds);
    }

    if let Some(install) = change.install_on_enrollment {
        set(
            "install_on_enrollment",
            Value::Integer(i64::from(install)),
            &mut binds,
        );
    }

    if let Some(at) = change.expiration {
        // TAK's own `-1` is "never", and the column holds `NULL` for it.
        let value = match at >= 0 {
            true => Value::Integer(at),
            false => Value::Null,
        };
        set("expiration", value, &mut binds);
    }

    if let Some(keywords) = &change.keywords {
        let is_package = keywords
            .iter()
            .any(|keyword| keyword.eq_ignore_ascii_case(MISSION_PACKAGE));

        set(
            "is_mission_package",
            Value::Integer(i64::from(is_package)),
            &mut binds,
        );
    }

    if sets.is_empty() {
        // Only the keywords changed, which has already been written.
        return Ok(usize::from(change.keywords.is_some()));
    }

    let sql = format!(
        "UPDATE resources SET {} WHERE hash = ?1 AND deleted_at IS NULL",
        sets.join(", ")
    );

    db.write(move |tx| tx.execute(&sql, rusqlite::params_from_iter(binds.iter())))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repos::NewResource;

    async fn seeded() -> Database {
        let db = Database::open_in_memory().await.unwrap();

        db.resources()
            .upsert(NewResource {
                hash: "aa".to_string(),
                uid: "uid-1".to_string(),
                name: "package.zip".to_string(),
                mime_type: "application/x-zip-compressed".to_string(),
                size: 12,
                tool: "public".to_string(),
                keywords: vec!["missionpackage".to_string()],
                is_mission_package: true,
                groups: vec!["Blue".to_string()],
                ..NewResource::default()
            })
            .await
            .unwrap();

        db
    }

    #[tokio::test]
    async fn every_field_an_administrator_may_set_is_written() {
        let db = seeded().await;

        let written = apply(
            &db,
            "aa",
            &PackageUpdate {
                name: Some("renamed.zip".to_string()),
                tool: Some("atak".to_string()),
                groups: Some(vec!["Red".to_string(), "Green".to_string()]),
                install_on_enrollment: Some(true),
                expiration: Some(1_790_000_000_000),
                keywords: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(written, 1);

        let row = db.resources().by_hash("aa").await.unwrap().unwrap();

        assert_eq!(row.name, "renamed.zip");
        assert_eq!(row.tool, "atak");
        assert_eq!(row.groups, vec!["Red".to_string(), "Green".to_string()]);
        assert!(row.install_on_enrollment);
        assert_eq!(row.expiration, Some(1_790_000_000_000));
    }

    #[tokio::test]
    async fn taking_the_keyword_away_takes_the_package_out_of_the_browser() {
        // A client's data-package list reads the indexed column; leaving it set
        // after the keyword went would keep a file listed that no longer says
        // it belongs there.
        let db = seeded().await;

        apply(
            &db,
            "aa",
            &PackageUpdate {
                keywords: Some(vec!["patrol".to_string()]),
                ..PackageUpdate::default()
            },
        )
        .await
        .unwrap();

        let row = db.resources().by_hash("aa").await.unwrap().unwrap();

        assert_eq!(row.keywords, vec!["patrol".to_string()]);
        assert!(!row.is_mission_package);
    }

    #[tokio::test]
    async fn a_negative_expiry_clears_it_rather_than_storing_the_past() {
        let db = seeded().await;

        apply(
            &db,
            "aa",
            &PackageUpdate {
                expiration: Some(1_790_000_000_000),
                ..PackageUpdate::default()
            },
        )
        .await
        .unwrap();
        apply(
            &db,
            "aa",
            &PackageUpdate {
                expiration: Some(-1),
                ..PackageUpdate::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(
            db.resources()
                .by_hash("aa")
                .await
                .unwrap()
                .unwrap()
                .expiration,
            None,
        );
    }

    #[tokio::test]
    async fn clearing_the_channels_is_a_change_rather_than_a_no_op() {
        let db = seeded().await;

        apply(
            &db,
            "aa",
            &PackageUpdate {
                groups: Some(Vec::new()),
                ..PackageUpdate::default()
            },
        )
        .await
        .unwrap();

        assert!(
            db.resources()
                .by_hash("aa")
                .await
                .unwrap()
                .unwrap()
                .groups
                .is_empty(),
            "an empty list means the default channel, which is not the same as nobody",
        );
    }

    #[tokio::test]
    async fn a_hash_that_is_not_stored_writes_nothing() {
        let db = seeded().await;

        assert_eq!(
            apply(
                &db,
                "bb",
                &PackageUpdate {
                    name: Some("x".to_string()),
                    ..PackageUpdate::default()
                },
            )
            .await
            .unwrap(),
            0,
        );
    }
}
