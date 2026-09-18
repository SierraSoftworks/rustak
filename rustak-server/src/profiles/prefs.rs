//! The `.pref` document: ATAK's SharedPreferences, as a file.
//!
//! One document holds several `<preference>` groups, each named after the
//! SharedPreferences store it is loaded into. Three names are special —
//! `cot_streams`, `cot_inputs` and `cot_outputs` are read by the connection
//! loader — and every other name is a plain keyed import.
//!
//! # The bytes matter more than the shape
//!
//! Three details are load-bearing and all three look like mistakes:
//!
//! 1. The XML declaration is **single-quoted with no `encoding`**, unlike every
//!    other XML document this server emits.
//! 2. There is **no whitespace between elements**.
//! 3. The `class` attribute carries a redundant leading `class ` token, and
//!    ATAK dereferences that attribute without a null check — a missing one
//!    does not skip an entry, it aborts the whole import.
//!
//! Escaping is the one place we deviate deliberately: the reference generator
//! does not escape keys or values, which produces a document its own parser
//! would refuse the moment a callsign contains an ampersand. We escape.

use rustak_api::{PrefClass, PrefEntry};

/// The declaration and the opening element, which never vary.
const HEADER: &str = "<?xml version='1.0' standalone='yes'?><preferences>";

/// ATAK-CIV's own SharedPreferences store, and where ordinary settings go.
pub const APP_PREFS: &str = "com.atakmap.app.civ_preferences";

/// The flavour-neutral name older packages use.
///
/// ATAK rewrites this (and `com.atakmap.civ_preferences`) to the running
/// package's own store, so it still works — but we emit [`APP_PREFS`] directly
/// rather than depending on a rewrite that a future release could drop.
pub const LEGACY_APP_PREFS: &str = "com.atakmap.app_preferences";

/// The group the connection loader reads stream definitions out of.
pub const COT_STREAMS: &str = "cot_streams";

/// The `version` attribute every group carries.
const GROUP_VERSION: u8 = 1;

/// One `<preference>` group.
///
/// Entries are a `Vec` rather than a map so that the rendered document is
/// byte-for-byte reproducible: TAK Server iterates a `HashMap` here and emits a
/// different order every run, which makes a golden test impossible and a diff
/// between two builds meaningless.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefGroup {
    pub name: String,
    pub version: u8,
    pub entries: Vec<PrefEntry>,
}

impl PrefGroup {
    /// An empty group with the standard version.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: GROUP_VERSION,
            entries: Vec::new(),
        }
    }

    /// The group ordinary settings are imported into.
    pub fn app(app_prefs: &str) -> Self {
        Self::new(app_prefs)
    }

    /// Adds a `class java.lang.String` entry, which is most of them.
    #[must_use]
    pub fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.entries.push(PrefEntry::string(key, value));
        self
    }

    /// Adds an entry of any class.
    #[must_use]
    pub fn typed(
        mut self,
        key: impl Into<String>,
        class: PrefClass,
        value: impl Into<String>,
    ) -> Self {
        self.entries.push(PrefEntry::new(key, class, value));
        self
    }

    /// Adds an entry only when there is a value for it.
    #[must_use]
    pub fn maybe(self, key: &str, value: Option<&str>) -> Self {
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            Some(value) => self.with(key, value),
            None => self,
        }
    }

    /// Whether the group would render no entries at all.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// What the enrolling person's own profile says about them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserSettings {
    pub callsign: Option<String>,
    pub team: Option<String>,
    pub role: Option<String>,
}

impl UserSettings {
    /// Whether there is anything worth sending.
    pub fn is_empty(&self) -> bool {
        self.callsign.is_none() && self.team.is_none() && self.role.is_none()
    }
}

/// Renders a `.pref` document.
///
/// Empty groups are kept: a group with no entries is harmless to ATAK and
/// dropping it would make the output depend on what happened to be configured,
/// which is exactly what a golden test is there to pin down.
pub fn render(groups: &[PrefGroup]) -> String {
    let mut out = String::from(HEADER);

    for group in groups {
        out.push_str(&format!(
            "<preference version=\"{}\" name=\"{}\">",
            group.version,
            crate::files::package::escape(&group.name),
        ));

        for entry in &group.entries {
            out.push_str(&format!(
                "<entry key=\"{}\" class=\"{}\">{}</entry>",
                crate::files::package::escape(&entry.key),
                entry.class.java(),
                escape_text(&entry.value),
            ));
        }

        out.push_str("</preference>");
    }

    out.push_str("</preferences>");
    out
}

/// The settings a freshly enrolled device needs before any of the rest of this
/// works.
///
/// `deviceProfileEnableOnConnect` is the important one: it defaults to `false`
/// on ATAK, so a device that never receives it will never call
/// `/Marti/api/device/profile/connection` and every connection profile an
/// operator configures will silently do nothing. The enrolment profile is the
/// only mechanism that turns it on.
///
/// `prefs_enable_channels_host-<host>` is a `String` holding `"true"` rather
/// than a `Boolean`, because that is what ATAK's own server sends and what the
/// Channels UI reads back.
pub fn enrollment_defaults(host: &str, user: Option<&UserSettings>) -> PrefGroup {
    let group = PrefGroup::app(APP_PREFS)
        .with("deviceProfileEnableOnConnect", "true")
        .with("displayServerConnectionWidget", "true")
        .with("prefs_enable_channels", "true")
        .with(format!("prefs_enable_channels_host-{host}"), "true");

    let Some(user) = user else {
        return group;
    };

    group
        .maybe("locationCallsign", user.callsign.as_deref())
        .maybe("locationTeam", user.team.as_deref())
        .maybe("atakRoleType", user.role.as_deref())
}

/// Escapes element text.
///
/// `"` and `'` are left alone: they are legal in character data, and escaping
/// them would make a callsign containing an apostrophe render differently here
/// than in every other document this server writes.
fn escape_text(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_is_rendered_with_no_whitespace_and_a_single_quoted_declaration() {
        let rendered = render(&[PrefGroup::app(APP_PREFS)
            .with("locationCallsign", "Alpha")
            .typed("count", PrefClass::Integer, "1")]);

        assert_eq!(
            rendered,
            concat!(
                "<?xml version='1.0' standalone='yes'?><preferences>",
                r#"<preference version="1" name="com.atakmap.app.civ_preferences">"#,
                r#"<entry key="locationCallsign" class="class java.lang.String">Alpha</entry>"#,
                r#"<entry key="count" class="class java.lang.Integer">1</entry>"#,
                "</preference></preferences>",
            ),
        );
    }

    #[test]
    fn every_entry_carries_a_class_attribute() {
        // A missing `class` does not skip one entry: ATAK dereferences the
        // attribute without checking, so the whole import fails.
        let rendered = render(&[PrefGroup::app(APP_PREFS)
            .typed("a", PrefClass::Boolean, "true")
            .typed("b", PrefClass::Long, "9000000000")
            .typed("c", PrefClass::Float, "1.5")]);

        assert_eq!(rendered.matches("class=\"class java.lang.").count(), 3);
    }

    #[test]
    fn the_order_entries_were_added_in_is_the_order_they_render_in() {
        let keys = ["z", "a", "m"];
        let group = keys.iter().fold(PrefGroup::app(APP_PREFS), |group, key| {
            group.with(*key, "1")
        });

        let rendered = render(&[group]);
        let found: Vec<usize> = keys
            .iter()
            .map(|key| rendered.find(&format!("key=\"{key}\"")).unwrap())
            .collect();

        assert!(found.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn markup_in_a_value_is_escaped() {
        let rendered = render(&[PrefGroup::app(APP_PREFS).with("locationCallsign", "A & <B>")]);

        assert!(rendered.contains(">A &amp; &lt;B&gt;</entry>"));
        assert!(!rendered.contains("<B>"));
    }

    #[test]
    fn the_enrolment_defaults_turn_on_the_preference_everything_else_depends_on() {
        let group = enrollment_defaults("tak.example.com", None);

        assert_eq!(group.entries[0].key, "deviceProfileEnableOnConnect");
        assert!(
            group.entries.iter().any(|entry| entry.key
                == "prefs_enable_channels_host-tak.example.com"
                && entry.class == PrefClass::String
                && entry.value == "true"),
            "the host-scoped channels flag is a String holding \"true\"",
        );
        assert_eq!(group.entries.len(), 4);
    }

    #[test]
    fn a_user_with_nothing_set_adds_no_entries() {
        let bare = enrollment_defaults("host", Some(&UserSettings::default()));
        assert_eq!(bare.entries.len(), 4);

        let described = enrollment_defaults(
            "host",
            Some(&UserSettings {
                callsign: Some("  Alpha  ".to_string()),
                team: Some(String::new()),
                role: Some("Team Lead".to_string()),
            }),
        );

        assert_eq!(described.entries.len(), 6, "an empty team is not an entry");
        assert_eq!(described.entries[4].value, "Alpha", "trimmed");
        assert_eq!(described.entries[5].key, "atakRoleType");
    }

    #[test]
    fn the_group_name_is_the_civ_flavoured_one() {
        // ATAK rewrites the legacy aliases, but relying on that puts our output
        // at the mercy of a rewrite table we do not control.
        assert_eq!(APP_PREFS, "com.atakmap.app.civ_preferences");
        assert_ne!(APP_PREFS, LEGACY_APP_PREFS);
        assert_eq!(COT_STREAMS, "cot_streams");
    }
}
