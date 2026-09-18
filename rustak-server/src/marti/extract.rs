//! What a Marti handler asks for in its signature.
//!
//! Every extractor here is deliberately forgiving, because the callers are
//! clients we do not control and a `400` from an extractor is a page that does
//! not load rather than a message somebody reads. TAK Server itself declares
//! most of these parameters as `String` and coerces them, which is why
//! [`LooseBool`] exists and why [`ApiVersion`] falls back rather than refusing.
//!
//! The one thing that is *not* forgiving is a parameter whose value changes
//! what a request means — a negative `secAgo`, a timestamp that does not parse,
//! a channel filter naming a channel the caller cannot see. Those are refused,
//! because guessing at them would answer a question nobody asked.

use std::collections::HashMap;
use std::future::{Ready, ready};
use std::str::FromStr;

use actix_web::{FromRequest, HttpRequest, dev::Payload};
use url::form_urlencoded;
use uuid::Uuid;

use super::error::MartiError;

/// The `API_VERSION` request header.
///
/// TAK clients use it to say which shape of a handful of payloads they can
/// read — `MissionSubscription` in particular differs between 2 and 3. Absent
/// or unparseable means 2, which is what a client that has never heard of the
/// header gets.
pub const API_VERSION_HEADER: &str = "API_VERSION";

/// The version a client that said nothing is assumed to speak.
pub const DEFAULT_API_VERSION: u32 = 2;

/// Which of the two listeners a request arrived on.
///
/// The routes are identical on both; what differs is which credentials are
/// accepted, which is [`auth_policy`](super::auth_policy)'s business.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenerRole {
    /// `:8446` — browsers, enrolment, OAuth, and CloudTAK's `webtak` URL.
    Public,
    /// `:8443` — mutually authenticated, every caller holding a client
    /// certificate we issued.
    Marti,
}

impl ListenerRole {
    /// The name used in traces and in the seam's own documentation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Marti => "marti",
        }
    }
}

/// The `API_VERSION` header, parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApiVersion(pub u32);

impl Default for ApiVersion {
    fn default() -> Self {
        Self(DEFAULT_API_VERSION)
    }
}

impl ApiVersion {
    /// Reads the header off a request, case-insensitively.
    pub fn of(request: &HttpRequest) -> Self {
        request
            .headers()
            .get(API_VERSION_HEADER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<i64>().ok())
            .and_then(|value| u32::try_from(value).ok())
            .map_or_else(Self::default, Self)
    }

    /// Whether the client can read the version-3 shapes.
    pub fn at_least(self, version: u32) -> bool {
        self.0 >= version
    }
}

impl FromRequest for ApiVersion {
    type Error = MartiError;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(request: &HttpRequest, _: &mut Payload) -> Self::Future {
        ready(Ok(Self::of(request)))
    }
}

/// How a mission was addressed.
///
/// CloudTAK sniffs an id that looks like a UUID and routes it to `/guid/`, so
/// both spellings reach us for the same mission. A `{name}` that parses as a
/// UUID is therefore treated as a guid rather than as a mission literally named
/// after one — which is also why creating a mission with a UUID-shaped name is
/// refused: it would be unaddressable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissionRef {
    /// Addressed by name.
    Name(String),
    /// Addressed by guid, or by a name that parses as one.
    Guid(Uuid),
}

impl MissionRef {
    /// Classifies an already-decoded path segment.
    ///
    /// # Errors
    ///
    /// [`MartiError::InvalidRequest`] for an empty segment, which would
    /// otherwise address every mission or none depending on the route.
    pub fn parse(segment: &str) -> Result<Self, MartiError> {
        let trimmed = segment.trim();

        if trimmed.is_empty() {
            return Err(MartiError::InvalidRequest(
                "a mission name is required".to_string(),
            ));
        }

        match Uuid::parse_str(trimmed) {
            Ok(guid) => Ok(Self::Guid(guid)),
            Err(_) => Ok(Self::Name(trimmed.to_string())),
        }
    }

    /// The name, when it was addressed by one.
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Name(name) => Some(name),
            Self::Guid(_) => None,
        }
    }

    /// The guid, when it was addressed by one.
    pub fn guid(&self) -> Option<Uuid> {
        match self {
            Self::Guid(guid) => Some(*guid),
            Self::Name(_) => None,
        }
    }
}

impl FromRequest for MissionRef {
    type Error = MartiError;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(request: &HttpRequest, _: &mut Payload) -> Self::Future {
        // actix has already percent-decoded the segment by the time it reaches
        // `match_info`, so a mission named `A B` arrives with its space.
        let segment = request
            .match_info()
            .get("guid")
            .or_else(|| request.match_info().get("name"));

        ready(match segment {
            Some(segment) => Self::parse(segment),
            None => Err(MartiError::InvalidRequest(
                "a mission name is required".to_string(),
            )),
        })
    }
}

/// A multi-valued query parameter.
///
/// Accepts both spellings every TAK client uses: `?group=a&group=b` and
/// `?group=a,b`. CloudTAK's `create()` comma-joins and its `update()` repeats
/// the parameter, so a server that supported only one of the two would work for
/// half of one client's calls.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommaList<T>(pub Vec<T>);

impl<T> CommaList<T> {
    /// Whether the caller supplied any value at all.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The parsed values.
    pub fn as_slice(&self) -> &[T] {
        &self.0
    }

    /// The parsed values, taken.
    pub fn into_inner(self) -> Vec<T> {
        self.0
    }
}

/// A boolean parameter that never refuses a request.
///
/// TAK Server declares these as `String` and compares against `"true"`, so any
/// other value — `1`, `yes`, an empty string, a typo — is false. A `400` here
/// would break a client for sending something a real server accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LooseBool(pub bool);

impl LooseBool {
    /// The value.
    pub fn get(self) -> bool {
        self.0
    }
}

impl FromStr for LooseBool {
    type Err = std::convert::Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self(value.trim().eq_ignore_ascii_case("true")))
    }
}

impl From<LooseBool> for bool {
    fn from(value: LooseBool) -> Self {
        value.0
    }
}

/// The query string, with case-insensitive parameter names.
///
/// The legacy Enterprise Sync servlets read their parameters case-insensitively
/// — `Filename`, `filename` and `FILENAME` all work against a real TAK Server,
/// and ATAK and the browser upload page do not agree on which they send. Keys
/// are lower-cased on the way in; lookups lower-case too.
#[derive(Debug, Clone, Default)]
pub struct CiQuery(HashMap<String, Vec<String>>);

impl CiQuery {
    /// Parses a raw query string.
    pub fn parse(query: &str) -> Self {
        let mut values: HashMap<String, Vec<String>> = HashMap::new();

        for (key, value) in form_urlencoded::parse(query.as_bytes()) {
            values
                .entry(key.to_lowercase())
                .or_default()
                .push(value.into_owned());
        }

        Self(values)
    }

    /// The first value given for a parameter.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.all(key).first().map(String::as_str)
    }

    /// Every value given for a parameter, in the order they arrived.
    pub fn all(&self, key: &str) -> &[String] {
        self.0
            .get(&key.to_lowercase())
            .map_or(&[][..], Vec::as_slice)
    }

    /// Whether a parameter was supplied at all, whatever its value.
    pub fn has(&self, key: &str) -> bool {
        !self.all(key).is_empty()
    }

    /// A boolean parameter, defaulting to false.
    pub fn flag(&self, key: &str) -> LooseBool {
        self.get(key)
            .and_then(|value| value.parse().ok())
            .unwrap_or_default()
    }

    /// A parameter parsed into a value.
    ///
    /// # Errors
    ///
    /// [`MartiError::InvalidRequest`] naming the parameter and what was sent.
    pub fn parsed<T: FromStr>(&self, key: &str) -> Result<Option<T>, MartiError> {
        let Some(raw) = self.get(key) else {
            return Ok(None);
        };

        raw.trim()
            .parse::<T>()
            .map(Some)
            .map_err(|_| MartiError::InvalidRequest(format!("{key}={raw}")))
    }

    /// A multi-valued parameter, in either spelling.
    ///
    /// # Errors
    ///
    /// [`MartiError::InvalidRequest`] naming the parameter and the entry that
    /// would not parse.
    pub fn list<T: FromStr>(&self, key: &str) -> Result<CommaList<T>, MartiError> {
        let mut parsed = Vec::new();

        for value in self.all(key) {
            for entry in value.split(',').map(str::trim).filter(|e| !e.is_empty()) {
                parsed.push(
                    entry
                        .parse::<T>()
                        .map_err(|_| MartiError::InvalidRequest(format!("{key}={entry}")))?,
                );
            }
        }

        Ok(CommaList(parsed))
    }

    /// A multi-valued parameter of plain strings.
    pub fn strings(&self, key: &str) -> Vec<String> {
        self.all(key)
            .iter()
            .flat_map(|value| value.split(','))
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
            .map(str::to_string)
            .collect()
    }
}

impl FromRequest for CiQuery {
    type Error = MartiError;
    type Future = Ready<Result<Self, Self::Error>>;

    fn from_request(request: &HttpRequest, _: &mut Payload) -> Self::Future {
        ready(Ok(Self::parse(request.query_string())))
    }
}

#[cfg(test)]
mod tests {
    use actix_web::test::TestRequest;

    use super::*;

    #[actix_web::test]
    async fn a_client_that_says_nothing_is_assumed_to_speak_version_two() {
        let request = TestRequest::default().to_http_request();

        assert_eq!(ApiVersion::of(&request), ApiVersion(2));
        assert!(!ApiVersion::of(&request).at_least(3));
    }

    #[actix_web::test]
    async fn the_header_is_read_whichever_way_it_is_spelled() {
        // HTTP header names are case-insensitive, and TAK clients disagree on
        // the casing of this one.
        for name in ["API_VERSION", "api_version", "Api_Version"] {
            let request = TestRequest::default()
                .insert_header((name, "3"))
                .to_http_request();

            assert_eq!(ApiVersion::of(&request), ApiVersion(3), "{name}");
        }
    }

    #[actix_web::test]
    async fn a_header_we_cannot_read_falls_back_rather_than_refusing() {
        // A client sending junk here still wants the version-2 shapes, not a
        // 400 on every call.
        for value in ["", "three", "-1", "9999999999999999999999"] {
            let request = TestRequest::default()
                .insert_header((API_VERSION_HEADER, value))
                .to_http_request();

            assert_eq!(ApiVersion::of(&request), ApiVersion(2), "{value:?}");
        }
    }

    #[test]
    fn a_uuid_shaped_name_is_treated_as_the_guid_it_looks_like() {
        // CloudTAK routes anything UUID-shaped to `/guid/`, so a mission named
        // after one would be unreachable by name anyway.
        let guid = Uuid::new_v4();

        assert_eq!(
            MissionRef::parse(&guid.to_string()).unwrap(),
            MissionRef::Guid(guid),
        );
        assert_eq!(
            MissionRef::parse("  Operation Alpha  ").unwrap(),
            MissionRef::Name("Operation Alpha".to_string()),
            "surrounding whitespace is trimmed, inner whitespace is not",
        );
    }

    #[test]
    fn a_mission_reference_reports_only_the_spelling_it_was_given() {
        let guid = Uuid::new_v4();

        assert_eq!(MissionRef::Guid(guid).guid(), Some(guid));
        assert_eq!(MissionRef::Guid(guid).name(), None);
        assert_eq!(MissionRef::Name("a".into()).name(), Some("a"));
        assert_eq!(MissionRef::Name("a".into()).guid(), None);
    }

    #[test]
    fn an_empty_mission_reference_is_refused() {
        assert!(MissionRef::parse("   ").is_err());
    }

    #[actix_web::test]
    async fn a_request_naming_no_mission_at_all_is_refused_rather_than_guessed_at() {
        // A route registered without a `{name}` or `{guid}` segment would
        // otherwise reach the handler with whatever the last mission was.
        let request = TestRequest::default().to_http_request();

        assert!(MissionRef::extract(&request).await.is_err());
    }

    #[test]
    fn a_multi_valued_parameter_accepts_both_spellings() {
        // CloudTAK's `create()` comma-joins and its `update()` repeats.
        let repeated = CiQuery::parse("group=Blue&group=Red");
        let joined = CiQuery::parse("group=Blue,Red");
        let mixed = CiQuery::parse("group=Blue,Red&group=Green");

        assert_eq!(repeated.strings("group"), ["Blue", "Red"]);
        assert_eq!(joined.strings("group"), ["Blue", "Red"]);
        assert_eq!(mixed.strings("group"), ["Blue", "Red", "Green"]);
    }

    #[test]
    fn spacing_around_a_comma_is_not_part_of_the_value() {
        let query = CiQuery::parse("group=Blue%2C%20Red%2C%2C");

        assert_eq!(query.strings("group"), ["Blue", "Red"]);
    }

    #[test]
    fn parameter_names_are_matched_without_regard_to_case() {
        // The legacy sync servlets are read case-insensitively by TAK Server,
        // and ATAK and the browser upload page disagree on the casing.
        let query = CiQuery::parse("Filename=a.zip&MIMEType=application/zip");

        assert_eq!(query.get("filename"), Some("a.zip"));
        assert_eq!(query.get("FILENAME"), Some("a.zip"));
        assert_eq!(query.get("mimetype"), Some("application/zip"));
        assert!(query.has("MIMEType"));
        assert!(!query.has("missing"));
    }

    #[test]
    fn a_boolean_parameter_is_true_only_for_the_literal_word() {
        let query = CiQuery::parse("a=true&b=TRUE&c=1&d=yes&e=&f=false");

        assert!(query.flag("a").get());
        assert!(query.flag("b").get());
        assert!(!query.flag("c").get(), "TAK Server compares against `true`");
        assert!(!query.flag("d").get());
        assert!(!query.flag("e").get());
        assert!(!query.flag("f").get());
        assert!(!query.flag("missing").get());
    }

    #[test]
    fn a_numeric_parameter_that_does_not_parse_names_itself() {
        let query = CiQuery::parse("secAgo=soon");

        let Err(err) = query.parsed::<i64>("secago") else {
            panic!("`soon` is not a number of seconds");
        };

        assert!(err.message().contains("secago=soon"), "{}", err.message());
        assert_eq!(err.status().as_u16(), 400);
    }

    #[test]
    fn a_parameter_that_was_not_sent_is_absent_rather_than_zero() {
        let query = CiQuery::parse("");

        assert_eq!(query.parsed::<i64>("secago").unwrap(), None);
        assert!(query.list::<i64>("bitpos").unwrap().is_empty());
        assert_eq!(query.get("anything"), None);
    }

    #[test]
    fn a_typed_list_parses_every_entry() {
        let query = CiQuery::parse("bitpos=1,2&bitpos=3");

        assert_eq!(
            query.list::<u32>("bitpos").unwrap().as_slice(),
            [1, 2, 3].as_slice(),
        );
        assert!(CiQuery::parse("bitpos=1,x").list::<u32>("bitpos").is_err());
    }

    #[test]
    fn each_listener_names_itself_for_the_auth_seam() {
        assert_eq!(ListenerRole::Public.as_str(), "public");
        assert_eq!(ListenerRole::Marti.as_str(), "marti");
    }

    #[test]
    fn a_loose_boolean_never_fails_to_parse() {
        assert!(LooseBool::from_str("True").unwrap().get());
        assert!(!LooseBool::from_str("¯\\_(ツ)_/¯").unwrap().get());
        assert!(bool::from(LooseBool(true)));
    }

    #[test]
    fn a_comma_list_reports_what_it_holds() {
        let list = CommaList(vec![1, 2, 3]);

        assert!(!list.is_empty());
        assert_eq!(list.as_slice(), [1, 2, 3]);
        assert_eq!(list.into_inner(), vec![1, 2, 3]);
        assert!(CommaList::<u8>::default().is_empty());
    }
}
