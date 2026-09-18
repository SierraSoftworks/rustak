//! Reading an upload's parameters, and turning what arrived into a row.
//!
//! # Every parameter is optional and none of them is trusted
//!
//! `/Marti/sync/upload` is a servlet from the era before request bodies had
//! schemas: the metadata arrives as query parameters, the names are matched
//! case-insensitively, and several of them have aliases because different
//! clients learned different spellings. Two fields a client sends are
//! deliberately **ignored**: `Hash`, because we compute our own while streaming
//! and a client that miscomputed it would otherwise be able to mislabel a blob,
//! and `SubmissionUser`, which is forced to whoever authenticated.
//!
//! A parameter we do not recognise is logged and dropped rather than refused —
//! TAK answers `400`, which makes a newer client unable to upload at all.
//!
//! # Channels
//!
//! `Groups` is checked against the caller's own memberships, so an upload
//! cannot be addressed to a channel the uploader is not in (an administrator
//! may address any). With no `Groups` at all the upload inherits the caller's
//! channels, which is what makes a file visible to the people they work with
//! rather than to nobody.

use actix_multipart::{Field, Multipart};
use futures::TryStreamExt as _;
use rustak_api::{AuditCategory, AuditOutcome};

use crate::db::AuditEntry;
use crate::db::repos::{NewResource, ResourceRow};
use crate::marti::error::MartiError;
use crate::marti::extract::CiQuery;
use crate::prelude::*;

use super::metadata::Viewer;
use super::store::Ingested;

/// The multipart part names clients use: ATAK's, then the browsers'.
pub const PART_NAMES: &[&str] = &["assetfile", "resource"];

/// What a resource's `tool` is when nobody said.
pub const DEFAULT_TOOL: &str = "public";

/// The keyword that puts a package in the public data-package list.
pub const MISSION_PACKAGE: &str = "missionpackage";

/// What a data package's content type is when nobody said.
pub const PACKAGE_MIME: &str = "application/x-zip-compressed";

/// What anything else's content type is when nobody said.
pub const DEFAULT_MIME: &str = "application/octet-stream";

/// The metadata a client attached to an upload.
#[derive(Debug, Clone, Default)]
pub struct Upload {
    pub uid: Option<String>,
    pub name: Option<String>,
    pub mime_type: Option<String>,
    pub keywords: Vec<String>,
    pub permissions: Vec<String>,
    pub contacts: Vec<String>,
    /// Empty means "the caller's own channels".
    pub groups: Vec<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub altitude: Option<f64>,
    pub remarks: Option<String>,
    pub tool: Option<String>,
    pub expiration: Option<i64>,
    pub creator_uid: Option<String>,
    pub mission_name: Option<String>,
    pub download_path: Option<String>,
    pub plugin_class_name: Option<String>,
}

impl Upload {
    /// Reads the parameters off a query string, in any of their spellings.
    ///
    /// # Errors
    ///
    /// [`MartiError::InvalidRequest`] for a coordinate or an expiry that is not
    /// a number.
    pub fn parse(query: &CiQuery) -> Result<Self, MartiError> {
        Ok(Self {
            uid: text(query, &["uid"]),
            name: text(query, &["name"]),
            // `MIME` is the alias TAK's own upload page sends; `mimetype` is
            // what the mission-package servlet calls the same thing.
            mime_type: text(query, &["mimetype", "mime"]),
            keywords: query.strings("keywords"),
            permissions: query.strings("permissions"),
            contacts: query.strings("contacts"),
            groups: query.strings("groups"),
            latitude: query.parsed("latitude")?,
            longitude: query.parsed("longitude")?,
            altitude: query.parsed("altitude")?,
            remarks: text(query, &["remarks"]),
            tool: text(query, &["tool"]),
            expiration: query.parsed("expiration")?,
            creator_uid: text(query, &["creatoruid"]),
            mission_name: text(query, &["missionname"]),
            download_path: text(query, &["downloadpath"]),
            plugin_class_name: text(query, &["pluginclassname"]),
        })
    }

    /// The channels this upload lands in, or a refusal.
    ///
    /// # Errors
    ///
    /// [`MartiError::Forbidden`] when a channel was named that the caller is
    /// not a member of, which is the one way an upload could be addressed
    /// somewhere its author cannot see.
    pub fn groups_for(&self, viewer: &Viewer) -> Result<Vec<String>, MartiError> {
        if self.groups.is_empty() {
            return Ok(viewer.held_groups.clone());
        }

        if !viewer.is_admin
            && let Some(refused) = self
                .groups
                .iter()
                .find(|group| !viewer.held_groups.iter().any(|held| held == *group))
        {
            return Err(MartiError::Forbidden(format!(
                "you are not a member of the channel {refused}"
            )));
        }

        Ok(self.groups.clone())
    }

    /// Builds the row from what was said and what actually arrived.
    ///
    /// `filename` is the multipart part's own name, which becomes the download
    /// path and stands in for `Name` when the client sent none.
    pub fn into_resource(
        self,
        stored: &Ingested,
        submitter: Option<(&str, UserId)>,
        filename: Option<String>,
        groups: Vec<String>,
    ) -> NewResource {
        let name = self
            .name
            .clone()
            .or_else(|| filename.clone())
            .unwrap_or_else(|| stored.hash.clone());
        let keywords = self.keywords.clone();

        NewResource {
            hash: stored.hash.clone(),
            uid: self.uid.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            name,
            filename: filename.clone(),
            mime_type: self.mime_type.unwrap_or_else(|| DEFAULT_MIME.to_string()),
            size: i64::try_from(stored.size).unwrap_or(i64::MAX),
            tool: self.tool.unwrap_or_else(|| DEFAULT_TOOL.to_string()),
            creator_uid: self.creator_uid,
            submitter_id: submitter.map(|(_, id)| id),
            submitter: submitter.map(|(name, _)| name.to_string()),
            expiration: self.expiration.filter(|at| *at >= 0),
            is_mission_package: keywords
                .iter()
                .any(|keyword| keyword.eq_ignore_ascii_case(MISSION_PACKAGE)),
            groups,
            mission_name: self.mission_name,
            latitude: self.latitude,
            longitude: self.longitude,
            altitude: self.altitude,
            remarks: self.remarks,
            permissions: join(&self.permissions),
            contacts: join(&self.contacts),
            download_path: self.download_path.or(filename),
            plugin_class_name: self.plugin_class_name,
            keywords,
        }
    }
}

/// Finds the part a client put the file in, draining anything before it.
///
/// ATAK calls it `assetfile` and browser upload forms call it `resource`;
/// anything else in the same request is somebody's extra form field and is
/// consumed so that the parser can reach the next boundary.
///
/// # Errors
///
/// [`MartiError::InvalidRequest`] when the body is not a well formed multipart
/// document.
pub async fn take_part(multipart: &mut Multipart) -> Result<Option<Field>, MartiError> {
    while let Some(mut field) = multipart.try_next().await.map_err(malformed)? {
        if field
            .name()
            .is_some_and(|name| PART_NAMES.contains(&name.trim()))
        {
            return Ok(Some(field));
        }

        debug!(
            part = field.name(),
            "Skipping an unexpected multipart part."
        );

        while field.try_next().await.map_err(malformed)?.is_some() {}
    }

    Ok(None)
}

/// The filename a part declared, if it declared one.
pub fn part_filename(field: &Field) -> Option<String> {
    field
        .content_disposition()
        .and_then(|disposition| disposition.get_filename())
        .map(str::to_string)
}

/// The content type a part declared, if it declared one.
pub fn part_content_type(field: &Field) -> Option<String> {
    field.content_type().map(ToString::to_string)
}

/// Records that a resource arrived, went or was changed.
///
/// Never the file's contents, and never its channels' membership: the entry
/// names the hash, which is enough to find the row and nothing on its own.
pub async fn audit(
    services: &impl Services,
    action: &'static str,
    actor: Option<&str>,
    resource: &ResourceRow,
) {
    let mut entry = AuditEntry::new(AuditCategory::Package, action, AuditOutcome::Success)
        .subject(&resource.hash)
        .message(format!("The file '{}' {action}.", resource.name))
        .detail(serde_json::json!({
            "uid": resource.uid,
            "name": resource.name,
            "size": resource.size,
            "tool": resource.tool,
        }));

    if let Some(actor) = actor {
        entry = entry.actor(actor);
    }

    if let Err(err) = services.audit().record(entry).await {
        warn!(error = %err, "Could not record a file change in the audit log.");
    }
}

/// The first of several spellings a client may have used.
fn text(query: &CiQuery, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| query.get(key))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Stores a repeated parameter in the one column it has.
fn join(values: &[String]) -> Option<String> {
    (!values.is_empty()).then(|| values.join(","))
}

/// What a body we could not parse as multipart is refused with.
fn malformed(err: actix_multipart::MultipartError) -> MartiError {
    debug!(error = %err, "Refused a multipart upload we could not read.");

    MartiError::InvalidRequest(
        "Data package upload must use multipart/form-data POST; \
         the part should be named 'assetfile'"
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored() -> Ingested {
        Ingested {
            hash: "aa".to_string(),
            size: 12,
        }
    }

    fn viewer(groups: &[&str]) -> Viewer {
        Viewer {
            username: Some("grace".to_string()),
            is_admin: false,
            out_groups: groups.iter().map(|g| (*g).to_string()).collect(),
            held_groups: groups.iter().map(|g| (*g).to_string()).collect(),
        }
    }

    #[test]
    fn the_aliases_clients_learned_all_reach_the_same_field() {
        for raw in [
            "MIMEType=text/plain",
            "mime=text/plain",
            "mimetype=text/plain",
        ] {
            let upload = Upload::parse(&CiQuery::parse(raw)).expect("a query");

            assert_eq!(upload.mime_type.as_deref(), Some("text/plain"), "{raw}");
        }
    }

    #[test]
    fn a_repeated_parameter_is_read_in_either_spelling() {
        let comma = Upload::parse(&CiQuery::parse("keywords=a,b")).unwrap();
        let repeated = Upload::parse(&CiQuery::parse("Keywords=a&keywords=b")).unwrap();

        assert_eq!(comma.keywords, vec!["a", "b"]);
        assert_eq!(repeated.keywords, vec!["a", "b"]);
    }

    #[test]
    fn a_coordinate_that_is_not_a_number_is_refused() {
        assert!(Upload::parse(&CiQuery::parse("latitude=north")).is_err());
        assert!(Upload::parse(&CiQuery::parse("latitude=1.5")).is_ok());
    }

    #[test]
    fn an_upload_with_no_channels_inherits_the_callers() {
        let upload = Upload::parse(&CiQuery::parse("")).unwrap();

        assert_eq!(
            upload.groups_for(&viewer(&["Blue", "Red"])).unwrap(),
            vec!["Blue", "Red"],
        );
    }

    #[test]
    fn a_channel_the_caller_is_not_in_is_refused() {
        let upload = Upload::parse(&CiQuery::parse("Groups=Blue,Green")).unwrap();

        let Err(err) = upload.groups_for(&viewer(&["Blue"])) else {
            panic!("a channel the caller is not in should be refused");
        };

        assert!(err.message().contains("Green"), "{err}");

        let admin = Viewer {
            is_admin: true,
            ..Viewer::default()
        };

        assert_eq!(
            upload.groups_for(&admin).unwrap(),
            vec!["Blue", "Green"],
            "an administrator may address any channel",
        );
    }

    #[test]
    fn the_name_falls_back_to_the_parts_filename_and_then_to_the_hash() {
        let named = Upload::parse(&CiQuery::parse("name=given.txt"))
            .unwrap()
            .into_resource(&stored(), None, Some("part.txt".to_string()), Vec::new());
        let from_part = Upload::default().into_resource(
            &stored(),
            None,
            Some("part.txt".to_string()),
            Vec::new(),
        );
        let bare = Upload::default().into_resource(&stored(), None, None, Vec::new());

        assert_eq!(named.name, "given.txt");
        assert_eq!(named.download_path.as_deref(), Some("part.txt"));
        assert_eq!(from_part.name, "part.txt");
        assert_eq!(bare.name, "aa");
    }

    #[test]
    fn a_uid_is_minted_when_none_was_sent_and_the_submitter_is_ours_to_set() {
        let resource = Upload::default().into_resource(
            &stored(),
            Some(("grace", UserId::from(3))),
            None,
            vec!["Blue".to_string()],
        );

        assert!(uuid::Uuid::parse_str(&resource.uid).is_ok());
        assert_eq!(resource.submitter.as_deref(), Some("grace"));
        assert_eq!(resource.tool, DEFAULT_TOOL);
        assert_eq!(resource.mime_type, DEFAULT_MIME);
        assert_eq!(resource.groups, vec!["Blue"]);
    }

    #[test]
    fn the_mission_package_keyword_is_what_marks_a_package() {
        let package = Upload::parse(&CiQuery::parse("keywords=MissionPackage"))
            .unwrap()
            .into_resource(&stored(), None, None, Vec::new());
        let plain = Upload::default().into_resource(&stored(), None, None, Vec::new());

        assert!(package.is_mission_package);
        assert!(!plain.is_mission_package);
    }

    #[test]
    fn a_negative_expiry_means_never_rather_than_the_past() {
        let never = Upload::parse(&CiQuery::parse("EXPIRATION=-1"))
            .unwrap()
            .into_resource(&stored(), None, None, Vec::new());

        assert_eq!(never.expiration, None);
    }
}
