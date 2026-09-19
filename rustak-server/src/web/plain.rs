//! `[web.public] plain_bind` — the plaintext port, and the two things it does.
//!
//! An ACME `http-01` challenge is fetched over plaintext HTTP on port 80, and
//! a browser typing a host name with no scheme asks for port 80 as well. This
//! listener answers both, and nothing else:
//!
//! | Request | Answer |
//! |---|---|
//! | `GET /.well-known/acme-challenge/{token}` | the key authorization, or `404` |
//! | anything else, any method | `301` to `https://<host><path>` |
//!
//! # It is not an insecure copy of the public listener
//!
//! No API, no admin UI, no Marti, no enrolment, no cookies: nothing on this
//! port can carry a credential, because everything that could is answered with
//! a redirect before a handler sees it. That is also why
//! [`allow_insecure_http`](crate::config::PublicWebConfig::allow_insecure_http)
//! does **not** gate it — that key is about serving the real surface without
//! TLS, which this listener never does, and requiring it here would mean an
//! operator setting the dangerous key to get the safe behaviour.
//!
//! # Why the redirect does not echo the `Host` header
//!
//! A `Location` built from whatever the caller asked for is an open redirect,
//! and a cached one poisons the entry for everybody else. The host is used
//! only when it is one this installation answers to — `[server] domains`,
//! `[acme] domains` or the host of `[server] base_url` — and any other is sent
//! to the canonical name instead. An installation with no name configured at
//! all has nothing to compare against and nowhere else to send anybody, so it
//! echoes the requested host and says so once, at start-up.
//!
//! The port comes from `[server] base_url` when it names one, and otherwise
//! from the first `[web.public] listen` address — omitted when that is 443,
//! because a URL says that already.

use std::net::TcpListener;
use std::sync::Arc;

use actix_web::http::header;
use actix_web::{App, HttpRequest, HttpResponse, HttpServer, dev::Server, web};

use crate::config::Config;
use crate::prelude::*;
use crate::web::{server::cannot_bind, telemetry::TracingLogger};

/// Binds `[web.public] plain_bind` and returns the server, unstarted.
///
/// [`None`] when the key is not set, which is the default and not a failure.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when the address cannot be resolved or
/// bound — something else is on the port, or port 80 needs privileges this
/// process does not have.
#[instrument("web.plain.build", skip_all, err(Display))]
pub fn build_plain(context: AppContext) -> Result<Option<Server>, Error> {
    if context.config().web.public.plain_bind.is_none() {
        return Ok(None);
    }

    plain_server(context, PlainSocket::Configured).map(Some)
}

/// Serves the plaintext listener on a socket the caller has already bound.
///
/// For the caller that has to know the port before the server exists — a test
/// binding `:0` to be given a free one, as `build_marti_on` does for the Marti
/// listener. `plain_bind` is not consulted: a caller holding the socket has
/// already decided.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when actix will not serve on the
/// socket it was handed.
#[instrument("web.plain.build_on", skip_all, err(Display))]
pub fn build_plain_on(context: AppContext, listener: TcpListener) -> Result<Server, Error> {
    plain_server(context, PlainSocket::Bound(listener))
}

/// Where the plaintext listener's socket comes from.
enum PlainSocket {
    /// `[web.public] plain_bind`, bound here.
    Configured,
    /// One socket the caller bound already.
    Bound(TcpListener),
}

/// The plaintext server, unstarted, on the socket `socket` describes.
fn plain_server(context: AppContext, socket: PlainSocket) -> Result<Server, Error> {
    let config = context.config();
    let origin = Arc::new(Origin::from_config(&config));
    let acme = context.acme();
    let drain = config.server.listener_drain_seconds();

    if origin.canonical.is_none() {
        warn!(
            "No `[server] domains` or `[server] base_url` is set, so the plaintext listener can \
             only redirect to the host each request asks for."
        );
    }

    let built = Arc::clone(&origin);
    let mut server = HttpServer::new(move || {
        App::new()
            .wrap(TracingLogger::<AppContext>::new())
            .app_data(web::Data::from(Arc::clone(&built)))
            .configure(crate::pki::acme::http01_routes(Arc::clone(&acme)))
            .default_service(web::to(redirect))
    })
    .disable_signals()
    .shutdown_timeout(drain);

    match socket {
        PlainSocket::Configured => {
            // Checked by `build_plain`; a caller that reached here another way
            // has asked for a listener with no address, which is nothing.
            let address = config
                .web
                .public
                .plain_bind
                .as_ref()
                .ok_or_else(no_address)?;

            for socket in address.to_socket_addrs()? {
                server = server
                    .bind(socket)
                    .map_err(|err| cannot_bind(socket, &err))?;

                announce(socket, &origin);
            }
        }
        PlainSocket::Bound(listener) => {
            let address = listener.local_addr().map_err(cannot_serve)?;

            server = server.listen(listener).map_err(cannot_serve)?;

            announce(address, &origin);
        }
    }

    Ok(server.run())
}

/// Says what this port is for, because an operator reading the log will
/// otherwise wonder why rustak bound a plaintext socket.
fn announce(socket: std::net::SocketAddr, origin: &Origin) {
    info!(
        address = %socket,
        redirect_to = %origin,
        "The plaintext listener is bound; it serves the ACME http-01 path and redirects everything else."
    );
}

/// What to say when the address went missing between the two checks.
fn no_address() -> Error {
    human_errors::system(
        "The plaintext listener was asked to bind `[web.public] plain_bind`, which is not set.",
        &["This is unexpected; please report it with the surrounding log entries."],
    )
}

/// Where this listener sends everything that is not a challenge.
#[derive(Debug)]
struct Origin {
    /// The name to redirect to when the request does not name one we serve.
    canonical: Option<String>,
    /// Every name this installation answers to, lower-cased.
    served: Vec<String>,
    /// The port to put in the URL, when it is not 443.
    port: Option<u16>,
}

impl Origin {
    /// Works out the canonical origin from the configuration, once.
    fn from_config(config: &Config) -> Self {
        let base = config
            .server
            .base_url
            .as_deref()
            .and_then(|url| url::Url::parse(url).ok());

        let mut served: Vec<String> = Vec::new();

        for name in base
            .as_ref()
            .and_then(|url| url.host_str())
            .into_iter()
            .chain(config.server.domains.iter().map(String::as_str))
            .chain(config.acme.domains.iter().map(String::as_str))
        {
            let name = name.trim().trim_end_matches('.').to_ascii_lowercase();

            if !name.is_empty() && !served.contains(&name) {
                served.push(name);
            }
        }

        Self {
            canonical: served.first().cloned(),
            served,
            // A configured `base_url` is what the outside world sees, so it
            // decides on its own: no port in it means the default, not "look
            // at what we bound". Otherwise the first public address, which is
            // where a browser following this redirect has to land.
            port: match base {
                Some(url) => url.port(),
                None => config.web.public.https_port(),
            },
        }
    }

    /// The authority to redirect a request for `host` to.
    ///
    /// [`None`] when the request named no host and nothing is configured,
    /// which is the one case there is no answer for.
    fn authority(&self, host: Option<&str>) -> Option<String> {
        let asked = host.map(hostname).filter(|host| is_hostname(host));

        let host = match asked {
            Some(asked) if self.served.iter().any(|name| name == &asked) => asked,
            // Not one of ours: the canonical name, never the caller's.
            _ => self.canonical.clone().or(asked)?,
        };

        Some(match self.port {
            Some(port) => format!("{host}:{port}"),
            None => host,
        })
    }
}

impl std::fmt::Display for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.authority(None) {
            Some(authority) => write!(f, "https://{authority}"),
            None => f.write_str("https://<the host each request asks for>"),
        }
    }
}

/// The host part of a `Host` header, lower-cased and without its port.
fn hostname(value: &str) -> String {
    let value = value.trim();

    // An IPv6 literal is bracketed, and the brackets are kept: they are part
    // of the authority in a URL.
    let host = match value.starts_with('[') {
        true => value
            .split_once(']')
            .map_or(value, |(inside, _)| &value[..inside.len() + 1]),
        false => value.split(':').next().unwrap_or(value),
    };

    host.to_ascii_lowercase()
}

/// Whether this is a host we would put in a `Location` header at all.
///
/// Deliberately narrow: letters, digits, `-`, `.`, and the brackets and colons
/// of an IPv6 literal. Anything else is a header somebody crafted rather than a
/// name a client resolved.
fn is_hostname(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '[' | ']' | ':'))
}

/// Answers everything that is not the challenge path with a `301`.
///
/// Every method, not only `GET`: a redirect is harmless whatever was asked
/// for, and the alternative is a second rule for a listener whose whole
/// purpose is one.
async fn redirect(request: HttpRequest, origin: web::Data<Origin>) -> HttpResponse {
    let asked = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok());

    let Some(authority) = origin.authority(asked) else {
        return HttpResponse::BadRequest()
            .content_type("text/plain; charset=utf-8")
            .body("This port only redirects to HTTPS, and the request named no host to redirect to.\n");
    };

    let path = request
        .uri()
        .path_and_query()
        .map_or_else(|| request.path().to_string(), ToString::to_string);

    HttpResponse::MovedPermanently()
        .insert_header((header::LOCATION, format!("https://{authority}{path}")))
        .finish()
}

/// What to say when a socket that is already bound cannot be served on.
fn cannot_serve(err: std::io::Error) -> Error {
    human_errors::system(
        format!("We could not serve the plaintext listener on the socket it was handed: {err}"),
        &["This is unexpected; please report it with the surrounding log entries."],
    )
}

#[cfg(test)]
mod tests {
    use actix_web::http::StatusCode;
    use actix_web::test;

    use super::*;

    use crate::config::TlsMode;

    /// The configuration an `http-01` deployment behind a proxy writes.
    fn config() -> Config {
        let mut config = Config::default();
        config.server.domains = vec!["tak.example.com".to_string()];
        config.web.public.listen = vec![rustak_core::config::ListenAddr::new("", 8446)];
        config.web.public.plain_bind = Some(rustak_core::config::ListenAddr::new("", 8080));

        config
    }

    #[actix_web::test]
    async fn a_request_for_a_name_we_serve_keeps_it() {
        let origin = Origin::from_config(&config());

        assert_eq!(
            origin.authority(Some("tak.example.com")).as_deref(),
            Some("tak.example.com:8446"),
        );
        // The port in the request is the plaintext one; the answer's is not.
        assert_eq!(
            origin.authority(Some("TAK.example.com:80")).as_deref(),
            Some("tak.example.com:8446"),
        );
    }

    #[actix_web::test]
    async fn a_foreign_host_is_sent_to_the_canonical_name_rather_than_back_to_itself() {
        // An open redirect, and — cached — everybody else's redirect too.
        let origin = Origin::from_config(&config());

        assert_eq!(
            origin.authority(Some("evil.example.net")).as_deref(),
            Some("tak.example.com:8446"),
        );
        assert_eq!(
            origin
                .authority(Some("tak.example.com.evil.net"))
                .as_deref(),
            Some("tak.example.com:8446"),
        );
    }

    #[actix_web::test]
    async fn a_standard_https_port_is_left_out_of_the_url() {
        let mut config = config();
        config.web.public.listen = vec![
            rustak_core::config::ListenAddr::new("", 443),
            rustak_core::config::ListenAddr::new("", 8446),
        ];

        assert_eq!(
            Origin::from_config(&config).authority(None).as_deref(),
            Some("tak.example.com"),
        );
    }

    #[actix_web::test]
    async fn a_configured_base_url_decides_the_host_and_the_port_on_its_own() {
        // The proxy deployment: rustak binds 8446, the world sees 443 — or
        // whatever else the operator published it on.
        let mut config = config();
        config.server.base_url = Some("https://tak.example.org".to_string());

        assert_eq!(
            Origin::from_config(&config).authority(None).as_deref(),
            Some("tak.example.org"),
        );

        config.server.base_url = Some("https://tak.example.org:9443".to_string());
        assert_eq!(
            Origin::from_config(&config)
                .authority(Some("tak.example.com"))
                .as_deref(),
            Some("tak.example.com:9443"),
        );
    }

    #[actix_web::test]
    async fn an_installation_with_no_names_configured_answers_the_host_it_was_asked_for() {
        // There is nothing to compare against and nowhere else to send
        // anybody; the alternative is a listener that only ever says 400.
        let mut config = config();
        config.server.domains = Vec::new();

        let origin = Origin::from_config(&config);

        assert_eq!(
            origin.authority(Some("tak.example.com")).as_deref(),
            Some("tak.example.com:8446"),
        );
        assert_eq!(origin.authority(None), None);
    }

    #[actix_web::test]
    async fn a_host_header_that_is_not_a_name_is_never_put_in_a_location() {
        let origin = Origin::from_config(&config());

        for crafted in ["evil.net/\\path", "name with spaces", "", "a\"b"] {
            assert_eq!(
                origin.authority(Some(crafted)).as_deref(),
                Some("tak.example.com:8446"),
                "{crafted}",
            );
        }
    }

    #[actix_web::test]
    async fn everything_that_is_not_a_challenge_is_redirected_with_its_path() {
        let origin = Arc::new(Origin::from_config(&config()));
        let app = test::init_service(
            App::new()
                .app_data(web::Data::from(origin))
                .default_service(web::to(redirect)),
        )
        .await;

        for (method, uri) in [
            (actix_web::http::Method::GET, "/api/v1/health"),
            (actix_web::http::Method::POST, "/api/v1/auth/token"),
            (actix_web::http::Method::GET, "/?a=1"),
        ] {
            let request = test::TestRequest::default()
                .method(method.clone())
                .uri(uri)
                .insert_header((header::HOST, "tak.example.com"))
                .to_request();
            let response = test::call_service(&app, request).await;

            assert_eq!(response.status(), StatusCode::MOVED_PERMANENTLY, "{uri}");
            assert_eq!(
                response
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|value| value.to_str().ok()),
                Some(format!("https://tak.example.com:8446{uri}").as_str()),
                "{method} {uri}",
            );
        }
    }

    #[actix_web::test]
    async fn the_challenge_path_is_answered_here_and_not_redirected() {
        let state = Arc::new(crate::services::AcmeState::new());
        state.publish("token", "token.thumbprint");

        let app = test::init_service(
            App::new()
                .app_data(web::Data::from(Arc::new(Origin::from_config(&config()))))
                .configure(crate::pki::acme::http01_routes(Arc::clone(&state)))
                .default_service(web::to(redirect)),
        )
        .await;

        let response = test::call_service(
            &app,
            test::TestRequest::get()
                .uri("/.well-known/acme-challenge/token")
                .insert_header((header::HOST, "tak.example.com"))
                .to_request(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            test::read_body(response).await,
            "token.thumbprint".as_bytes()
        );
    }

    #[actix_web::test]
    async fn nothing_is_bound_when_the_key_is_not_set() {
        let context = AppContext::new_mock(|config| {
            config.web.public.plain_bind = None;
        })
        .await
        .unwrap();

        assert!(build_plain(context).unwrap().is_none());
    }

    #[actix_web::test]
    async fn the_listener_binds_the_address_it_was_given() {
        let context = AppContext::new_mock(|config| {
            config.server.domains = vec!["tak.example.com".to_string()];
            config.web.public.tls.mode = TlsMode::Acme;
            // Port zero, so concurrent suites do not race for a number.
            config.web.public.plain_bind =
                Some(rustak_core::config::ListenAddr::new("127.0.0.1", 0));
        })
        .await
        .unwrap();

        assert!(build_plain(context).unwrap().is_some());
    }
}
