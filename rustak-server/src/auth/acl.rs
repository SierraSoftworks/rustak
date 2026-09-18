//! Who may sign in, and who administers the installation.
//!
//! Both questions are answered by a [`filt_rs`] expression from `[auth]`
//! evaluated against the request, so an operator can write
//! `claims.groups contains "tak-admins"` without us inventing a policy
//! language. The expressions are evaluated **per request** rather than at
//! sign-in, so editing the configuration file takes effect on the next call
//! instead of when everybody's tokens expire.
//!
//! Both filters default to denying: an installation that has not said who may
//! in lets nobody in, and administrators come from the `users.is_admin` column
//! the setup wizard sets. That is the safe direction to be wrong in.

use std::borrow::Cow;

use actix_web::http::header::HeaderMap;
use filt_rs::{FilterValue, Filterable};

use crate::config::AuthConfig;

/// The prefix that addresses a request header in a filter expression.
const HEADERS: &str = "headers.";

/// The prefix that addresses an identity-provider claim.
const CLAIMS: &str = "claims.";

/// What an access-control expression can see about a request.
///
/// Deliberately small: the method, the path, where the request came from, its
/// headers, the validated claims behind it and who we decided that is. Nothing
/// here is attacker-controlled *and* trusted — `client_ip` honours
/// `[server] trust_proxy`, and `claims` are only ever present once a signature
/// has been checked.
pub struct AuthRequestFilter<'a> {
    /// The HTTP method, upper case.
    pub method: &'a str,
    /// The request path, without the query string.
    pub path: &'a str,
    /// The client address, as [`client_ip`] resolved it.
    ///
    /// [`client_ip`]: crate::web::helpers::request::client_ip
    pub client_ip: Option<String>,
    /// The request headers.
    pub headers: &'a HeaderMap,
    /// The identity provider's claims, when the sign-in came from one.
    pub claims: Option<&'a serde_json::Map<String, serde_json::Value>>,
    /// The account the request speaks for.
    pub username: &'a str,
    /// Where that account came from: `oidc`, `local`, `service`.
    pub source: &'a str,
}

impl Filterable for AuthRequestFilter<'_> {
    fn get(&self, key: &str) -> FilterValue<'_> {
        match key {
            "method" => self.method.into(),
            "path" => self.path.into(),
            "client_ip" => self.client_ip.as_deref().into(),
            "username" => self.username.into(),
            "source" => self.source.into(),
            key if key.starts_with(HEADERS) => self
                .headers
                .get(&key[HEADERS.len()..])
                .and_then(|value| value.to_str().ok())
                .into(),
            key if key.starts_with(CLAIMS) => self
                .claims
                .and_then(|claims| claims.get(&key[CLAIMS.len()..]))
                .map_or(FilterValue::Null, json_to_filter_value),
            _ => FilterValue::Null,
        }
    }
}

/// What the two filters decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AclOutcome {
    /// Whether this request may proceed at all.
    pub allowed: bool,
    /// Whether the expressions grant administrative access. The
    /// `users.is_admin` column grants it independently, so a caller ORs this
    /// with the stored flag rather than treating it as the whole answer.
    pub is_admin: bool,
}

/// Evaluates both access-control expressions against one request.
///
/// A filter that fails to evaluate — which can only happen when it compares
/// types the values do not support — counts as not matching, so a mistake in
/// `admin_acl` cannot accidentally hand out administrative access.
pub fn evaluate(auth: &AuthConfig, filter: &AuthRequestFilter<'_>) -> AclOutcome {
    AclOutcome {
        allowed: auth.user_acl().matches(filter).unwrap_or(false),
        is_admin: auth.admin_acl().matches(filter).unwrap_or(false),
    }
}

/// Bridges `serde_json` into the filter language.
///
/// Written here rather than derived because of the orphan rule, and because the
/// mapping needs deciding rather than inferring: an object has no useful
/// comparison in an expression, so it becomes [`FilterValue::Null`] alongside
/// JSON's own `null`, while an array becomes a tuple so that `contains` works
/// on a `groups` claim — which is the one shape every operator writes.
pub fn json_to_filter_value(value: &serde_json::Value) -> FilterValue<'_> {
    match value {
        serde_json::Value::Null | serde_json::Value::Object(_) => FilterValue::Null,
        serde_json::Value::Bool(value) => FilterValue::Bool(*value),
        serde_json::Value::Number(value) => value
            .as_f64()
            .map_or(FilterValue::Null, FilterValue::Number),
        serde_json::Value::String(value) => FilterValue::String(Cow::Borrowed(value)),
        serde_json::Value::Array(values) => {
            FilterValue::Tuple(values.iter().map(json_to_filter_value).collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use actix_web::http::header::{HeaderName, HeaderValue};

    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();

        for (name, value) in pairs {
            map.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }

        map
    }

    fn claims() -> serde_json::Map<String, serde_json::Value> {
        serde_json::json!({
            "email": "ada@example.com",
            "groups": ["tak-admins", "ops"],
            "level": 3,
            "nested": { "a": 1 },
        })
        .as_object()
        .unwrap()
        .clone()
    }

    fn filter<'a>(
        headers: &'a HeaderMap,
        claims: Option<&'a serde_json::Map<String, serde_json::Value>>,
    ) -> AuthRequestFilter<'a> {
        AuthRequestFilter {
            method: "GET",
            path: "/api/v1/users",
            client_ip: Some("10.0.0.7".to_string()),
            headers,
            claims,
            username: "ada",
            source: "oidc",
        }
    }

    #[test]
    fn the_request_surface_is_what_an_operator_can_write_about() {
        let headers = headers(&[("x-forwarded-host", "tak.example.com")]);
        let claims = claims();
        let filter = filter(&headers, Some(&claims));

        assert_eq!(filter.get("method"), FilterValue::String("GET".into()));
        assert_eq!(
            filter.get("path"),
            FilterValue::String("/api/v1/users".into())
        );
        assert_eq!(
            filter.get("client_ip"),
            FilterValue::String("10.0.0.7".into())
        );
        assert_eq!(filter.get("username"), FilterValue::String("ada".into()));
        assert_eq!(filter.get("source"), FilterValue::String("oidc".into()));
        assert_eq!(
            filter.get("headers.x-forwarded-host"),
            FilterValue::String("tak.example.com".into())
        );
        assert_eq!(
            filter.get("claims.email"),
            FilterValue::String("ada@example.com".into())
        );
    }

    #[test]
    fn anything_we_were_not_asked_about_is_null_rather_than_an_error() {
        // A filter naming a claim the provider did not send has to be false,
        // not a failure: providers differ, and an installation should not stop
        // letting people in because one of them dropped an optional claim.
        let headers = HeaderMap::new();
        let filter = filter(&headers, None);

        assert_eq!(filter.get("claims.email"), FilterValue::Null);
        assert_eq!(filter.get("headers.missing"), FilterValue::Null);
        assert_eq!(filter.get("whatever"), FilterValue::Null);
    }

    #[test]
    fn a_groups_claim_is_a_tuple_so_contains_works_on_it() {
        // This is the expression every operator writes, so it is the one shape
        // that has to keep working.
        let headers = HeaderMap::new();
        let claims = claims();
        let filter = filter(&headers, Some(&claims));

        assert!(
            filt_rs::Filter::new(r#"claims.groups contains "tak-admins""#)
                .unwrap()
                .matches(&filter)
                .unwrap()
        );
        assert!(
            !filt_rs::Filter::new(r#"claims.groups contains "nobody""#)
                .unwrap()
                .matches(&filter)
                .unwrap()
        );
    }

    #[test]
    fn a_nested_claim_has_no_comparison_so_it_reads_as_absent() {
        let headers = HeaderMap::new();
        let claims = claims();
        let filter = filter(&headers, Some(&claims));

        assert_eq!(filter.get("claims.nested"), FilterValue::Null);
        assert_eq!(filter.get("claims.level"), FilterValue::Number(3.0));
    }

    #[test]
    fn an_installation_that_has_not_said_who_may_in_lets_nobody_in() {
        let headers = HeaderMap::new();
        let claims = claims();
        let outcome = evaluate(&AuthConfig::default(), &filter(&headers, Some(&claims)));

        assert_eq!(
            outcome,
            AclOutcome {
                allowed: false,
                is_admin: false
            }
        );
    }

    #[test]
    fn the_two_expressions_are_answered_separately() {
        let auth = AuthConfig {
            user_acl: Some(filt_rs::Filter::new("true").unwrap()),
            admin_acl: Some(
                filt_rs::Filter::new(r#"claims.groups contains "tak-admins""#).unwrap(),
            ),
            ..AuthConfig::default()
        };

        let headers = HeaderMap::new();
        let claims = claims();

        assert_eq!(
            evaluate(&auth, &filter(&headers, Some(&claims))),
            AclOutcome {
                allowed: true,
                is_admin: true
            }
        );

        // The same installation, somebody the provider did not put in that
        // group: still allowed in, still not an administrator.
        assert_eq!(
            evaluate(&auth, &filter(&headers, None)),
            AclOutcome {
                allowed: true,
                is_admin: false
            }
        );
    }
}
