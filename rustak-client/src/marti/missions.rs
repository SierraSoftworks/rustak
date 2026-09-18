//! Data Sync missions: creating one, reading it, subscribing to it, filing
//! content into it, and reading back what changed.
//!
//! A mission is addressed by name. The GUID family (`/missions/guid/{guid}`)
//! exists on the server and is not wrapped here, because a plugin that holds a
//! guid also holds the mission it came from — and TAK's own clients address by
//! name.
//!
//! # Mission tokens
//!
//! Subscribing to a password-protected or invite-only mission hands back a
//! token, and every later call about that mission has to present it.
//! [`Missions::with_token`] is how: it sets `MissionAuthorization` on the
//! requests that follow, which is the header TAK reads a mission token from when
//! `Authorization` is already carrying the caller's own identity.

use rustak_core::prelude::*;
use serde_json::json;

use super::MISSION_AUTHORIZATION;
use super::client::MartiClient;
use super::mission_model::{
    Mission, MissionChange, MissionCreate, MissionLog, MissionSubscription,
};

/// Mission calls, borrowed from a [`MartiClient`].
#[derive(Debug, Clone)]
pub struct Missions<'a> {
    client: &'a MartiClient,
    token: Option<String>,
}

impl<'a> Missions<'a> {
    pub(super) fn new(client: &'a MartiClient) -> Self {
        Self {
            client,
            token: None,
        }
    }

    /// Presents a mission token on every call made through the result.
    #[must_use]
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Every mission this identity may see, optionally filtered by tool.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the server refuses the request
    /// or cannot be reached.
    pub async fn list(&self, tool: Option<&str>) -> Result<Vec<Mission>, Error> {
        let mut request = self.request(reqwest::Method::GET, "/Marti/api/missions");

        if let Some(tool) = tool {
            request = request.query(&[("tool", tool)]);
        }

        Ok(self
            .client
            .enveloped::<Vec<Mission>>(request, "list missions")
            .await?
            .unwrap_or_default())
    }

    /// One mission by name.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when it is not there, is not for
    /// this identity, or the server cannot be reached.
    pub async fn get(&self, name: &str) -> Result<Mission, Error> {
        let request = self.request(reqwest::Method::GET, &path(name, ""));

        self.one(request, format!("read the mission '{name}'"))
            .await
    }

    /// One mission, with its change history over the last `secago` seconds.
    ///
    /// # Errors
    ///
    /// As [`get`](Self::get).
    pub async fn get_with_changes(&self, name: &str, secago: u64) -> Result<Mission, Error> {
        let request = self
            .request(reqwest::Method::GET, &path(name, ""))
            .query(&[("changes", "true"), ("secago", &secago.to_string())]);

        self.one(request, format!("read the mission '{name}'"))
            .await
    }

    /// Creates a mission, or updates one that already exists.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the server refuses it — a name
    /// that is taken by a mission in another channel, a password policy, an
    /// identity that may not create missions.
    pub async fn create(&self, name: &str, create: &MissionCreate) -> Result<Mission, Error> {
        let request = self
            .request(reqwest::Method::PUT, &path(name, ""))
            .query(&create.query());

        self.one(request, format!("create the mission '{name}'"))
            .await
    }

    /// Deletes a mission.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when it is not there or not this
    /// identity's to delete.
    pub async fn delete(&self, name: &str, creator_uid: Option<&str>) -> Result<(), Error> {
        let mut request = self.request(reqwest::Method::DELETE, &path(name, ""));

        if let Some(uid) = creator_uid {
            request = request.query(&[("creatorUid", uid)]);
        }

        self.client
            .send(request, &format!("delete the mission '{name}'"))
            .await
            .map(drop)
    }

    /// Subscribes a device uid to a mission, answering the subscription and its
    /// token.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the mission is invite-only, the
    /// password is wrong, or it is not there.
    pub async fn subscribe(
        &self,
        name: &str,
        uid: &str,
        password: Option<&str>,
    ) -> Result<MissionSubscription, Error> {
        let mut request = self
            .request(reqwest::Method::PUT, &path(name, "/subscription"))
            .query(&[("uid", uid)]);

        if let Some(password) = password {
            request = request.query(&[("password", password)]);
        }

        self.client
            .enveloped(request, &format!("subscribe to the mission '{name}'"))
            .await?
            .ok_or_else(|| {
                human_errors::user(
                    format!("The server acknowledged no subscription to the mission '{name}'."),
                    &["The mission may be invite-only, or may have been deleted."],
                )
            })
    }

    /// Ends a subscription.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when there was no such subscription.
    pub async fn unsubscribe(&self, name: &str, uid: &str) -> Result<(), Error> {
        let request = self
            .request(reqwest::Method::DELETE, &path(name, "/subscription"))
            .query(&[("uid", uid)]);

        self.client
            .send(request, &format!("unsubscribe from the mission '{name}'"))
            .await
            .map(drop)
    }

    /// Files content into a mission by hash (a file) or uid (a CoT event).
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the identity may not write to
    /// the mission, or a hash names a file the server does not hold.
    pub async fn add_contents(
        &self,
        name: &str,
        hashes: &[String],
        uids: &[String],
        creator_uid: Option<&str>,
    ) -> Result<Mission, Error> {
        let mut request = self
            .request(reqwest::Method::PUT, &path(name, "/contents"))
            .json(&json!({ "hashes": hashes, "uids": uids }));

        if let Some(uid) = creator_uid {
            request = request.query(&[("creatorUid", uid)]);
        }

        self.one(request, format!("file content into the mission '{name}'"))
            .await
    }

    /// What changed in a mission over the last `secago` seconds.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the identity may not read the
    /// mission.
    pub async fn changes(&self, name: &str, secago: u64) -> Result<Vec<MissionChange>, Error> {
        let request = self
            .request(reqwest::Method::GET, &path(name, "/changes"))
            .query(&[("secago", secago.to_string())]);

        Ok(self
            .client
            .enveloped::<Vec<MissionChange>>(
                request,
                &format!("read the changes to the mission '{name}'"),
            )
            .await?
            .unwrap_or_default())
    }

    /// A mission's operational log.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the identity may not read it.
    pub async fn logs(&self, name: &str) -> Result<Vec<MissionLog>, Error> {
        let request = self.request(reqwest::Method::GET, &path(name, "/log"));

        Ok(self
            .client
            .enveloped::<Vec<MissionLog>>(request, &format!("read the log of the mission '{name}'"))
            .await?
            .unwrap_or_default())
    }

    /// Writes an entry into one or more missions' logs.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the entry carries an id (which
    /// makes it an update), names a mission that is not there, or the identity
    /// may not write to one of them.
    pub async fn add_log(&self, entry: &MissionLog) -> Result<Vec<MissionLog>, Error> {
        let request = self
            .request(reqwest::Method::POST, "/Marti/api/missions/logs/entries")
            .json(entry);

        Ok(self
            .client
            .enveloped::<Vec<MissionLog>>(request, "write a mission log entry")
            .await?
            .unwrap_or_default())
    }

    /// Exchanges a mission password for a token.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the password is wrong or the
    /// mission is not password-protected.
    pub async fn token(&self, name: &str, password: &str) -> Result<String, Error> {
        let request = self
            .request(reqwest::Method::GET, &path(name, "/token"))
            .query(&[("password", password)]);

        self.client
            .enveloped::<String>(request, &format!("get a token for the mission '{name}'"))
            .await?
            .ok_or_else(|| {
                human_errors::user(
                    format!("The server issued no token for the mission '{name}'."),
                    &["Check the mission's password."],
                )
            })
    }

    /// The one-mission form: every mission endpoint answers an array of one.
    async fn one(&self, request: reqwest::RequestBuilder, what: String) -> Result<Mission, Error> {
        self.client
            .enveloped::<Vec<Mission>>(request, &what)
            .await?
            .and_then(|missions| missions.into_iter().next())
            .ok_or_else(|| {
                human_errors::user(
                    format!("Could not {what}: the server answered with no mission."),
                    &["The mission may have been deleted, or may not be one this service may see."],
                )
            })
    }

    /// A request carrying the mission token, when one is held.
    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let request = self.client.request(method, path);

        match &self.token {
            Some(token) => request.header(MISSION_AUTHORIZATION, format!("Bearer {token}")),
            None => request,
        }
    }
}

/// The path of one mission, plus a suffix.
///
/// The name is percent-encoded: a mission name may contain spaces, and TAK's
/// own clients create them.
fn path(name: &str, suffix: &str) -> String {
    let encoded: String = name
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect();

    format!("/Marti/api/missions/{encoded}{suffix}")
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{header, method, path as path_matcher, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn envelope(data: serde_json::Value) -> serde_json::Value {
        json!({ "version": "3", "type": "Mission", "data": data, "nodeId": "rustak-test" })
    }

    async fn client(server: &MockServer) -> MartiClient {
        MartiClient::with_http(&server.uri(), reqwest::Client::new()).unwrap()
    }

    #[test]
    fn a_mission_name_with_a_space_in_it_is_encoded() {
        // ATAK creates these, and an unencoded space is a 400 from actix before
        // it ever reaches a handler.
        assert_eq!(
            path("OPERATION X", "/changes"),
            "/Marti/api/missions/OPERATION%20X/changes"
        );
    }

    #[tokio::test]
    async fn reading_a_mission_unwraps_the_envelope_and_the_array_of_one() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/Marti/api/missions/OPS"))
            .and(header("API_VERSION", "3"))
            .respond_with(ResponseTemplate::new(200).set_body_json(envelope(json!([{
                "name": "OPS",
                "guid": "2c1e",
                "tool": "public",
                "uids": [{ "data": "ANDROID-1", "timestamp": "2026-09-18T12:00:00.000Z" }],
            }]))))
            .mount(&server)
            .await;

        let mission = client(&server).await.missions().get("OPS").await.unwrap();

        assert_eq!(mission.name, "OPS");
        assert_eq!(mission.guid.as_deref(), Some("2c1e"));
        assert_eq!(mission.uids[0].data, "ANDROID-1");
        assert_eq!(
            mission.expiration, 0,
            "absent means the default, not an error"
        );
    }

    #[tokio::test]
    async fn creating_a_mission_sends_its_parameters_as_a_query() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path_matcher("/Marti/api/missions/OPS"))
            .and(query_param("creatorUid", "SERVICE-weather"))
            .and(query_param("tool", "public"))
            .respond_with(
                ResponseTemplate::new(201)
                    .set_body_json(envelope(json!([{ "name": "OPS", "token": "a-token" }]))),
            )
            .mount(&server)
            .await;

        let created = client(&server)
            .await
            .missions()
            .create(
                "OPS",
                &MissionCreate {
                    tool: Some("public".into()),
                    keywords: vec!["weather".into()],
                    ..MissionCreate::by("SERVICE-weather")
                },
            )
            .await
            .unwrap();

        assert_eq!(created.token.as_deref(), Some("a-token"));
    }

    #[tokio::test]
    async fn a_mission_token_travels_in_its_own_header() {
        // Not `Authorization`: that one is already carrying the service's own
        // identity, and TAK only falls back to it when nothing else did.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_matcher("/Marti/api/missions/OPS/changes"))
            .and(header("MissionAuthorization", "Bearer a-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(envelope(json!([{
                "type": "ADD_CONTENT",
                "missionName": "OPS",
                "contentUid": "ANDROID-1",
            }]))))
            .mount(&server)
            .await;

        let changes = client(&server)
            .await
            .missions()
            .with_token("a-token")
            .changes("OPS", 60)
            .await
            .unwrap();

        assert_eq!(changes[0].kind, "ADD_CONTENT");
        assert_eq!(changes[0].content_uid.as_deref(), Some("ANDROID-1"));
    }

    #[tokio::test]
    async fn an_acknowledgement_with_no_payload_is_not_a_failure() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({ "version": "3", "type": "Mission", "nodeId": "rustak-test" }),
            ))
            .mount(&server)
            .await;

        assert!(
            client(&server)
                .await
                .missions()
                .unsubscribe("OPS", "SERVICE-weather")
                .await
                .is_ok()
        );
    }
}
