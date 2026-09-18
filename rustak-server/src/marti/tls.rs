//! `/Marti/api/tls/*` — turning a username and a secret into a certificate.
//!
//! This is the first thing an EUD does and the only part of the surface a
//! device reaches before it has an identity. ATAK asks for the name entries,
//! builds a signing request from them and posts it; CloudTAK does the same
//! through `node-forge`. Both then keep the key and never send it anywhere.
//!
//! # The three rules that break a client if they slip
//!
//! 1. **`200`, never `201`.** ATAK's status mapping treats only `200` as
//!    success, so a `201` on a perfectly good enrolment reads as a failure
//!    (`compat/enrollment.md` §3).
//! 2. **At least two `<nameEntry>` elements.** CloudTAK parses the config
//!    document with `xml-js` in compact mode, which collapses a single-element
//!    array to a bare object; its `for (… of nameEntries.nameEntry)` then throws
//!    on a non-iterable (§1).
//! 3. **Bare base64, no PEM armour**, in both representations. Each client adds
//!    the banner itself, and they disagree about which end does it (§3).
//!
//! # We choose the subject
//!
//! The common name in the signing request must equal the authenticated user,
//! case-insensitively, and a mismatch is a `403` rather than a silent rewrite.
//! What is actually issued carries our own subject regardless — see
//! [`crate::pki`] — so the request contributes its public key and nothing else.
//!
//! # A one-time token is spent on success only
//!
//! `tls/config` is called first and `signClient/v2` second, with the same
//! credential. Consuming the token on the first call would strand somebody
//! half-way through enrolling with a token that no longer works, so it is spent
//! by [`sign_client_v2`] after a certificate has been issued and recorded.

use actix_web::http::StatusCode;
use actix_web::http::header::{ACCEPT, HeaderValue, WWW_AUTHENTICATE};
use actix_web::{HttpRequest, HttpResponse, web};

use crate::auth::basic::CHALLENGE;
use crate::auth::resolve::{AuthFailure, ListenerAuthPolicy, Resolved, resolve_principal};
use crate::pki::{IssuedCert, IssuedVia, Pki, bare_base64_64col, legacy_signclient_v1};
use crate::prelude::*;

use super::enroll::{SignQuery, internal_error, issue};
use super::error::{MartiError, MartiResult};
use super::extract::ListenerRole;
use super::response;

/// Which representation the client asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Representation {
    /// CloudTAK, and anything that did not say.
    Json,
    /// ATAK.
    Xml,
}

impl Representation {
    /// Reads `Accept`, defaulting to JSON.
    ///
    /// An absent header, `*/*` and an empty value all mean JSON, because that
    /// is what CloudTAK sends and what every client that does not care gets.
    /// Anything we cannot answer is [`None`], which the handler turns into a
    /// `400` rather than guessing.
    fn of(request: &HttpRequest) -> Option<Self> {
        let Some(accept) = request.headers().get(ACCEPT).and_then(|v| v.to_str().ok()) else {
            return Some(Self::Json);
        };

        if accept.trim().is_empty() {
            return Some(Self::Json);
        }

        // One header may list several; the first we can serve wins, which is
        // how a client saying `application/xml, */*` gets XML.
        for offered in accept.split(',') {
            let media = offered.split(';').next().unwrap_or_default().trim();

            match media {
                "application/xml" | "text/xml" => return Some(Self::Xml),
                "application/json" | "*/*" | "application/*" => return Some(Self::Json),
                _ => {}
            }
        }

        None
    }

    /// How the certificate came to exist, for the audit trail.
    fn issued_via(self) -> IssuedVia {
        match self {
            Self::Json => IssuedVia::EnrollV2Json,
            Self::Xml => IssuedVia::EnrollV2Xml,
        }
    }
}

/// `GET /Marti/api/tls/config` — the name entries a client builds its signing
/// request from.
///
/// # Errors
///
/// [`MartiError::Internal`] when the authority is not available.
pub async fn config(request: HttpRequest, context: web::Data<AppContext>) -> MartiResult {
    let pki = match caller(&request, &context).await {
        Ok((_, pki)) => pki,
        Err(response) => return Ok(response),
    };

    Ok(response::xml(certificate_config(&pki)))
}

/// `POST /Marti/api/tls/signClient/v2` — the enrolment itself.
///
/// # Errors
///
/// [`MartiError::InvalidRequest`] for an `Accept` we cannot answer or a signing
/// request we cannot read, [`MartiError::Forbidden`] when the common name is
/// not the authenticated user, and [`MartiError::Internal`] when signing or
/// recording fails.
pub async fn sign_client_v2(
    request: HttpRequest,
    context: web::Data<AppContext>,
    query: web::Query<SignQuery>,
    body: web::Bytes,
) -> MartiResult {
    let (resolved, pki) = match caller(&request, &context).await {
        Ok(pair) => pair,
        Err(response) => return Ok(response),
    };

    let Some(representation) = Representation::of(&request) else {
        return Err(MartiError::InvalidRequest(
            "this endpoint answers application/json or application/xml".to_string(),
        ));
    };

    let issued = issue(
        &request,
        &context,
        &pki,
        &resolved,
        &query,
        &body,
        representation.issued_via(),
    )
    .await?;

    Ok(match representation {
        Representation::Json => response::bare_json(&json_body(&pki, &issued)),
        Representation::Xml => response::xml(xml_body(&pki, &issued)),
    })
}

/// `POST /Marti/api/tls/signClient` — the pre-v2 endpoint, which answers with a
/// PKCS#12 bundle of certificates rather than base64.
///
/// # Errors
///
/// As [`sign_client_v2`], plus [`MartiError::Internal`] when the bundle cannot
/// be written.
pub async fn sign_client_v1(
    request: HttpRequest,
    context: web::Data<AppContext>,
    query: web::Query<SignQuery>,
    body: web::Bytes,
) -> MartiResult {
    let (resolved, pki) = match caller(&request, &context).await {
        Ok(pair) => pair,
        Err(response) => return Ok(response),
    };

    let issued = issue(
        &request,
        &context,
        &pki,
        &resolved,
        &query,
        &body,
        IssuedVia::EnrollV1P12,
    )
    .await?;

    let chain = pki.chain();
    let links: Vec<&[u8]> = chain.iter().map(|link| link.as_ref()).collect();
    let options = pki.p12_options(resolved.user.username.as_str());

    let bundle = legacy_signclient_v1(&issued.der, &links, &options)
        .map_err(|err| internal_error(&context, &err))?;

    Ok(HttpResponse::Ok()
        .insert_header((
            actix_web::http::header::CONTENT_TYPE,
            "application/octet-stream",
        ))
        .body(bundle))
}

/// `GET /Marti/api/tls/profile/enrollment` — the package ATAK fetches straight
/// after enrolling.
///
/// `204` until device profiles land in M3: ATAK reads "there is nothing for
/// you" and carries on to the stream, which is exactly the behaviour we want
/// until there is something to send.
///
/// # Errors
///
/// Never.
pub async fn enrollment_profile() -> MartiResult {
    Ok(HttpResponse::NoContent().finish())
}

/// `GET /Marti/api/device/profile/connection` — the on-connect profile fetch.
///
/// `204` for the same reason as [`enrollment_profile`].
///
/// # Errors
///
/// Never.
pub async fn connection_profile() -> MartiResult {
    Ok(HttpResponse::NoContent().finish())
}

/// Who is enrolling, and the authority that will answer them.
///
/// The refusal is returned as a response rather than as a [`MartiError`]
/// because these paths answer a bare `Unauthorized` with a `WWW-Authenticate`
/// challenge — the shape a TAK client and an HTTP client both understand —
/// rather than the JSON envelope the rest of the Marti surface uses.
async fn caller(
    request: &HttpRequest,
    context: &AppContext,
) -> Result<(Resolved, std::sync::Arc<Pki>), HttpResponse> {
    let resolved = resolve_principal(context, request, policy_for(request))
        .await
        .map_err(|failure| refusal(context, failure))?;

    let pki = context.pki().map_err(|err| {
        error!(error = %err, "An enrolment request arrived before the authority was ready.");
        context.session().record_human_error(&err);

        HttpResponse::ServiceUnavailable()
            .insert_header((actix_web::http::header::CONTENT_TYPE, response::TEXT_PLAIN))
            .body("Certificate enrollment is not available on this server.")
    })?;

    Ok((resolved, pki))
}

/// The credential policy of the listener this request arrived on.
pub(super) fn policy_for(request: &HttpRequest) -> ListenerAuthPolicy {
    let role = request
        .app_data::<web::Data<ListenerRole>>()
        .map_or(ListenerRole::Public, |role| *role.get_ref());

    super::auth_policy(role).listener()
}

/// What a refused enrolment looks like on the wire.
///
/// Plain text and a Basic challenge, never the JSON envelope: these are the
/// paths where a client is *expected* to retry with credentials, and the
/// challenge is how it is told so.
fn refusal(context: &AppContext, failure: AuthFailure) -> HttpResponse {
    let (status, body) = match failure {
        AuthFailure::Rejected => (StatusCode::UNAUTHORIZED, "Unauthorized"),
        AuthFailure::Forbidden(_) => (StatusCode::FORBIDDEN, "Forbidden"),
        AuthFailure::RateLimited(_) => (StatusCode::TOO_MANY_REQUESTS, "Too Many Requests"),
        AuthFailure::Unavailable(err) => {
            error!(error = %err, "Could not resolve who an enrolment request is from.");
            context.session().record_human_error(&err);

            (StatusCode::INTERNAL_SERVER_ERROR, "Server Error")
        }
    };

    let mut builder = HttpResponse::build(status);
    builder.insert_header((actix_web::http::header::CONTENT_TYPE, response::TEXT_PLAIN));

    if status == StatusCode::UNAUTHORIZED {
        builder.insert_header((WWW_AUTHENTICATE, HeaderValue::from_static(CHALLENGE)));
    }

    builder.body(body)
}

/// The `ns2:certificateConfig` document, exactly as both clients parse it.
///
/// The `xmlns:ns2` value is the literal string `com.bbn.marti.config` rather
/// than a URI — that is what TAK Server emits and what CloudTAK's client indexes
/// by, so it is reproduced verbatim.
fn certificate_config(pki: &Pki) -> String {
    let mut entries = pki.name_entries();

    // CloudTAK's compact-mode parser collapses a one-element array to an
    // object and then iterates it. Two elements is the contract, not a
    // preference — an empty value is better than a broken client.
    if entries.len() < 2 {
        entries.push(("OU", ""));
    }

    let rendered: String = entries
        .iter()
        .map(|(name, value)| {
            format!(
                "<nameEntry name=\"{}\" value=\"{}\"/>",
                escape(name),
                escape(value)
            )
        })
        .collect();

    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <ns2:certificateConfig xmlns=\"http://bbn.com/marti/xml/config\" \
         xmlns:ns2=\"com.bbn.marti.config\">\
         <nameEntries>{rendered}</nameEntries>\
         </ns2:certificateConfig>"
    )
}

/// The JSON representation: `signedCert` plus one `caN` per link in the chain.
fn json_body(pki: &Pki, issued: &IssuedCert) -> serde_json::Value {
    let mut body = serde_json::Map::new();
    body.insert(
        "signedCert".to_string(),
        bare_base64_64col(&issued.der).into(),
    );

    for (index, link) in pki.chain().iter().enumerate() {
        body.insert(format!("ca{index}"), bare_base64_64col(link).into());
    }

    serde_json::Value::Object(body)
}

/// The XML representation ATAK reads: every child but `signedCert` is a CA.
fn xml_body(pki: &Pki, issued: &IssuedCert) -> String {
    let chain: String = pki
        .chain()
        .iter()
        .map(|link| format!("<ca>{}</ca>", bare_base64_64col(link)))
        .collect();

    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><enrollment><signedCert>{}</signedCert>{chain}</enrollment>",
        bare_base64_64col(&issued.der),
    )
}

/// XML-escapes an attribute value.
///
/// TAK Server does not; we do, because a name entry an administrator typed can
/// contain an ampersand and ATAK parses this with a real XML parser.
fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test::TestRequest;

    #[test]
    fn an_absent_or_permissive_accept_header_means_json() {
        // CloudTAK sends no Accept at all on this call.
        for request in [
            TestRequest::default().to_http_request(),
            TestRequest::default()
                .insert_header((ACCEPT, "*/*"))
                .to_http_request(),
            TestRequest::default()
                .insert_header((ACCEPT, "application/json"))
                .to_http_request(),
        ] {
            assert_eq!(Representation::of(&request), Some(Representation::Json));
        }
    }

    #[test]
    fn atak_asks_for_xml_and_gets_it() {
        let request = TestRequest::default()
            .insert_header((ACCEPT, "application/xml"))
            .to_http_request();

        assert_eq!(Representation::of(&request), Some(Representation::Xml));

        // A list is answered with the first entry we can serve.
        let request = TestRequest::default()
            .insert_header((ACCEPT, "application/xml, */*"))
            .to_http_request();

        assert_eq!(Representation::of(&request), Some(Representation::Xml));
    }

    #[test]
    fn something_we_cannot_answer_is_refused_rather_than_guessed() {
        let request = TestRequest::default()
            .insert_header((ACCEPT, "application/pkix-cert"))
            .to_http_request();

        assert_eq!(Representation::of(&request), None);
    }

    #[test]
    fn the_audit_trail_records_which_representation_was_served() {
        assert_eq!(Representation::Json.issued_via(), IssuedVia::EnrollV2Json);
        assert_eq!(Representation::Xml.issued_via(), IssuedVia::EnrollV2Xml);
    }

    #[test]
    fn an_attribute_value_that_needs_escaping_gets_it() {
        assert_eq!(escape(r#"A & B <"C">"#), "A &amp; B &lt;&quot;C&quot;&gt;");
    }
}
