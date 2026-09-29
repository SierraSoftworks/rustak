//! The ATAK preferences this server knows about, described as a JSON Schema.
//!
//! A profile's preferences are free keys: ATAK has well over a thousand, its
//! plugins add their own, and none of it is published as a list. So the
//! catalogue never *limits* what a profile may carry. What it does is name the
//! keys this repository can vouch for — what each does, the Java class ATAK
//! reads it as, which values it takes, and ATAK's own default where that is
//! known — so the console can draw each one with the input it wants instead of
//! asking an operator to type a key, a class and a value from memory.
//!
//! # Every entry is a fact with a source
//!
//! Short and right beats long and guessed. Each entry below cites where its
//! key and class were read: the verified research reports under
//! `.claude/plan/research/` (07 is ATAK's own client source, 06 TAK Server's,
//! 04 OpenTAKServer's, 02 the protocol survey) or this repository's own code.
//! A key with no such source is not here — it is still configurable, as an
//! "other" preference with its class chosen by hand. Where the only source is
//! what a server *sends* rather than what ATAK *reads*, the comment says so.
//!
//! # The schema's own keywords
//!
//! Every property carries [`READS_KEYWORD`] — the Java class ATAK reads the
//! key as, which is what its value means and which input edits it —
//! [`CLASSES_KEYWORD`] — the classes it is known to be *sent* as, the first
//! being the one a new entry gets — and [`GROUP_KEYWORD`], the heading the
//! console's picker files it under. Everything else is plain JSON Schema:
//! `type`, `title`, `description`, `default`, `oneOf` of `const`s, `examples`,
//! `minimum` and `maximum`.
//!
//! # What a key means is not how it is sent
//!
//! ATAK reads `prefs_enable_channels` as a boolean, and TAK Server,
//! OpenTAKServer and this server's own enrolment profile all send it as a
//! String holding `"true"`. Both are working wire forms, so a key's value
//! schema (a boolean) and the classes it may travel as (String or Boolean) are
//! two separate facts. A String entry for a boolean-valued key is edited with a
//! toggle and keeps its String class; only a class nobody is known to send, or
//! a value that means nothing to the key, is a disagreement.

use serde_json::{Map, Value, json};

use crate::profile::PrefClass;

/// The keyword naming the Java class ATAK reads a property as.
pub const READS_KEYWORD: &str = "x-atak-reads";

/// The keyword listing the Java classes a property is known to be sent as,
/// the preferred one first.
pub const CLASSES_KEYWORD: &str = "x-atak-classes";

/// The keyword naming the heading a property is listed under.
pub const GROUP_KEYWORD: &str = "x-atak-group";

/// One preference the catalogue knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KnownPref {
    pub key: &'static str,
    /// The class ATAK reads it as: what its value means, and so which values
    /// it takes and which input edits it.
    pub reads: PrefClass,
    /// The classes it is known to be sent as. The first is the one a new
    /// entry gets: the class this server's own enrolment profile sends it as,
    /// when it sends it at all, and otherwise the class ATAK reads.
    pub sent_as: &'static [PrefClass],
    pub group: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// ATAK's own default, as the text a `.pref` entry would carry.
    pub default: Option<&'static str>,
    /// The values it takes, with what to call each, when it takes only some.
    pub choices: &'static [(&'static str, &'static str)],
    /// What a value looks like, for a free-text key.
    pub examples: &'static [&'static str],
}

/// Just the class ATAK reads, which is most keys.
const fn only(class: PrefClass) -> &'static [PrefClass] {
    match class {
        PrefClass::String => &[PrefClass::String],
        PrefClass::Boolean => &[PrefClass::Boolean],
        PrefClass::Integer => &[PrefClass::Integer],
        PrefClass::Long => &[PrefClass::Long],
        PrefClass::Float => &[PrefClass::Float],
    }
}

/// A String holding `"true"`/`"false"` first, because that is how this
/// server's enrolment profile sends the key; or a Boolean, which is what ATAK
/// reads.
const STRING_OR_BOOLEAN: &[PrefClass] = &[PrefClass::String, PrefClass::Boolean];

const fn pref(
    key: &'static str,
    reads: PrefClass,
    group: &'static str,
    title: &'static str,
    description: &'static str,
) -> KnownPref {
    KnownPref {
        key,
        reads,
        sent_as: only(reads),
        group,
        title,
        description,
        default: None,
        choices: &[],
        examples: &[],
    }
}

const CONNECTION: &str = "Server connection";
const IDENTITY: &str = "Identity";
const REPORTING: &str = "Position reporting";
const UPDATES: &str = "Updates";
const DISPLAY: &str = "Display";

/// The catalogue, in the order it is listed.
pub const KNOWN: &[KnownPref] = &[
    // Research 07 §2.4 (read as a boolean, default false, gating both
    // on-connect profile fetches); compat/profiles.md §4. Sent as a String by
    // this server's enrolment profile (`rustak-server/src/profiles/prefs.rs`,
    // `enrollment_defaults`) and as a Boolean by OpenTAKServer's (research 04).
    KnownPref {
        default: Some("false"),
        sent_as: STRING_OR_BOOLEAN,
        ..pref(
            "deviceProfileEnableOnConnect",
            PrefClass::Boolean,
            CONNECTION,
            "Fetch profiles on every connection",
            "Whether the device asks this server for its connection and tool profiles each time \
             it connects. Nothing delivered on connect reaches a device until this is on; this \
             server's enrolment profile turns it on.",
        )
    },
    // Sent as a Boolean by OpenTAKServer's enrolment profile (research 04) and
    // as a String by this server's (`profiles/prefs.rs`, `enrollment_defaults`).
    KnownPref {
        sent_as: STRING_OR_BOOLEAN,
        ..pref(
            "displayServerConnectionWidget",
            PrefClass::Boolean,
            CONNECTION,
            "Show the server connection widget",
            "Whether the map shows the indicator for this device's server connections.",
        )
    },
    // Research 07 §5.4 (read as a boolean, default false). Sent as a String
    // holding "true" by TAK Server (research 06 §10.3), OpenTAKServer
    // (research 04) and this server's enrolment profile (`profiles/prefs.rs`).
    KnownPref {
        default: Some("false"),
        sent_as: STRING_OR_BOOLEAN,
        ..pref(
            "prefs_enable_channels",
            PrefClass::Boolean,
            CONNECTION,
            "Show the Channels selector",
            "Whether the device shows the Channels overlay and its button, which is where \
             somebody chooses the channels they send and receive on. TAK Server and \
             OpenTAKServer both send it as a String holding \"true\".",
        )
    },
    // Research 06 §10.3 (TAK Server sends it as a String); 02 (ATAK imports it
    // although it never exports it).
    pref(
        "locationCallsign",
        PrefClass::String,
        IDENTITY,
        "Callsign",
        "The name other people see beside this device on their maps.",
    ),
    // Research 06 §10.3 (a String, from the directory's colour attribute); 02
    // for `__group name="Cyan"`.
    KnownPref {
        examples: &["Cyan"],
        ..pref(
            "locationTeam",
            PrefClass::String,
            IDENTITY,
            "Team colour",
            "The team colour this device reports, and which other devices draw it in: the name \
             of a colour, as it appears in every position report's group.",
        )
    },
    // Research 06 §10.3 (a String, from the directory's role attribute); 02
    // for `__group role="Team Member"`.
    KnownPref {
        examples: &["Team Member"],
        ..pref(
            "atakRoleType",
            PrefClass::String,
            IDENTITY,
            "Role",
            "The role this device reports beside its team colour.",
        )
    },
    // Research 07 §3.7: `Dynamic` (the default) or `Constant`.
    KnownPref {
        default: Some("Dynamic"),
        choices: &[("Dynamic", "Dynamic"), ("Constant", "Constant")],
        ..pref(
            "locationReportingStrategy",
            PrefClass::String,
            REPORTING,
            "Reporting strategy",
            "How the device paces the position reports it sends.",
        )
    },
    // Research 07 §3.7 (gates position reporting) and §7.2 (a false value
    // leaves a chat message's point at zero).
    pref(
        "dispatchLocationCotExternal",
        PrefClass::Boolean,
        REPORTING,
        "Send own position",
        "Whether the device sends its own position to the servers and peers it is connected \
         to.",
    ),
    // Research 07 §7.2: while it is true, a chat message's point is zero.
    pref(
        "dispatchLocationHidden",
        PrefClass::Boolean,
        REPORTING,
        "Hide own position",
        "When on, the device leaves its own position out of what it sends: a chat message \
         carries a zero point instead.",
    ),
    // Research 04: OpenTAKServer's enrolment profile sends these three, the
    // switch and the startup sync as Booleans and the URL as a String.
    pref(
        "appMgmtEnableUpdateServer",
        PrefClass::Boolean,
        UPDATES,
        "Use an update server",
        "Whether the device looks for apps and plugins on an update server.",
    ),
    KnownPref {
        examples: &["https://tak.example.com:8443/api/packages"],
        ..pref(
            "atakUpdateServerUrl",
            PrefClass::String,
            UPDATES,
            "Update server URL",
            "The package repository the device looks in, when an update server is on.",
        )
    },
    pref(
        "repoStartupSync",
        PrefClass::Boolean,
        UPDATES,
        "Check for updates at startup",
        "Whether the device syncs with its update server when it starts.",
    ),
    // `rustak-server/src/profiles/account.rs`: what an account's own symbol
    // edition is delivered as, and the two values it sends.
    KnownPref {
        choices: &[("2525C", "MIL-STD-2525C"), ("2525D", "MIL-STD-2525D")],
        ..pref(
            "symbologyProvider",
            PrefClass::String,
            DISPLAY,
            "Symbol edition",
            "Which edition of MIL-STD-2525 the device authors symbols in. An account's own \
             choice, under Account in this console, is delivered as this key.",
        )
    },
];

/// A stored value as the JSON a schema-drawn input edits, read as `class` —
/// the class ATAK reads the key as — or [`None`] when it is not a value of
/// that class.
pub fn to_json(class: PrefClass, raw: &str) -> Option<Value> {
    if !class.accepts(raw) {
        return None;
    }

    match class {
        PrefClass::String => Some(Value::String(raw.to_string())),
        PrefClass::Boolean => raw.parse::<bool>().ok().map(Value::Bool),
        PrefClass::Integer | PrefClass::Long => raw.parse::<i64>().ok().map(Value::from),
        PrefClass::Float => raw
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number),
    }
}

/// An edited value back as the text a `.pref` entry carries, for a key ATAK
/// reads as `class`, or [`None`] when it is not a value of that class. The
/// text is the same whichever class the entry is *sent* as: a boolean is
/// `"true"` or `"false"` in a Boolean entry and in a String one alike.
pub fn from_json(class: PrefClass, value: &Value) -> Option<String> {
    let text = match (class, value) {
        (PrefClass::String, Value::String(text)) => text.clone(),
        (PrefClass::Boolean, Value::Bool(on)) => on.to_string(),
        (PrefClass::Integer | PrefClass::Long, Value::Number(number)) => {
            number.as_i64()?.to_string()
        }
        (PrefClass::Float, Value::Number(number)) => number.as_f64()?.to_string(),
        _ => return None,
    };

    class.accepts(&text).then_some(text)
}

/// The class the catalogue in `schema` says ATAK reads `key` as.
pub fn reads_of(schema: &Value, key: &str) -> Option<PrefClass> {
    schema
        .get("properties")?
        .get(key)?
        .get(READS_KEYWORD)?
        .as_str()
        .and_then(PrefClass::parse)
}

/// The classes the catalogue in `schema` says `key` is known to be sent as,
/// the preferred one first. Empty for a key it does not list.
pub fn classes_of(schema: &Value, key: &str) -> Vec<PrefClass> {
    schema
        .get("properties")
        .and_then(|properties| properties.get(key))
        .and_then(|property| property.get(CLASSES_KEYWORD))
        .and_then(Value::as_array)
        .map(|classes| {
            classes
                .iter()
                .filter_map(|class| class.as_str().and_then(PrefClass::parse))
                .collect()
        })
        .unwrap_or_default()
}

/// The class a new entry for `key` is given.
pub fn class_of(schema: &Value, key: &str) -> Option<PrefClass> {
    classes_of(schema, key).first().copied()
}

/// One entry, as a property of the schema.
fn property(known: &KnownPref) -> Value {
    let mut node = Map::new();
    let kind = match known.reads {
        PrefClass::String => "string",
        PrefClass::Boolean => "boolean",
        PrefClass::Integer | PrefClass::Long => "integer",
        PrefClass::Float => "number",
    };

    node.insert("title".into(), known.title.into());
    node.insert("description".into(), known.description.into());
    node.insert("type".into(), kind.into());
    node.insert(READS_KEYWORD.into(), known.reads.as_str().into());
    let classes: Vec<Value> = known.sent_as.iter().map(|c| c.as_str().into()).collect();
    node.insert(CLASSES_KEYWORD.into(), classes.into());
    node.insert(GROUP_KEYWORD.into(), known.group.into());

    if let Some(default) = known.default.and_then(|raw| to_json(known.reads, raw)) {
        node.insert("default".into(), default);
    }
    if !known.choices.is_empty() {
        let choices = known.choices.iter();
        node.insert(
            "oneOf".into(),
            choices
                .map(|(value, title)| json!({ "const": value, "title": title }))
                .collect(),
        );
    }
    if !known.examples.is_empty() {
        node.insert("examples".into(), known.examples.to_vec().into());
    }

    let bounds = match known.reads {
        PrefClass::Integer => Some((i64::from(i32::MIN), i64::from(i32::MAX))),
        PrefClass::Long => Some((i64::MIN, i64::MAX)),
        _ => None,
    };
    if let Some((minimum, maximum)) = bounds {
        node.insert("minimum".into(), minimum.into());
        node.insert("maximum".into(), maximum.into());
    }

    Value::Object(node)
}

/// The catalogue as a JSON Schema for a profile's preferences, keyed by
/// preference. `additionalProperties` is open: a key this does not list is a
/// preference nobody catalogued, not a mistake.
#[must_use]
pub fn schema() -> Value {
    let properties: Map<String, Value> = KNOWN
        .iter()
        .map(|known| (known.key.to_string(), property(known)))
        .collect();

    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "ATAK preferences",
        "description": "The preferences this server can describe. A profile may carry any other \
            key as well; ATAK and its plugins define far more than any list holds.",
        "type": "object",
        "properties": properties,
        "additionalProperties": true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_key_is_listed_twice_and_every_default_and_choice_fits_its_class() {
        let mut keys: Vec<&str> = KNOWN.iter().map(|known| known.key).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), KNOWN.len(), "a duplicate key");

        for known in KNOWN {
            assert!(!known.title.is_empty() && !known.description.is_empty());
            if let Some(default) = known.default {
                assert!(known.reads.accepts(default), "{}'s default", known.key);
                if !known.choices.is_empty() {
                    assert!(known.choices.iter().any(|(value, _)| *value == default));
                }
            }
            for (value, _) in known.choices {
                assert!(known.reads.accepts(value), "{}'s choice {value}", known.key);
            }
            assert!(
                known.sent_as.contains(&known.reads) || known.reads == PrefClass::String,
                "{} may always be sent as what ATAK reads",
                known.key,
            );
        }
    }

    #[test]
    fn every_key_is_a_property_carrying_its_class_type_and_group() {
        let schema = schema();

        for known in KNOWN {
            let node = &schema["properties"][known.key];
            assert_eq!(reads_of(&schema, known.key), Some(known.reads));
            assert_eq!(classes_of(&schema, known.key), known.sent_as);
            assert_eq!(class_of(&schema, known.key), known.sent_as.first().copied());
            assert_eq!(node[GROUP_KEYWORD], known.group);
            assert_eq!(node["title"], known.title);
        }

        let connect = &schema["properties"]["deviceProfileEnableOnConnect"];
        assert_eq!(connect["type"], "boolean");
        assert_eq!(connect["default"], false, "typed, not the text \"false\"");
        assert_eq!(
            classes_of(&schema, "prefs_enable_channels"),
            [PrefClass::String, PrefClass::Boolean],
            "a String holding \"true\" is how every server sends it",
        );
        assert_eq!(
            class_of(&schema, "dispatchLocationHidden"),
            Some(PrefClass::Boolean)
        );

        let strategy = &schema["properties"]["locationReportingStrategy"];
        assert_eq!(
            strategy["oneOf"][1],
            json!({ "const": "Constant", "title": "Constant" })
        );
        assert_eq!(schema["additionalProperties"], true);
        assert_eq!(class_of(&schema, "somethingNobodyCatalogued"), None);
        assert!(classes_of(&schema, "somethingNobodyCatalogued").is_empty());
    }

    #[test]
    fn a_value_crosses_to_json_and_back_unchanged_in_every_class() {
        for (class, raw, json) in [
            (PrefClass::String, "Cyan", json!("Cyan")),
            (PrefClass::String, "", json!("")),
            (PrefClass::Boolean, "true", json!(true)),
            (PrefClass::Integer, "-20", json!(-20)),
            (PrefClass::Long, "9000000000", json!(9_000_000_000_i64)),
            (PrefClass::Float, "1.5", json!(1.5)),
        ] {
            assert_eq!(to_json(class, raw), Some(json.clone()), "{raw}");
            assert_eq!(from_json(class, &json).as_deref(), Some(raw), "{json}");
        }
    }

    #[test]
    fn a_value_its_class_could_not_hold_does_not_cross_either_way() {
        assert_eq!(to_json(PrefClass::Boolean, "yes"), None);
        assert_eq!(to_json(PrefClass::Integer, "9000000000"), None);
        assert_eq!(
            from_json(PrefClass::Integer, &json!(9_000_000_000_i64)),
            None
        );
        assert_eq!(from_json(PrefClass::Boolean, &json!("true")), None);
        assert_eq!(from_json(PrefClass::String, &json!(1)), None);
    }
}
