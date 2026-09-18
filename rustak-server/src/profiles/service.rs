//! What the profile routes call.
//!
//! The routes parse and the service decides: which profiles this caller is
//! owed, which files those profiles carry, what the newest change among them
//! was. Everything that reaches a device goes through [`ProfileService`], so
//! the group check and the `Last-Modified` computation exist once.
//!
//! # The enrolment profile is never empty
//!
//! Even an installation that has configured nothing sends
//! `rustak-enrollment.pref`, because it carries
//! `deviceProfileEnableOnConnect`. Without it a device never asks for a
//! connection profile, and every profile an operator later configures silently
//! does nothing. That is a phantom bug worth spending one file to avoid.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use rustak_api::{GroupName, PrefEntry, Profile};
use rustak_core::identity::{Direction, GroupSet};
use rustak_core::prelude::*;
use tokio::io::AsyncReadExt as _;

use super::builder::ProfileFileData;
use super::model::{Delivery, ProfileFileRow, ProfileRow};
use super::prefs::{APP_PREFS, PrefGroup, UserSettings, enrollment_defaults, render};
use super::repo::ProfilesRepo;
use crate::db::Database;
use crate::store::ContentStore;

/// The generated preference file every enrolment profile carries.
pub const ENROLLMENT_PREF: &str = "rustak-enrollment.pref";

/// The file a profile's own preferences are rendered into.
fn pref_filename(profile: &ProfileRow) -> String {
    format!("{}.pref", profile.name.replace(['/', '\\'], "-"))
}

/// A set of files and the newest change among them.
#[derive(Debug, Clone, Default)]
pub struct Assembled {
    pub files: Vec<ProfileFileData>,
    /// The newest `updated`, truncated to the second — millisecond jitter
    /// against an `If-Modified-Since` a client echoed back would otherwise
    /// report a change that did not happen.
    pub last_modified: Option<DateTime<Utc>>,
}

impl Assembled {
    /// Whether there is nothing to send, which is a `204`.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Records a file and moves `last_modified` forward.
    fn push(&mut self, file: ProfileFileData) {
        let at = truncate(file.updated);

        self.last_modified = Some(match self.last_modified {
            Some(current) if current >= at => current,
            _ => at,
        });

        self.files.push(file);
    }
}

/// Reads and assembles device profiles.
pub struct ProfileService<'a> {
    db: &'a Database,
    content: Arc<ContentStore>,
}

impl<'a> ProfileService<'a> {
    /// Borrows the database for the length of a call, sharing the content
    /// store that start-up installed.
    pub fn new(db: &'a Database, content: Arc<ContentStore>) -> Self {
        Self { db, content }
    }

    /// The repository underneath, for the administrative routes.
    pub fn repo(&self) -> ProfilesRepo<'a> {
        ProfilesRepo::new(self.db)
    }

    /// Every profile, with its counts, as the admin API lists them.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if a read fails.
    pub async fn list(&self) -> Result<Vec<Profile>, Error> {
        let repo = self.repo();
        let mut listed = Vec::new();

        for row in repo.list().await? {
            listed.push(self.describe(row).await?);
        }

        Ok(listed)
    }

    /// One profile with its counts.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if a read fails.
    pub async fn describe(&self, row: ProfileRow) -> Result<Profile, Error> {
        let (file_count, pref_count) = self.counts(row.id).await?;

        Ok(Profile {
            id: row.id,
            name: row.name,
            description: row.description,
            active: row.active,
            apply_on_enrollment: row.apply_on_enrollment,
            apply_on_connect: row.apply_on_connect,
            tool: row.tool,
            kind: row.kind,
            groups: row.groups,
            updated: row.updated_at,
            file_count,
            pref_count,
        })
    }

    /// The channels a principal holds, by name.
    ///
    /// Both directions count: a profile is configuration rather than traffic,
    /// so somebody who can only listen on a channel is still a member of it.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn group_names(&self, groups: &GroupSet) -> Result<Vec<GroupName>, Error> {
        let index = self.db.groups().index().await?;
        let mut names = groups.names(&index, Direction::Out);

        for name in groups.names(&index, Direction::In) {
            if !names.contains(&name) {
                names.push(name);
            }
        }

        Ok(names)
    }

    /// The package a device is handed straight after enrolling.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if a read fails or a file cannot
    /// be opened.
    pub async fn enrollment(
        &self,
        host: &str,
        held: &[GroupName],
        user: &UserSettings,
        defaults: bool,
    ) -> Result<Assembled, Error> {
        let mut assembled = Assembled::default();

        if defaults {
            let document = render(&[enrollment_defaults(host, Some(user))]);
            assembled.push(ProfileFileData::new(
                ENROLLMENT_PREF,
                document.into_bytes(),
                Utc::now(),
            ));
        }

        self.collect(Delivery::Enrollment, None, held, &mut assembled)
            .await?;
        self.enrollment_packages(&mut assembled).await?;

        Ok(assembled)
    }

    /// The package a device is handed on connect.
    ///
    /// # Errors
    ///
    /// As [`enrollment`](Self::enrollment).
    pub async fn connection(
        &self,
        held: &[GroupName],
        sync_secago: i64,
    ) -> Result<Assembled, Error> {
        let mut assembled = Assembled::default();
        self.collect(Delivery::Connect, window(sync_secago), held, &mut assembled)
            .await?;

        Ok(assembled)
    }

    /// The package a device is handed for one tool.
    ///
    /// # Errors
    ///
    /// As [`enrollment`](Self::enrollment).
    pub async fn tool(
        &self,
        tool: &str,
        held: &[GroupName],
        sync_secago: i64,
    ) -> Result<Assembled, Error> {
        let mut assembled = Assembled::default();
        self.collect(
            Delivery::Tool(tool.to_string()),
            window(sync_secago),
            held,
            &mut assembled,
        )
        .await?;

        Ok(assembled)
    }

    /// Whether any profile matches a tool at all, which decides `404` from
    /// `304` on the `/file` endpoints.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn tool_exists(&self, tool: &str, held: &[GroupName]) -> Result<bool, Error> {
        let rows = self
            .repo()
            .for_delivery(Delivery::Tool(tool.to_string()), None)
            .await?;

        Ok(rows.iter().any(|row| row.visible_to(held)))
    }

    /// The stored files of a tool's profiles whose path matches one of
    /// `wanted`, either exactly or as a directory prefix.
    ///
    /// # Errors
    ///
    /// As [`enrollment`](Self::enrollment).
    pub async fn tool_files(
        &self,
        tool: &str,
        held: &[GroupName],
        wanted: &[String],
        sync_secago: i64,
    ) -> Result<Assembled, Error> {
        let repo = self.repo();
        let mut assembled = Assembled::default();

        for row in repo
            .for_delivery(Delivery::Tool(tool.to_string()), window(sync_secago))
            .await?
        {
            if !row.visible_to(held) {
                continue;
            }

            for file in repo.files(row.id).await? {
                if wanted.iter().any(|path| matches_path(&file.path, path)) {
                    assembled.push(self.read(&file).await?);
                }
            }
        }

        Ok(assembled)
    }

    /// The zip an administrator previews, which is what a device would receive.
    ///
    /// # Errors
    ///
    /// As [`enrollment`](Self::enrollment).
    pub async fn assemble_one(&self, row: &ProfileRow) -> Result<Assembled, Error> {
        let mut assembled = Assembled::default();
        self.add(row, &mut assembled).await?;

        Ok(assembled)
    }

    /// The bytes of one stored file.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error when the content store cannot be
    /// read.
    pub async fn read(&self, file: &ProfileFileRow) -> Result<ProfileFileData, Error> {
        let mut handle = self.content.open(&file.hash).await?;
        let mut data = Vec::with_capacity(file.size as usize);

        handle.read_to_end(&mut data).await.or_system_err(&[
            "The file may have been removed from the content store; re-upload it.",
        ])?;

        Ok(ProfileFileData::new(
            file.path.clone(),
            data,
            file.updated_at,
        ))
    }

    /// How many files and preferences a profile has, for the listing.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn counts(&self, id: rustak_api::ProfileId) -> Result<(u32, u32), Error> {
        self.db
            .read(move |c| {
                c.query_one(
                    "SELECT (SELECT COUNT(*) FROM profile_files WHERE profile_id = ?1), \
                            (SELECT COUNT(*) FROM profile_prefs WHERE profile_id = ?1)",
                    [id.get()],
                    |row| Ok((row.get::<_, i64>(0)? as u32, row.get::<_, i64>(1)? as u32)),
                )
            })
            .await
    }

    /// Adds every profile a delivery offers this caller.
    async fn collect(
        &self,
        delivery: Delivery,
        since: Option<DateTime<Utc>>,
        held: &[GroupName],
        into: &mut Assembled,
    ) -> Result<(), Error> {
        for row in self.repo().for_delivery(delivery, since).await? {
            if row.visible_to(held) {
                self.add(&row, into).await?;
            }
        }

        Ok(())
    }

    /// Adds one profile's rendered preferences and its stored files.
    async fn add(&self, row: &ProfileRow, into: &mut Assembled) -> Result<(), Error> {
        let repo = self.repo();
        let entries: Vec<PrefEntry> = repo.prefs(row.id).await?;

        if !entries.is_empty() {
            let mut group = PrefGroup::new(APP_PREFS);
            group.entries = entries;

            into.push(ProfileFileData::new(
                pref_filename(row),
                render(&[group]).into_bytes(),
                row.updated_at,
            ));
        }

        for file in repo.files(row.id).await? {
            into.push(self.read(&file).await?);
        }

        Ok(())
    }

    /// The data packages an administrator flagged to install at enrolment.
    async fn enrollment_packages(&self, into: &mut Assembled) -> Result<(), Error> {
        let rows: Vec<(String, String, DateTime<Utc>)> = self
            .db
            .read(move |c| {
                c.prepare(
                    "SELECT name, hash, submission_time FROM resources \
                     WHERE install_on_enrollment = 1 AND deleted_at IS NULL \
                     ORDER BY name ASC",
                )?
                .query_map([], |row| {
                    Ok((row.get(0)?, row.get(1)?, crate::db::row::ts(row, 2)?))
                })?
                .collect()
            })
            .await?;

        for (name, hash, at) in rows {
            let mut handle = self.content.open(&hash).await?;
            let mut data = Vec::new();
            handle
                .read_to_end(&mut data)
                .await
                .or_system_err(&["The package is missing from the content store."])?;

            into.push(ProfileFileData::new(name, data, at));
        }

        Ok(())
    }
}

/// The host name a device should scope its host-keyed preferences by.
///
/// The configured public host first — it is the only value that is right for a
/// server behind NAT, because the name we report is passed on to a client's
/// peers. Then the `Host` header with its port stripped, which is right for a
/// directly reachable server. Then the canonical domain, and finally
/// `localhost`, which is what a first start before the wizard is.
pub fn preferred_host(
    public_host: Option<&str>,
    host_header: Option<&str>,
    canonical: Option<&str>,
) -> String {
    if let Some(configured) = public_host.filter(|host| !host.is_empty()) {
        return configured.to_string();
    }

    // An IPv6 literal keeps its brackets: `[::1]` without them is not an
    // authority a client could put back into a URL.
    let from_header = host_header
        .map(|authority| match authority.find(']') {
            Some(end) => &authority[..=end],
            None => authority.split(':').next().unwrap_or(authority),
        })
        .filter(|host| !host.is_empty());

    from_header
        .or(canonical)
        .map_or_else(|| "localhost".to_string(), str::to_string)
}

/// `syncSecago` as an instant, or [`None`] for "everything".
///
/// `-1` is the documented "everything" value; a first run sends roughly the
/// current epoch in seconds, which lands before any row we hold and therefore
/// means the same thing.
fn window(sync_secago: i64) -> Option<DateTime<Utc>> {
    (sync_secago > 0).then(|| Utc::now() - chrono::Duration::seconds(sync_secago))
}

/// Whether a stored path is the one asked for, or sits under it.
fn matches_path(stored: &str, wanted: &str) -> bool {
    let wanted = wanted.trim_start_matches('/');

    if wanted.is_empty() {
        return true;
    }

    stored == wanted || stored.starts_with(&format!("{}/", wanted.trim_end_matches('/')))
}

/// Drops the sub-second part, which `If-Modified-Since` cannot carry.
fn truncate(at: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp(at.timestamp(), 0).unwrap_or(at)
}

#[cfg(test)]
mod tests {
    use rustak_api::ProfileId;

    use super::*;

    #[test]
    fn a_relative_path_matches_the_file_and_everything_under_it() {
        assert!(matches_path("maps/source.xml", "/maps"));
        assert!(matches_path("maps/source.xml", "maps/"));
        assert!(matches_path("maps/source.xml", "maps/source.xml"));
        assert!(!matches_path("maps/source.xml", "map"));
        assert!(!matches_path("maps2/source.xml", "maps"));
        assert!(matches_path("anything", "/"), "the root asks for all of it");
    }

    #[test]
    fn the_configured_public_host_outranks_the_header_a_client_sent() {
        assert_eq!(
            preferred_host(Some("tak.example.com"), Some("nat.internal:8446"), None),
            "tak.example.com",
        );
        assert_eq!(preferred_host(None, Some("host:8446"), None), "host");
        assert_eq!(preferred_host(None, Some("[::1]:8446"), None), "[::1]");
        assert_eq!(
            preferred_host(Some(""), None, Some("rustak.test")),
            "rustak.test"
        );
        assert_eq!(preferred_host(None, None, None), "localhost");
    }

    #[test]
    fn only_a_positive_sync_window_narrows_anything() {
        assert!(window(-1).is_none(), "-1 means everything");
        assert!(window(0).is_none());
        assert!(window(60).is_some());
    }

    #[test]
    fn the_newest_change_wins_and_is_whole_seconds() {
        let mut assembled = Assembled::default();
        let early = DateTime::from_timestamp(1_700_000_000, 500_000_000).unwrap();
        let late = DateTime::from_timestamp(1_700_000_060, 0).unwrap();

        assembled.push(ProfileFileData::new("b", Vec::new(), late));
        assembled.push(ProfileFileData::new("a", Vec::new(), early));

        assert_eq!(assembled.last_modified, Some(late));
        assert_eq!(assembled.files.len(), 2);
        assert!(!assembled.is_empty());
        assert_eq!(truncate(early).timestamp_subsec_nanos(), 0);
    }

    #[test]
    fn a_profile_name_that_looks_like_a_path_cannot_escape_its_file() {
        let row = ProfileRow {
            id: ProfileId::new(1),
            name: "../../etc/passwd".to_string(),
            description: None,
            active: true,
            apply_on_enrollment: true,
            apply_on_connect: false,
            tool: None,
            kind: None,
            groups: Vec::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        assert_eq!(pref_filename(&row), "..-..-etc-passwd.pref");
    }
}
