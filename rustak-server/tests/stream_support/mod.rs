//! An in-process `:8089` listener, and enrolled clients to point at it.
//!
//! Everything here goes through the real thing: a real certificate authority, a
//! real enrolment, a real mutually authenticated handshake, and
//! `rustak_client::stream::testing::Eud` on the other end. There is no test-only
//! branch anywhere in `src/` and none here either — a harness that took a
//! shortcut past the handshake would be a suite that passed while enrolment was
//! broken.
//!
//! Elliptic-curve keys throughout, because an RSA authority is hundreds of
//! milliseconds of key generation and every test here wants several. The one
//! RSA key start-up insists on — the token signing key — is the build's shared
//! one ([`rustak_server::testing::keys`]), adopted as `TestServer` adopts it.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use rustak_api::identity::{Direction, GroupName, Username};
use rustak_api::{MembershipSource, UserKind};
use rustak_client::stream::testing::Eud;
use rustak_client::stream::{Endpoint, StreamConfig, TlsIdentity};
use rustak_core::runtime::Shutdown;
use rustak_server::config::{Config, KeyType};
use rustak_server::db::repos::{NewGroup, NewUser};
use rustak_server::pki::{Enrollment, IssuedVia, Pki, pem_certificate};
use rustak_server::prelude::*;
use rustak_server::stream::{LiveState, StreamRuntime, mission_hook};

/// How long a test waits for something that is coming before calling it hung.
///
/// A hung-wait, not a measurement: a passing test returns the moment the thing
/// arrives, so this costs nothing when green, and it is long enough that no
/// healthy host reaches it. It used to be five seconds, and in `feed_sidecars`
/// that was measured from a sidecar's spawn across its registration, its
/// handshake and its first tick — the next thing to fail as CI runners slowed
/// (M9-13, M10-08). Nothing may compare an elapsed time against it, and nothing
/// may treat reaching it as a pass: a wait for a connection to *end* goes
/// through [`expect_closed`], which fails on a timeout rather than counting it
/// as an error like any other.
pub const EXPECT: Duration = Duration::from_secs(60);

/// How long a test waits to be sure a message is *not* coming.
///
/// Short on purpose: every negative assertion pays it, and the positive path it
/// is racing against is a loopback socket and a hash-map lookup. It is only
/// ever a negative: a slow host can make a leak arrive after it, and so pass a
/// test it should have failed, but never fail one that should pass. Waiting
/// for something that *will* come — the protocol negotiation [`settle`] pumps,
/// most of all — takes [`EXPECT`].
pub const SETTLE: Duration = Duration::from_millis(400);

/// A running stream listener and everything needed to connect to it.
pub struct Harness {
    /// The server's own context.
    pub context: AppContext,
    /// The authority that issued every certificate in the test.
    pub pki: Arc<Pki>,
    /// The live registry, for assertions about what is connected.
    pub live: LiveState,
    /// The address the listener bound.
    pub addr: SocketAddr,
    shutdown: Shutdown,
    listener: tokio::task::JoinHandle<Result<(), Error>>,
    /// Held because the certificates and history live under it.
    pub data_dir: tempfile::TempDir,
}

/// One enrolled device: what it presents, and what it calls itself.
pub struct Identity {
    /// The account it belongs to.
    pub username: Username,
    /// The `clientUid` its certificate was enrolled for.
    pub uid: String,
    /// The sha256 of its certificate, which is what revoking it names.
    pub fingerprint: String,
    truststore: std::path::PathBuf,
    certificate: std::path::PathBuf,
    key: std::path::PathBuf,
}

impl Harness {
    /// Starts a listener on an ephemeral port.
    pub async fn start() -> Self {
        Self::start_with(|_| {}).await
    }

    /// Starts a listener with the real `<dest mission>` publisher installed.
    ///
    /// The default harness installs the M1 stub, because most suites have no
    /// missions and a stub keeps them from paying for a mission lookup on
    /// every message. A suite about `<dest mission>` needs the real one.
    pub async fn start_with_missions() -> Self {
        Self::build(|_| {}, true).await
    }

    /// Starts a listener, letting the caller adjust the configuration.
    pub async fn start_with(adjust: impl FnOnce(&mut Config)) -> Self {
        Self::build(adjust, false).await
    }

    /// The body both entry points share.
    async fn build(adjust: impl FnOnce(&mut Config), missions: bool) -> Self {
        rustak_server::pki::tls::install_crypto_provider();

        let data_dir = tempfile::tempdir().expect("a temporary data directory");
        let path = data_dir.path().join("data");

        let mut config = Config::testing(&path);
        config.server.domains = vec!["localhost".to_string()];
        config.server.base_url = Some("https://localhost:8446".to_string());
        config.pki.key_type = KeyType::EcdsaP256;
        config.pki.server_ips = vec![std::net::IpAddr::from([127, 0, 0, 1])];
        // Every test here says exactly which channels it is testing, and the
        // default channel would silently connect all of them to each other.
        config.auth.anon_group_default = false;
        adjust(&mut config);

        // The shared token signing key, stored where start-up will find it, so
        // that `build_context` loads a key rather than generating one — seconds
        // of every harness under coverage. Nothing here is about that key; the
        // first start's own generation is tested in the library.
        rustak_server::testing::keys::adopt_signing_key(&config).await;

        let context = rustak_server::build_context(config, session(), Shutdown::new())
            .await
            .expect("the server's storage");

        let config = context.config();
        let pki = Pki::load(
            context.db(),
            context.secrets(),
            &config.pki,
            &config.server.data_dir,
            &["localhost".to_string()],
            &config.pki.server_ips,
        )
        .await
        .expect("a certificate authority");

        let ingest: std::sync::Arc<dyn rustak_server::stream::MissionIngest> = match missions {
            true => rustak_server::missions::MissionPublisher::shared(context.clone()),
            false => mission_hook::no_missions(),
        };

        let runtime = StreamRuntime::bind(&context, &pki, ingest)
            .await
            .expect("the stream listener binds");

        let addr = runtime.local_addr();
        let live = runtime.live().clone();

        // Published only for the mission variant: a `t-x-m-c` is addressed to
        // the *connected* subscribers, and the service asks the context for the
        // registry to find out who those are.
        if missions {
            context
                .install_live(std::sync::Arc::new(live.clone()))
                .expect("the registry is installed once");
        }

        let shutdown = context.shutdown().clone();
        let listener = tokio::spawn(runtime.run(shutdown.clone()));

        Self {
            context,
            pki,
            live,
            addr,
            shutdown,
            listener,
            data_dir,
        }
    }

    /// Creates an account, puts it in the named channels, and enrols a device.
    pub async fn enroll(
        &self,
        username: &str,
        uid: &str,
        channels: &[(&str, Direction)],
    ) -> Identity {
        let db = self.context.db();
        let name = Username::parse(username).expect("a usable username");

        let user = match db.users().get_by_username(&name).await.unwrap() {
            Some(existing) => existing,
            None => db
                .users()
                .create(NewUser {
                    kind: UserKind::Person,
                    ..NewUser::person(name.clone())
                })
                .await
                .expect("the account under test"),
        };

        for (channel, direction) in channels {
            let group = self.channel(channel).await;

            db.members()
                .grant(user.id, group, *direction, MembershipSource::Manual)
                .await
                .expect("a channel membership");
        }

        let key = rustak_server::pki::generate_key(KeyType::EcdsaP256).expect("a client key");
        let mut params = rcgen::CertificateParams::default();
        params.distinguished_name = rcgen::DistinguishedName::new();
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, username);

        let csr = params.serialize_request(&key).expect("a signing request");

        let issued = self
            .pki
            .enroll(
                db,
                Enrollment {
                    username: &name,
                    csr_body: csr.der(),
                    client_uid: Some(uid),
                    user_id: Some(user.id),
                    device_id: None,
                    credential_id: None,
                    issued_via: IssuedVia::EnrollV2Json,
                    channels_capable: true,
                },
            )
            .await
            .expect("the authority issues");

        self.write_identity(username, uid, &issued.der, &issued.fingerprint, &key, name)
    }

    /// Creates a channel if it is not there, and answers its row id.
    async fn channel(&self, name: &str) -> rustak_api::identity::GroupId {
        let db = self.context.db();
        let name = GroupName::parse(name).expect("a usable channel name");

        if let Some(existing) = db.groups().get_by_name(&name).await.unwrap() {
            return existing.id;
        }

        db.groups()
            .create(NewGroup::manual(name))
            .await
            .expect("the channel under test")
            .id
    }

    /// Writes the three files a `TlsIdentity` is loaded from.
    fn write_identity(
        &self,
        username: &str,
        uid: &str,
        der: &rustls_pki_types::CertificateDer<'static>,
        fingerprint: &str,
        key: &rcgen::KeyPair,
        name: Username,
    ) -> Identity {
        // Keyed by **uid** as well as by account: one person with two phones is
        // an ordinary fixture, and a directory named only for the account meant
        // the second enrolment silently overwrote the first's certificate, so
        // both `Identity` handles connected as the same device.
        let dir = self
            .data_dir
            .path()
            .join("clients")
            .join(username)
            .join(uid);
        std::fs::create_dir_all(&dir).expect("a directory for the client's material");

        let truststore = dir.join("ca.pem");
        let certificate = dir.join("client.pem");
        let key_file = dir.join("client.key");

        std::fs::write(&truststore, self.pki.ca().certificate_pem()).unwrap();
        std::fs::write(&certificate, pem_certificate(der)).unwrap();
        std::fs::write(&key_file, key.serialize_pem()).unwrap();

        Identity {
            username: name,
            uid: uid.to_string(),
            fingerprint: fingerprint.to_string(),
            truststore,
            certificate,
            key: key_file,
        }
    }

    /// Connects an EUD with the given callsign.
    pub async fn eud(&self, identity: &Identity, callsign: &str) -> Eud {
        self.eud_with(identity, callsign, |config| config).await
    }

    /// Connects an EUD, adjusting the connection configuration first.
    pub async fn eud_with(
        &self,
        identity: &Identity,
        callsign: &str,
        adjust: impl FnOnce(StreamConfig) -> StreamConfig,
    ) -> Eud {
        let config = adjust(
            StreamConfig::new(
                Endpoint::tls(self.addr.ip().to_string(), self.addr.port()),
                identity.uid.clone(),
            )
            .with_tls(identity.tls())
            .with_callsign(callsign),
        );

        Eud::connect(&config, callsign)
            .await
            .unwrap_or_else(|err| panic!("{callsign} should reach the listener: {err}"))
    }

    /// Tries to connect, and hands back whatever went wrong.
    pub async fn try_eud(
        &self,
        identity: &Identity,
        callsign: &str,
    ) -> Result<Eud, rustak_client::stream::StreamError> {
        let config = StreamConfig::new(
            Endpoint::tls(self.addr.ip().to_string(), self.addr.port()),
            identity.uid.clone(),
        )
        .with_tls(identity.tls())
        .with_callsign(callsign);

        Eud::connect(&config, callsign).await
    }

    /// Waits until `count` clients are registered, or gives up after
    /// [`EXPECT`].
    ///
    /// Bounded by a deadline rather than a number of polls: two hundred polls
    /// of ten milliseconds was two seconds on a quick host and a bet on a slow
    /// one.
    pub async fn await_connected(&self, count: usize) {
        let deadline = tokio::time::Instant::now() + EXPECT;

        while tokio::time::Instant::now() < deadline {
            if self.live.connected() >= count {
                return;
            }

            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        panic!(
            "expected {count} connections, saw {}",
            self.live.connected()
        );
    }

    /// Waits until the hub has seen a callsign, or gives up after [`EXPECT`].
    pub async fn await_callsign(&self, callsign: &str) {
        let deadline = tokio::time::Instant::now() + EXPECT;

        while tokio::time::Instant::now() < deadline {
            if self
                .live
                .snapshot()
                .iter()
                .any(|endpoint| endpoint.callsign == callsign)
            {
                return;
            }

            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        panic!("{callsign} never announced itself");
    }

    /// Stops the listener and waits for it.
    pub async fn stop(self) {
        self.shutdown.cancel();

        let _ = tokio::time::timeout(Duration::from_secs(10), self.listener).await;
    }
}

impl Identity {
    /// The material this device connects with.
    pub fn tls(&self) -> TlsIdentity {
        TlsIdentity::from_pem_files(&self.truststore, &self.certificate, &self.key)
            .expect("the client's own material loads")
    }
}

/// Reads until the connection goes quiet, so that everything the client owes
/// itself — answering the protocol offer, most of all — has happened.
///
/// A `Stream` + `Sink` over one socket only makes progress while it is polled,
/// and `Eud::send` polls the write half. Nothing drives the *read* half until a
/// test asks for a message, which is why the negotiation a test wants to assert
/// on has to be pumped explicitly rather than assumed.
///
/// Pumped until the encoding is **settled** — the server offers on every
/// connection, and the client either takes protobuf or stays on XML — and only
/// then for [`SETTLE`] of quiet. It used to be the quiet alone, which made
/// [`SETTLE`] an upper bound on the negotiation: a host slow enough to answer
/// the offer after 400 ms left the mode assertion that follows reading `Xml`.
pub async fn settle(eud: &mut Eud) {
    let deadline = tokio::time::Instant::now() + EXPECT;

    while !eud.stream().negotiation().is_settled() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "{} never settled its encoding: still {:?}",
            eud.callsign(),
            eud.stream().negotiation(),
        );

        // Polled in short slices, because settling yields nothing: the offer
        // and the answer are control traffic the stream consumes, so the only
        // sign of them is the state checked above.
        match tokio::time::timeout(Duration::from_millis(20), eud.stream_mut().next()).await {
            Err(_) => {}
            Ok(None) => panic!("{}'s connection closed while settling", eud.callsign()),
            Ok(Some(Err(err))) => panic!(
                "{}'s connection failed while settling: {err}",
                eud.callsign()
            ),
            Ok(Some(Ok(event))) => panic!(
                "nothing but control traffic while settling, and {} was sent a '{}' from {}",
                eud.callsign(),
                event.r#type,
                event.uid,
            ),
        }
    }

    eud.expect_none(SETTLE)
        .await
        .expect("nothing but control traffic while settling");
}

/// Waits for the server to end `eud`'s connection, and answers how it ended.
///
/// Fails if anything arrives first, and — unlike asserting `is_err()` on an
/// [`Eud::expect`] — fails if nothing happens at all: a timeout is an error
/// too, so that assertion passed, after however long the wait was, against a
/// server that never closed the connection.
pub async fn expect_closed(eud: &mut Eud) -> rustak_client::stream::StreamError {
    use rustak_client::stream::StreamError;

    match eud.expect(|_| true, EXPECT).await {
        Ok(event) => panic!(
            "{}'s connection should have ended, and it was sent a '{}' from {}",
            eud.callsign(),
            event.r#type,
            event.uid,
        ),
        Err(err @ (StreamError::Timeout(_) | StreamError::RxTimeout)) => panic!(
            "{}'s connection should have ended, and was still open: {err}",
            eud.callsign(),
        ),
        Err(ended) => ended,
    }
}

/// A telemetry session that records into memory.
fn session() -> Arc<rustak_core::telemetry::Session> {
    Arc::new(
        rustak_core::telemetry::Session::new("rustak", "0.0.0-test")
            .with_battery(tracing_batteries::Testing),
    )
}
