//! The mission as the rest of the server thinks of it, and what changes it.
//!
//! [`Mission`] is the stored row with its enums parsed and its defaults
//! resolved; the route layer never sees a [`MissionRow`]. The parameter structs
//! below are the other direction: what a create, an update, a copy or a listing
//! was asked for, assembled by the route file from query parameters and an
//! optional JSON body, and handed to the service as one value.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::db::repos::MissionRow;
use crate::marti::{CiQuery, MartiError};

use super::dto::MissionBody;
use super::roles::Role;

/// The longest name TAK Server accepts.
pub const MAX_NAME: usize = 1024;

/// Names the mission scope uses for its own routes.
///
/// A mission called `all` would be addressed by `/Marti/api/missions/all/...`,
/// which is the administrative listing — so the name is refused at creation
/// rather than routed around. `guid` is the GUID family's own prefix.
pub const RESERVED_NAMES: &[&str] = &["all", "logs", "invitations", "guid", "hierarchy"];

/// The punctuation a mission name may carry, beside letters, digits and spaces.
///
/// Transcribed from the character class TAK Server validates against, minus the
/// forward slash — which would make the name unaddressable in a path and which
/// CloudTAK refuses client-side too.
const ALLOWED_PUNCTUATION: &str = ".()!=@#$&^*_-+[]{}:,|\\";

/// The tool a mission belongs to when nobody said.
pub const DEFAULT_TOOL: &str = "public";

/// How many missions `/pagedmissions` returns when the caller does not say.
pub const DEFAULT_PAGE_SIZE: u32 = 10;

/// One mission, as the service and the route layer use it.
#[derive(Debug, Clone, PartialEq)]
pub struct Mission {
    pub id: i64,
    pub guid: Uuid,
    pub name: String,
    pub description: String,
    pub chat_room: Option<String>,
    pub base_layer: Option<String>,
    pub bbox: Option<String>,
    pub bounding_polygon: Vec<String>,
    pub path: Option<String>,
    pub classification: Option<String>,
    pub tool: String,
    pub keywords: Vec<String>,
    pub creator_uid: Option<String>,
    pub create_time: DateTime<Utc>,
    pub last_edited: Option<DateTime<Utc>>,
    /// The role a caller gets when nothing else names them.
    pub default_role: Role,
    pub invite_only: bool,
    /// Present means "password protected"; the hash itself never leaves here.
    pub password_hash: Option<String>,
    /// Epoch seconds, or `None` for never.
    pub expiration: Option<i64>,
    pub groups: Vec<String>,
    pub parent_id: Option<i64>,
    pub deleted_at: Option<DateTime<Utc>>,
}

impl Mission {
    /// Reads a stored row, resolving its enums and defaults.
    pub fn from_row(row: MissionRow) -> Self {
        Self {
            id: row.id,
            guid: row.guid,
            name: row.name,
            description: row.description,
            chat_room: row.chat_room,
            base_layer: row.base_layer,
            bbox: row.bbox,
            bounding_polygon: row.bounding_polygon,
            path: row.path,
            classification: row.classification,
            tool: row.tool,
            keywords: row.keywords,
            creator_uid: row.creator_uid,
            create_time: row.create_time,
            last_edited: row.last_edited,
            // An unparseable stored role means a row written around the schema;
            // the least privileged reading is the safe one.
            default_role: Role::parse(&row.default_role).unwrap_or(Role::ReadonlySubscriber),
            invite_only: row.invite_only,
            password_hash: row.password_hash,
            expiration: row.expiration,
            groups: row.groups,
            parent_id: row.parent_id,
            deleted_at: row.deleted_at,
        }
    }

    /// Whether a password has to be presented to get a token for this mission.
    pub fn is_password_protected(&self) -> bool {
        self.password_hash.is_some()
    }

    /// Whether this mission has been soft-deleted.
    pub fn is_deleted(&self) -> bool {
        self.deleted_at.is_some()
    }

    /// The channels this mission is shared with, `__ANON__` when it names none.
    pub fn effective_groups(&self) -> Vec<String> {
        if self.groups.is_empty() {
            return vec![rustak_api::identity::GroupName::ANON.to_string()];
        }

        self.groups.clone()
    }
}

/// Checks and normalises a requested mission name.
///
/// # Errors
///
/// [`MartiError::Validation`] for an empty or over-long name, a name carrying a
/// character TAK Server's own validator refuses, a reserved name, or a name
/// shaped like a UUID — the last because a client that sniffs UUIDs (CloudTAK
/// does) would route every call for it to the GUID family and never reach it.
pub fn validate_name(name: &str) -> Result<String, MartiError> {
    let trimmed = name.trim();

    if trimmed.is_empty() {
        return Err(MartiError::Validation(
            "a mission name must have a length greater than 0".to_string(),
        ));
    }

    if trimmed.chars().count() > MAX_NAME {
        return Err(MartiError::Validation(format!(
            "a mission name cannot exceed {MAX_NAME} characters"
        )));
    }

    if trimmed.contains('/') {
        return Err(MartiError::Validation(
            "a mission name cannot contain forward slashes".to_string(),
        ));
    }

    if !trimmed.chars().all(is_allowed) {
        return Err(MartiError::Validation(
            "a mission name contains an invalid character".to_string(),
        ));
    }

    if RESERVED_NAMES
        .iter()
        .any(|reserved| trimmed.eq_ignore_ascii_case(reserved))
    {
        return Err(MartiError::Validation(format!(
            "'{trimmed}' is reserved by the mission API"
        )));
    }

    if Uuid::parse_str(trimmed.trim_matches(['{', '}'])).is_ok() {
        return Err(MartiError::Validation(
            "a mission name cannot be a UUID; clients route those to the guid family".to_string(),
        ));
    }

    Ok(trimmed.to_string())
}

/// Whether one character may appear in a mission name.
fn is_allowed(character: char) -> bool {
    character.is_alphanumeric()
        || character.is_whitespace()
        || ALLOWED_PUNCTUATION.contains(character)
}

/// What a create or an update was asked for.
///
/// Every field is optional because the same struct serves both: a create fills
/// the gaps with defaults, an update leaves untouched whatever nobody named.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MissionParams {
    pub creator_uid: Option<String>,
    pub description: Option<String>,
    pub chat_room: Option<String>,
    pub base_layer: Option<String>,
    pub bbox: Option<String>,
    pub bounding_polygon: Option<Vec<String>>,
    pub path: Option<String>,
    pub classification: Option<String>,
    pub tool: Option<String>,
    pub keywords: Option<Vec<String>>,
    /// The plaintext password, hashed on the way in and never stored.
    pub password: Option<String>,
    pub default_role: Option<Role>,
    /// Epoch seconds; `-1` from a client means "never", which is stored as
    /// `None`.
    pub expiration: Option<i64>,
    pub invite_only: Option<bool>,
    pub groups: Option<Vec<String>>,
    /// Accepted and ignored: CloudTAK sets it whenever it sends a group.
    pub allow_group_change: bool,
    /// Accepted and ignored on `POST`; a duplicate name is an update here.
    pub allow_dupe: bool,
    /// The client uid the owner subscription is created for, when the caller
    /// named neither a `creatorUid` nor a device.
    pub device_uid: Option<String>,
    /// A non-JSON request body, imported as a data package after the create.
    pub package: Option<Vec<u8>>,
}

/// What a create or an update did.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// A new mission: `201`, with the owner's token and role attached.
    Created {
        mission: Mission,
        token: String,
        owner_role: Role,
    },
    /// An existing mission: `200`, with no token.
    Updated(Mission),
}

impl Outcome {
    /// The mission either way.
    pub fn mission(&self) -> &Mission {
        match self {
            Self::Created { mission, .. } | Self::Updated(mission) => mission,
        }
    }

    /// Whether this was a create.
    pub fn is_created(&self) -> bool {
        matches!(self, Self::Created { .. })
    }
}

/// What a copy was asked for.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CopyParams {
    pub creator_uid: Option<String>,
    pub copy_name: Option<String>,
    pub copy_path: Option<String>,
    pub default_role: Option<Role>,
    pub password: Option<String>,
}

/// Which keywords a request is replacing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeywordTarget {
    /// The mission's own keywords.
    Mission,
    /// A filed map item's.
    Uid(String),
    /// A filed resource's, by content hash.
    Hash(String),
}

impl MissionParams {
    /// Reads the parameters a create or an update carries in its query string.
    ///
    /// # Errors
    ///
    /// [`MartiError::Validation`] for a `defaultRole` that names no role we
    /// know, and [`MartiError::InvalidRequest`] for an `expiration` that will
    /// not parse.
    pub fn from_query(query: &CiQuery) -> Result<Self, MartiError> {
        let text = |key: &str| query.get(key).map(str::to_string);
        let default_role = match query.get("defaultRole") {
            Some(requested) => Some(Role::parse(requested).ok_or_else(|| {
                MartiError::Validation(format!("{requested} is not a mission role"))
            })?),
            None => None,
        };

        Ok(Self {
            creator_uid: text("creatorUid"),
            description: text("description"),
            chat_room: text("chatRoom"),
            base_layer: text("baseLayer"),
            bbox: text("bbox"),
            bounding_polygon: non_empty(query.strings("boundingPolygon")),
            path: text("path"),
            classification: text("classification"),
            tool: text("tool"),
            keywords: non_empty(query.strings("keyword")),
            password: text("password"),
            default_role,
            expiration: query.parsed::<i64>("expiration")?,
            invite_only: query.get("inviteOnly").map(|value| value == "true"),
            groups: non_empty(query.strings("group")),
            allow_group_change: query.flag("allowGroupChange").get(),
            allow_dupe: query.flag("allowDupe").get(),
            device_uid: None,
            package: None,
        })
    }

    /// Lets a JSON body override whatever it names.
    ///
    /// A field the body does not carry leaves the query parameter alone, which
    /// is what makes sending both at once well defined.
    pub fn apply_body(&mut self, body: MissionBody) -> Result<(), MartiError> {
        if let Some(requested) = body.default_role.as_ref().and_then(role_name) {
            self.default_role = Some(Role::parse(&requested).ok_or_else(|| {
                MartiError::Validation(format!("{requested} is not a mission role"))
            })?);
        }

        merge(&mut self.creator_uid, body.creator_uid);
        merge(&mut self.description, body.description);
        merge(&mut self.chat_room, body.chat_room);
        merge(&mut self.base_layer, body.base_layer);
        merge(&mut self.bbox, body.bbox);
        merge(&mut self.bounding_polygon, body.bounding_polygon);
        merge(&mut self.path, body.path);
        merge(&mut self.classification, body.classification);
        merge(&mut self.tool, body.tool);
        merge(&mut self.keywords, body.keywords);
        merge(&mut self.password, body.password);
        merge(&mut self.expiration, body.expiration);
        merge(&mut self.invite_only, body.invite_only);
        merge(&mut self.groups, body.groups);

        Ok(())
    }
}

/// The `defaultRole` a JSON body carried, in either spelling.
///
/// A client may send the string or the whole `{type, permissions}` object it
/// read back from us, and both mean the same thing.
fn role_name(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(name) => Some(name.clone()),
        serde_json::Value::Object(map) => map.get("type")?.as_str().map(str::to_string),
        _ => None,
    }
}

/// `None` for an empty list, so that "the caller said nothing" and "the caller
/// asked for nothing" stay distinguishable.
fn non_empty(values: Vec<String>) -> Option<Vec<String>> {
    (!values.is_empty()).then_some(values)
}

/// Overwrites a parameter when the body named it.
fn merge<T>(slot: &mut Option<T>, from_body: Option<T>) {
    if from_body.is_some() {
        *slot = from_body;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_is_trimmed_and_kept() {
        assert_eq!(validate_name("  Alpha Team  ").unwrap(), "Alpha Team");
        assert_eq!(validate_name("Op. 12 (east)").unwrap(), "Op. 12 (east)");
    }

    #[test]
    fn a_slash_a_reserved_word_and_a_uuid_are_all_refused() {
        for refused in [
            "a/b",
            "all",
            "GUID",
            "invitations",
            "7e57d004-2b97-0e7a-b45f-5387367791cd",
            "{7e57d004-2b97-0e7a-b45f-5387367791cd}",
            "",
            "   ",
        ] {
            assert!(validate_name(refused).is_err(), "{refused} was accepted");
        }
    }

    #[test]
    fn a_name_longer_than_the_limit_is_refused() {
        assert!(validate_name(&"a".repeat(MAX_NAME)).is_ok());
        assert!(validate_name(&"a".repeat(MAX_NAME + 1)).is_err());
    }

    #[test]
    fn a_mission_with_no_channels_is_anonymous() {
        let mission = Mission::from_row(MissionRow {
            id: 1,
            guid: Uuid::nil(),
            name: "Alpha".to_string(),
            description: String::new(),
            chat_room: None,
            base_layer: None,
            bbox: None,
            bounding_polygon: Vec::new(),
            path: None,
            classification: None,
            tool: DEFAULT_TOOL.to_string(),
            keywords: Vec::new(),
            creator_uid: None,
            create_time: Utc::now(),
            last_edited: None,
            default_role: "MISSION_SUBSCRIBER".to_string(),
            invite_only: false,
            password_hash: None,
            expiration: None,
            groups: Vec::new(),
            parent_id: None,
            deleted_at: None,
        });

        assert_eq!(mission.effective_groups(), vec!["__ANON__".to_string()]);
        assert!(!mission.is_password_protected());
        assert_eq!(mission.default_role, Role::Subscriber);
    }
}
