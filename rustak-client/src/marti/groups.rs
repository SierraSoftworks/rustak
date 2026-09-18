//! Channels, as the identity making the call holds them.
//!
//! TAK calls them groups on the wire and channels everywhere a person reads
//! them; this module keeps the wire spelling in the paths and the readable one
//! in the type, because a plugin author is the second audience and the server is
//! the first.
//!
//! A channel is held in a *direction*: `IN` is what an identity may send to and
//! `OUT` is what it receives from, and the same name appears twice when it holds
//! both. That is why [`Channel`] is not a set of names — a sidecar that publishes
//! into a channel it cannot read is an ordinary and useful configuration.

use rustak_core::prelude::*;

use super::client::MartiClient;

/// One channel, in one direction.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
pub struct Channel {
    pub name: String,

    /// `IN` or `OUT`, never both: a channel held in both directions appears
    /// twice.
    #[serde(default)]
    pub direction: String,

    /// Whether the calling device currently has it switched on.
    #[serde(default)]
    pub active: bool,

    /// The bit this channel occupies in the server's routing mask.
    #[serde(default)]
    pub bitpos: u32,

    /// `yyyy-MM-dd`, date only — the one place the API emits a bare date.
    #[serde(default)]
    pub created: String,

    #[serde(default)]
    pub description: Option<String>,
}

impl Channel {
    /// Whether this is a channel the identity receives from.
    pub fn is_outbound(&self) -> bool {
        self.direction.eq_ignore_ascii_case("OUT")
    }

    /// Whether this is a channel the identity may send to.
    pub fn is_inbound(&self) -> bool {
        self.direction.eq_ignore_ascii_case("IN")
    }
}

/// Channel calls, borrowed from a [`MartiClient`].
#[derive(Debug, Clone)]
pub struct Groups<'a> {
    client: &'a MartiClient,
}

impl<'a> Groups<'a> {
    pub(super) fn new(client: &'a MartiClient) -> Self {
        Self { client }
    }

    /// Every channel this identity holds, in both directions.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the identity is not one the
    /// server recognises, or it cannot be reached.
    pub async fn all(&self) -> Result<Vec<Channel>, Error> {
        let request = self.client.get("/Marti/api/groups/all");

        Ok(self
            .client
            .enveloped::<Vec<Channel>>(request, "list this service's channels")
            .await?
            .unwrap_or_default())
    }

    /// The names of the channels this identity receives from.
    ///
    /// The shape a plugin usually wants: "which channels will my stream carry?"
    ///
    /// # Errors
    ///
    /// As [`all`](Self::all).
    pub async fn receiving(&self) -> Result<Vec<String>, Error> {
        Ok(self
            .all()
            .await?
            .into_iter()
            .filter(Channel::is_outbound)
            .map(|channel| channel.name)
            .collect())
    }

    /// Switches this device's channel selection to exactly `active`.
    ///
    /// Answers nothing: the endpoint replies `200` with an empty `text/plain`
    /// body, and the new selection is read back with [`all`](Self::all).
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the identity does not hold one
    /// of the channels named, or the server cannot be reached.
    pub async fn set_active(&self, client_uid: &str, active: &[Channel]) -> Result<(), Error> {
        let body: Vec<serde_json::Value> = active
            .iter()
            .map(|channel| {
                serde_json::json!({
                    "name": channel.name,
                    "direction": channel.direction,
                    "active": channel.active,
                })
            })
            .collect();

        let request = self
            .client
            .request(reqwest::Method::PUT, "/Marti/api/groups/active")
            .query(&[("clientUid", client_uid)])
            .json(&body);

        self.client
            .send(request, "change this service's active channels")
            .await
            .map(drop)
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    async fn server_listing() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Marti/api/groups/all"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "version": "3",
                "type": "com.bbn.marti.remote.groups.Group",
                "data": [
                    { "name": "Blue", "direction": "IN", "active": true, "bitpos": 1,
                      "created": "2026-09-18", "type": "SYSTEM" },
                    { "name": "Blue", "direction": "OUT", "active": true, "bitpos": 1,
                      "created": "2026-09-18", "type": "SYSTEM" },
                    { "name": "Red", "direction": "IN", "active": false, "bitpos": 2,
                      "created": "2026-09-18", "type": "SYSTEM" },
                ],
                "nodeId": "rustak-test",
            })))
            .mount(&server)
            .await;

        server
    }

    #[tokio::test]
    async fn a_channel_held_in_both_directions_is_listed_twice() {
        // The shape a plugin has to understand: `IN` and `OUT` are separate
        // holdings, and a sidecar that publishes into a channel it cannot read
        // is a normal configuration rather than a mistake.
        let server = server_listing().await;
        let client = MartiClient::with_http(&server.uri(), reqwest::Client::new()).unwrap();

        let channels = client.groups().all().await.unwrap();

        assert_eq!(channels.len(), 3);
        assert!(channels[0].is_inbound());
        assert!(channels[1].is_outbound());
        assert_eq!(
            client.groups().receiving().await.unwrap(),
            vec!["Blue".to_string()],
        );
    }

    #[tokio::test]
    async fn a_type_field_we_do_not_model_does_not_break_the_listing() {
        // `type: SYSTEM` is in every one of those payloads and this client has
        // no use for it; refusing it would make a server upgrade a client bug.
        let server = server_listing().await;
        let client = MartiClient::with_http(&server.uri(), reqwest::Client::new()).unwrap();

        assert!(client.groups().all().await.is_ok());
    }

    #[tokio::test]
    async fn changing_the_selection_answers_an_empty_body_rather_than_a_channel_list() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/Marti/api/groups/active"))
            .respond_with(ResponseTemplate::new(200).set_body_string(""))
            .mount(&server)
            .await;
        let client = MartiClient::with_http(&server.uri(), reqwest::Client::new()).unwrap();

        let changed = client
            .groups()
            .set_active(
                "SERVICE-weather",
                &[Channel {
                    name: "Blue".into(),
                    direction: "OUT".into(),
                    active: true,
                    ..Channel::default()
                }],
            )
            .await;

        assert!(changed.is_ok());
    }
}
