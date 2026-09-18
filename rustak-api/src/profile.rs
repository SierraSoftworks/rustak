//! Device profiles: the preferences and files ATAK pulls at enrolment and on
//! every connection.
//!
//! A profile is a named bundle of two things — typed preference entries, which
//! become a `.pref` document, and uploaded files, which travel beside it in a
//! Mission Package. The two flags decide when a device is handed it, and the
//! channel list decides who.
//!
//! # Why a preference carries its class
//!
//! ATAK's importer reads the `class` attribute of every entry without checking
//! whether it is there, so a preference whose type we have forgotten crashes
//! the import of the whole document rather than skipping one key. The class is
//! therefore part of the value everywhere it travels: in the editor, in the
//! API, in storage and on the wire.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::identity::{GroupName, ProfileId};

/// The Java type ATAK stores a preference entry as.
///
/// The five classes `PreferenceControl` dispatches on. Anything else is
/// dropped by the client, so there is no "other" variant to store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PrefClass {
    /// `class java.lang.String`, and what almost everything is — including the
    /// booleans ATAK's own server sends, which it writes as the strings
    /// `"true"` and `"false"`.
    #[default]
    String,
    /// `class java.lang.Boolean`.
    Boolean,
    /// `class java.lang.Integer`.
    Integer,
    /// `class java.lang.Long`.
    Long,
    /// `class java.lang.Float`.
    Float,
}

impl PrefClass {
    /// Every class, in the order an editor offers them.
    pub const ALL: &'static [Self] = &[
        Self::String,
        Self::Boolean,
        Self::Integer,
        Self::Long,
        Self::Float,
    ];

    /// The short name carried in our own JSON and stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::String => "String",
            Self::Boolean => "Boolean",
            Self::Integer => "Integer",
            Self::Long => "Long",
            Self::Float => "Float",
        }
    }

    /// The literal `class` attribute ATAK matches on.
    ///
    /// The redundant leading `class ` token is not a mistake to tidy up: it is
    /// what `PreferenceControl` compares against, and a value without it is a
    /// preference the client silently drops.
    pub fn java(&self) -> &'static str {
        match self {
            Self::String => "class java.lang.String",
            Self::Boolean => "class java.lang.Boolean",
            Self::Integer => "class java.lang.Integer",
            Self::Long => "class java.lang.Long",
            Self::Float => "class java.lang.Float",
        }
    }

    /// Reads a class back from storage or from a request.
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|it| it.as_str() == value)
    }

    /// Whether `value` is one this class could hold.
    ///
    /// Checked at the edge so that a typo in the editor is a `400` rather than
    /// a device that imports a preference and then behaves oddly.
    pub fn accepts(&self, value: &str) -> bool {
        match self {
            Self::String => true,
            Self::Boolean => matches!(value, "true" | "false"),
            Self::Integer => value.parse::<i32>().is_ok(),
            Self::Long => value.parse::<i64>().is_ok(),
            Self::Float => value.parse::<f32>().is_ok_and(f32::is_finite),
        }
    }
}

/// One preference entry: a key, its Java class, and its rendered value.
///
/// The value is a string in every class, because that is what the `.pref`
/// document carries and what SharedPreferences stores it back as.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefEntry {
    pub key: String,
    #[serde(default)]
    pub class: PrefClass,
    pub value: String,
}

impl PrefEntry {
    /// A `class java.lang.String` entry, which is most of them.
    pub fn string(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            class: PrefClass::String,
            value: value.into(),
        }
    }

    /// An entry of any class.
    pub fn new(key: impl Into<String>, class: PrefClass, value: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            class,
            value: value.into(),
        }
    }
}

/// One profile, as the admin UI lists it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub id: ProfileId,
    pub name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Whether this profile is delivered at all. An inactive profile is kept
    /// and edited but never handed to a device.
    pub active: bool,

    /// Delivered by `GET /Marti/api/tls/profile/enrollment`.
    pub apply_on_enrollment: bool,

    /// Delivered by `GET /Marti/api/device/profile/connection`.
    pub apply_on_connect: bool,

    /// The tool name `GET /Marti/api/device/profile/tool/{tool}` matches on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,

    /// TAK's free-form `type`, carried through untouched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,

    /// The channels whose members receive it. Empty means everybody.
    #[serde(default)]
    pub groups: Vec<GroupName>,

    pub updated: DateTime<Utc>,

    pub file_count: u32,
    pub pref_count: u32,
}

/// A profile about to be created.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProfileCreate {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// Absent means active: a profile nobody asked to disable is on.
    #[serde(default)]
    pub active: Option<bool>,
    #[serde(default)]
    pub apply_on_enrollment: bool,
    #[serde(default)]
    pub apply_on_connect: bool,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub groups: Vec<GroupName>,
}

/// A change to a profile: every field absent means "leave it alone".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProfileUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apply_on_enrollment: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apply_on_connect: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub groups: Option<Vec<GroupName>>,
}

impl ProfileUpdate {
    /// Whether this change would do nothing, which the API refuses.
    pub fn is_empty(&self) -> bool {
        self.description.is_none()
            && self.active.is_none()
            && self.apply_on_enrollment.is_none()
            && self.apply_on_connect.is_none()
            && self.tool.is_none()
            && self.kind.is_none()
            && self.groups.is_none()
    }
}

/// One file attached to a profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileFile {
    pub id: i64,
    /// The path the file is delivered under, which is also what a
    /// `relativePath` query matches against.
    pub name: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    pub updated: DateTime<Utc>,
}

/// One entry of the curated catalogue the preference editor autocompletes from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrefCatalogEntry {
    pub key: String,
    pub class: PrefClass,
    /// What the preference does, in a sentence somebody configuring a server
    /// can act on.
    pub description: String,
    /// ATAK's own default, so the editor can show what changing it costs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_class_round_trips_through_its_stored_name() {
        for class in PrefClass::ALL {
            assert_eq!(PrefClass::parse(class.as_str()), Some(*class));
            assert!(class.java().starts_with("class java.lang."));
        }

        assert_eq!(PrefClass::parse("Double"), None);
    }

    #[test]
    fn a_class_refuses_a_value_it_could_not_hold() {
        assert!(PrefClass::Boolean.accepts("true"));
        assert!(!PrefClass::Boolean.accepts("True"));
        assert!(PrefClass::Integer.accepts("-3"));
        assert!(!PrefClass::Integer.accepts("3.5"));
        assert!(PrefClass::Long.accepts("9000000000"));
        assert!(!PrefClass::Integer.accepts("9000000000"));
        assert!(PrefClass::Float.accepts("1.5"));
        assert!(!PrefClass::Float.accepts("inf"));
        assert!(PrefClass::String.accepts("anything at all"));
    }

    #[test]
    fn a_preference_entry_round_trips() {
        let entry = PrefEntry::new("deviceProfileEnableOnConnect", PrefClass::String, "true");
        let json = serde_json::to_string(&entry).unwrap();

        assert_eq!(
            json,
            r#"{"key":"deviceProfileEnableOnConnect","class":"String","value":"true"}"#
        );
        assert_eq!(serde_json::from_str::<PrefEntry>(&json).unwrap(), entry);
    }

    #[test]
    fn a_profile_round_trips() {
        let profile = Profile {
            id: ProfileId::new(7),
            name: "Enrollment".to_string(),
            description: None,
            active: true,
            apply_on_enrollment: true,
            apply_on_connect: false,
            tool: Some("public".to_string()),
            kind: None,
            groups: vec![GroupName::parse("Blue").unwrap()],
            updated: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            file_count: 2,
            pref_count: 3,
        };

        let json = serde_json::to_string(&profile).unwrap();

        assert!(!json.contains("description"), "an absent field is omitted");
        assert_eq!(serde_json::from_str::<Profile>(&json).unwrap(), profile);
    }

    #[test]
    fn a_create_needs_nothing_but_a_name() {
        let created: ProfileCreate =
            serde_json::from_value(serde_json::json!({ "name": "Channels" })).unwrap();

        assert_eq!(created.name, "Channels");
        assert_eq!(created.active, None, "absent means active");
        assert!(!created.apply_on_enrollment);
        assert!(created.groups.is_empty(), "no channels means everybody");
    }

    #[test]
    fn an_empty_update_would_change_nothing() {
        let empty: ProfileUpdate = serde_json::from_value(serde_json::json!({})).unwrap();

        assert!(empty.is_empty());
        assert!(
            !ProfileUpdate {
                groups: Some(Vec::new()),
                ..ProfileUpdate::default()
            }
            .is_empty(),
            "clearing the channel list is a change",
        );
    }

    #[test]
    fn a_file_and_a_catalogue_entry_round_trip() {
        let file = ProfileFile {
            id: 3,
            name: "maps/source.xml".to_string(),
            size: 40,
            mime_type: Some("application/xml".to_string()),
            updated: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        };

        let json = serde_json::to_string(&file).unwrap();
        assert_eq!(serde_json::from_str::<ProfileFile>(&json).unwrap(), file);

        let entry = PrefCatalogEntry {
            key: "prefs_enable_channels".to_string(),
            class: PrefClass::String,
            description: "Shows the Channels selector.".to_string(),
            default: Some("false".to_string()),
        };

        let json = serde_json::to_string(&entry).unwrap();
        assert_eq!(
            serde_json::from_str::<PrefCatalogEntry>(&json).unwrap(),
            entry
        );
    }
}
