//! Who is connected: the contact list, and the endpoint listing behind it.
//!
//! Two views of the same thing, and a plugin wants different ones at different
//! times. [`Contacts::all`] is what an EUD's contact list is drawn from — every
//! device this identity may see, right now. [`Contacts::endpoints`] is the
//! administrative view, which also carries devices that have *dis*connected and
//! says which.
//!
//! `/Marti/api/contacts/all` is one of the handful of Marti endpoints that
//! answers a bare array rather than the envelope, which is why this module reads
//! it differently from every other one.

use rustak_core::prelude::*;

use super::client::MartiClient;

/// One device on the contact list.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Contact {
    /// The device's own identifier, which is what CoT is addressed to.
    #[serde(default)]
    pub uid: String,

    #[serde(default)]
    pub callsign: String,

    /// The team colour it reports, e.g. `Cyan`.
    #[serde(default)]
    pub team: String,

    /// Its role within that team, e.g. `Team Member`.
    #[serde(default)]
    pub role: String,

    /// `platform:version`, from the device's `<takv>`.
    #[serde(default)]
    pub takv: String,
}

/// One device, as the administrative listing renders it.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ClientEndpoint {
    #[serde(default)]
    pub uid: String,

    #[serde(default)]
    pub callsign: String,

    /// The account the device authenticated as.
    #[serde(default)]
    pub username: String,

    #[serde(default)]
    pub team: String,

    #[serde(default)]
    pub role: String,

    /// Exactly `Connected` or `Disconnected`.
    #[serde(default)]
    pub last_status: String,

    /// Epoch milliseconds, unpadded.
    #[serde(default)]
    pub last_event_time: String,
}

impl ClientEndpoint {
    /// Whether this device is on the stream now.
    pub fn is_connected(&self) -> bool {
        self.last_status.eq_ignore_ascii_case("Connected")
    }
}

/// Contact calls, borrowed from a [`MartiClient`].
#[derive(Debug, Clone)]
pub struct Contacts<'a> {
    client: &'a MartiClient,
}

impl<'a> Contacts<'a> {
    pub(super) fn new(client: &'a MartiClient) -> Self {
        Self { client }
    }

    /// Every device this identity may see, right now.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the identity is not one the
    /// server recognises, or it cannot be reached.
    pub async fn all(&self) -> Result<Vec<Contact>, Error> {
        // One of the few endpoints with no envelope; see the module docs.
        let request = self.client.get("/Marti/api/contacts/all");

        self.client.bare(request, "list the contacts").await
    }

    /// Every device the server knows about, connected or not.
    ///
    /// `secago` bounds how far back a disconnected device is still listed;
    /// [`None`] takes the server's own window.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::User`] error when the identity may not read one
    /// of the channels asked for, or the server cannot be reached.
    pub async fn endpoints(&self, secago: Option<u64>) -> Result<Vec<ClientEndpoint>, Error> {
        let mut request = self.client.get("/Marti/api/clientEndPoints");

        if let Some(secago) = secago {
            request = request.query(&[("secAgo", secago.to_string())]);
        }

        Ok(self
            .client
            .enveloped::<Vec<ClientEndpoint>>(request, "list the client endpoints")
            .await?
            .unwrap_or_default())
    }

    /// The uids that are connected right now.
    ///
    /// # Errors
    ///
    /// As [`endpoints`](Self::endpoints).
    pub async fn connected(&self) -> Result<Vec<String>, Error> {
        Ok(self
            .endpoints(None)
            .await?
            .into_iter()
            .filter(ClientEndpoint::is_connected)
            .map(|endpoint| endpoint.uid)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    #[tokio::test]
    async fn the_contact_list_is_a_bare_array_rather_than_an_envelope() {
        // The property this module exists to absorb: `/contacts/all` is not
        // enveloped, and a client that assumed otherwise would fail on the one
        // endpoint every plugin calls first.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Marti/api/contacts/all"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "uid": "ANDROID-1", "callsign": "ADA", "team": "Cyan",
                  "role": "Team Member", "takv": "ATAK-CIV:5.1", "notes": "",
                  "filterGroups": [] },
            ])))
            .mount(&server)
            .await;
        let client = MartiClient::with_http(&server.uri(), reqwest::Client::new()).unwrap();

        let contacts = client.contacts().all().await.unwrap();

        assert_eq!(contacts.len(), 1);
        assert_eq!(contacts[0].callsign, "ADA");
        assert_eq!(contacts[0].takv, "ATAK-CIV:5.1");
    }

    #[tokio::test]
    async fn the_endpoint_listing_says_who_has_gone_as_well_as_who_is_here() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/Marti/api/clientEndPoints"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "version": "3",
                "type": "com.bbn.marti.remote.ClientEndpoint",
                "data": [
                    { "uid": "ANDROID-1", "callsign": "ADA", "username": "ada",
                      "lastStatus": "Connected", "lastEventTime": "1789646440000" },
                    { "uid": "ANDROID-2", "callsign": "BOB", "username": "bob",
                      "lastStatus": "Disconnected", "lastEventTime": "1789646000000" },
                ],
                "nodeId": "rustak-test",
            })))
            .mount(&server)
            .await;
        let client = MartiClient::with_http(&server.uri(), reqwest::Client::new()).unwrap();

        let endpoints = client.contacts().endpoints(None).await.unwrap();

        assert_eq!(endpoints.len(), 2);
        assert!(endpoints[0].is_connected());
        assert!(!endpoints[1].is_connected());
        assert_eq!(
            client.contacts().connected().await.unwrap(),
            vec!["ANDROID-1".to_string()],
        );
    }
}
