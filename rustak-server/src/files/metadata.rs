//! Who may see a resource, and the modern `Resource` view of one.
//!
//! # The visibility rule
//!
//! A resource is readable when the caller administers the installation, when
//! they submitted it, or when one of the channels it was uploaded to is one
//! they may **receive** from. Receive rather than send: `OUT` is the direction
//! that means "this reaches me", and a member who may only publish into a
//! channel has no business browsing what other people put there.
//!
//! A resource with no channels at all is `__ANON__`'s, which every principal
//! holds — that is what makes a file uploaded by a client that never joined a
//! channel visible to the rest of the installation rather than to nobody.
//!
//! The rule is applied **after** the database has narrowed the rows rather than
//! as SQL: group membership is a bit vector in the principal and a JSON array
//! in the row, and joining the two in SQLite would mean either denormalising
//! the membership or a `json_each` per row for no gain at these volumes.

use rustak_api::identity::{Direction, GroupName};
use rustak_core::identity::{GroupIndex, Principal};
use rustak_core::prelude::*;

use crate::db::Database;
use crate::db::repos::ResourceRow;
use crate::marti::time;

/// What a caller is allowed to see.
#[derive(Debug, Clone, Default)]
pub struct Viewer {
    /// The caller's name, when they have one. Their own uploads are always
    /// visible to them.
    pub username: Option<String>,
    /// Administrators see everything, which is what makes the admin file
    /// manager useful.
    pub is_admin: bool,
    /// The channels the caller may receive from, which is what decides
    /// whether somebody else's upload is visible.
    pub out_groups: Vec<String>,
    /// Every channel the caller is a member of, in either direction, which is
    /// what decides where they may address an upload.
    pub held_groups: Vec<String>,
}

impl Viewer {
    /// Whether this caller may see a resource.
    pub fn can_read(&self, resource: &ResourceRow) -> bool {
        if self.is_admin {
            return true;
        }

        if let (Some(username), Some(submitter)) = (&self.username, &resource.submitter)
            && username.eq_ignore_ascii_case(submitter)
        {
            return true;
        }

        self.can_read_groups(&resource.groups)
    }

    /// Whether this caller holds one of the channels a thing was shared with.
    ///
    /// An empty list means `__ANON__`, which every principal is a member of.
    pub fn can_read_groups(&self, groups: &[String]) -> bool {
        if self.is_admin {
            return true;
        }

        if groups.is_empty() {
            return self.out_groups.iter().any(|held| held == GroupName::ANON);
        }

        groups
            .iter()
            .any(|group| self.out_groups.iter().any(|held| held == group))
    }

    /// Whether this caller may delete or re-own a resource.
    ///
    /// Narrower than reading: being able to see a channel's package does not
    /// make it yours to remove.
    pub fn can_write(&self, resource: &ResourceRow) -> bool {
        if self.is_admin {
            return true;
        }

        matches!(
            (&self.username, &resource.submitter),
            (Some(username), Some(submitter)) if username.eq_ignore_ascii_case(submitter)
        )
    }

    /// Keeps only the resources this caller may see.
    pub fn filter(&self, resources: Vec<ResourceRow>) -> Vec<ResourceRow> {
        resources
            .into_iter()
            .filter(|resource| self.can_read(resource))
            .collect()
    }
}

/// Resolves a caller's channels into the names the rows carry.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if the channel index cannot be read.
pub async fn viewer_for(
    db: &Database,
    username: Option<&str>,
    principal: Option<&Principal>,
) -> Result<Viewer, Error> {
    let Some(principal) = principal else {
        return Ok(Viewer::default());
    };

    let index: GroupIndex = db.groups().index().await?;
    let names = |direction| -> Vec<String> {
        principal
            .groups
            .names(&index, direction)
            .into_iter()
            .map(|name| name.as_str().to_string())
            .collect()
    };

    let out_groups = names(Direction::Out);
    // The union of the two directions rather than `Direction::Both`, which is
    // their intersection: a member who may only publish into a channel is
    // still a member of it for the purpose of addressing an upload.
    let mut held_groups = names(Direction::In);
    held_groups.extend(out_groups.iter().cloned());
    held_groups.sort();
    held_groups.dedup();

    Ok(Viewer {
        username: username.map(str::to_string),
        is_admin: principal.is_admin,
        out_groups,
        held_groups,
    })
}

/// The lowerCamelCase `Resource` object `/Marti/api/sync/search` emits.
///
/// Distinct from the Title-case `Metadata` of the legacy servlets in more than
/// spelling: `size` is a **number** here and a string there, and
/// `submissionTime` is the unpadded-millisecond form. Unifying the two would
/// break one client or the other.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceJson {
    /// Always present, empty rather than absent, as TAK's own defaults are.
    pub filename: String,
    pub keywords: Vec<String>,
    pub mime_type: String,
    pub name: String,
    pub submission_time: String,
    pub submitter: String,
    pub uid: String,
    pub hash: String,
    pub size: i64,
    pub creator_uid: String,
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latitude: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub longitude: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub altitude: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expiration: Option<i64>,
    pub groups: Vec<String>,
}

/// Renders a row as the modern `Resource` object.
pub fn resource_json(resource: &ResourceRow) -> ResourceJson {
    ResourceJson {
        filename: resource
            .filename
            .clone()
            .unwrap_or_else(|| resource.name.clone()),
        keywords: resource.keywords.clone(),
        mime_type: resource.mime_type.clone(),
        name: resource.name.clone(),
        submission_time: time::cot_date_unpadded(resource.submission_time),
        submitter: resource.submitter.clone().unwrap_or_default(),
        uid: resource.uid.clone(),
        hash: resource.hash.clone(),
        size: resource.size,
        creator_uid: resource.creator_uid.clone().unwrap_or_default(),
        tool: resource.tool.clone(),
        latitude: resource.latitude,
        longitude: resource.longitude,
        altitude: resource.altitude,
        expiration: resource.expiration,
        groups: resource.groups.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(submitter: &str, groups: &[&str]) -> ResourceRow {
        ResourceRow {
            id: 1,
            hash: "aa".to_string(),
            uid: "uid-1".to_string(),
            name: "package.zip".to_string(),
            filename: None,
            mime_type: "application/x-zip-compressed".to_string(),
            size: 12,
            tool: "public".to_string(),
            creator_uid: None,
            submitter_id: None,
            submitter: Some(submitter.to_string()),
            submission_time: chrono::Utc::now(),
            expiration: None,
            is_mission_package: true,
            groups: groups.iter().map(|g| (*g).to_string()).collect(),
            mission_name: None,
            latitude: None,
            longitude: None,
            altitude: None,
            remarks: None,
            permissions: None,
            contacts: None,
            download_path: None,
            plugin_class_name: None,
            install_on_enrollment: false,
            deleted_at: None,
            created_at: chrono::Utc::now(),
            keywords: vec!["missionpackage".to_string()],
        }
    }

    fn viewer(username: &str, groups: &[&str]) -> Viewer {
        Viewer {
            username: Some(username.to_string()),
            is_admin: false,
            out_groups: groups.iter().map(|g| (*g).to_string()).collect(),
            held_groups: groups.iter().map(|g| (*g).to_string()).collect(),
        }
    }

    #[test]
    fn a_channel_the_caller_receives_from_makes_a_resource_visible() {
        let blue = row("grace", &["Blue"]);

        assert!(viewer("alan", &["Blue", "Red"]).can_read(&blue));
        assert!(!viewer("alan", &["Red"]).can_read(&blue));
    }

    #[test]
    fn the_submitter_always_sees_their_own_upload() {
        let blue = row("grace", &["Blue"]);

        assert!(
            viewer("GRACE", &["Red"]).can_read(&blue),
            "a username compare is case-insensitive, as it is everywhere else",
        );
    }

    #[test]
    fn a_resource_with_no_channels_belongs_to_the_default_one() {
        let anonymous = row("grace", &[]);

        assert!(viewer("alan", &["__ANON__"]).can_read(&anonymous));
        assert!(
            !viewer("alan", &["Red"]).can_read(&anonymous),
            "a caller who is not even in the default channel sees nothing",
        );
    }

    #[test]
    fn an_administrator_sees_everything_and_an_anonymous_caller_sees_nothing() {
        let blue = row("grace", &["Blue"]);
        let admin = Viewer {
            is_admin: true,
            ..Viewer::default()
        };

        assert!(admin.can_read(&blue));
        assert!(admin.can_write(&blue));
        assert!(!Viewer::default().can_read(&blue));
    }

    #[test]
    fn writing_is_narrower_than_reading() {
        let blue = row("grace", &["Blue"]);
        let member = viewer("alan", &["Blue"]);

        assert!(member.can_read(&blue));
        assert!(
            !member.can_write(&blue),
            "seeing a channel's package does not make it yours to delete",
        );
    }

    #[test]
    fn the_resource_view_is_camel_case_with_a_numeric_size() {
        // node-tak's schema for this endpoint requires `size` to be an integer
        // and every one of these keys to be present.
        let json = serde_json::to_value(resource_json(&row("grace", &["Blue"]))).unwrap();

        assert!(json["size"].is_i64(), "{json}");
        assert_eq!(json["mimeType"], "application/x-zip-compressed");
        assert_eq!(json["uid"], "uid-1");
        assert_eq!(json["filename"], "package.zip");
        assert!(json["keywords"].is_array());
        assert!(json["submitter"].is_string());
        assert!(
            json.get("expiration").is_none(),
            "an absent value is omitted rather than null",
        );
    }

    #[tokio::test]
    async fn an_unauthenticated_caller_resolves_to_the_empty_viewer() {
        let db = Database::open_in_memory().await.unwrap();

        let viewer = viewer_for(&db, None, None).await.unwrap();

        assert!(!viewer.is_admin);
        assert!(viewer.out_groups.is_empty());
        assert!(viewer.username.is_none());
    }
}
