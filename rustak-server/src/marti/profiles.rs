//! `/Marti/api/tls/profile/*` and `/Marti/api/device/profile/*` — the
//! configuration a device pulls for itself.
//!
//! ATAK calls these twice: once, unconditionally, straight after enrolling,
//! and again on every stream connect. Nothing here is used by CloudTAK; this
//! surface exists entirely for ATAK compatibility.
//!
//! # Three statuses, and getting one wrong is silent
//!
//! * **`204`** — there is nothing for you. ATAK logs it and carries on.
//! * **`200`** — a zip, or a single raw file on the `/file` endpoints.
//! * **`304`** — everything that matched is older than the `If-Modified-Since`
//!   the client echoed back from our own `Last-Modified`.
//!
//! Anything else is a `ConnectionException` inside ATAK, which surfaces to the
//! user as a failed connection rather than as a missing profile.
//!
//! # The enrolment profile is fetched with a credential that is already spent
//!
//! `/tls/profile/enrollment` is the third call of ATAK's enrolment and carries
//! the same one-time token the signing call consumed, so authenticating it is
//! not the ordinary question. The answer is not here: the two `/tls/profile`
//! routes map to [`Purpose::EnrollmentProfile`], which
//! [`crate::identity::verify::Grace`] allows a spent token for — same device,
//! same window, nothing else (M2-15). Handlers below are unchanged by it, and a
//! route added under `/tls/profile/` inherits the relaxation, so put anything
//! that is not a device profile somewhere else.
//!
//! [`Purpose::EnrollmentProfile`]: crate::identity::verify::Purpose::EnrollmentProfile
//!
//! # There is no `/device/profile/enrollment`
//!
//! Real TAK Server has no such mapping despite its own security configuration
//! granting the path. The enrolment profile lives only at
//! `/Marti/api/tls/profile/enrollment`, and adding an alias would be a route
//! that no client calls and that our own contract tests would then have to
//! justify.

use actix_web::http::header::{
    CONTENT_DISPOSITION, CONTENT_TYPE, HeaderValue, HttpDate, LastModified,
};
use actix_web::{HttpRequest, HttpResponse, web};

use crate::prelude::*;

use crate::profiles::UserSettings;
use bytes::Bytes;

use crate::profiles::builder::PROFILE_FILENAME;
use crate::profiles::package::{multifile_package, profile_package};
use crate::profiles::repo::ProfilesRepo;
use crate::profiles::service::{Assembled, ProfileService};

use super::error::{MartiError, MartiResult};
use super::extract::CiQuery;
use super::principal::MartiPrincipal;
use super::response::{self, kind};

/// `application/zip`, which is what ATAK checks before unpacking a body it was
/// not told to import.
const ZIP: HeaderValue = HeaderValue::from_static("application/zip");

/// Registers every profile route.
///
/// Order matters: the two literal `device/profile` children are registered
/// before the `{name}` that would also match them, and `/tool/{tool}/file`
/// before `/tool/{tool}`.
pub fn routes(config: &mut web::ServiceConfig) {
    config
        .route("/tls/profile/enrollment", web::get().to(enrollment))
        .route("/tls/profile/tool/{tool}/file", web::get().to(tool_file))
        .route("/device/profile/connection", web::get().to(connection))
        .route("/device/profile/tool/{tool}/file", web::get().to(tool_file))
        .route("/device/profile/tool/{tool}", web::get().to(tool))
        .route("/device/profile", web::get().to(admin_list))
        .route("/device/profile/directories", web::get().to(unsupported))
        .route(
            "/device/profile/{name}/missionpackage",
            web::get().to(mission_package),
        )
        .route(
            "/device/profile/{name}/missionpackage",
            web::head().to(mission_package_head),
        )
        .route("/device/profile/{name}", web::get().to(admin_get));
}

/// `GET /Marti/api/tls/profile/enrollment?clientUid=`.
///
/// # Errors
///
/// [`MartiError::InvalidRequest`] without a `clientUid`,
/// [`MartiError::Unauthorized`] without a credential, and
/// [`MartiError::Internal`] when a read fails.
pub async fn enrollment(
    request: HttpRequest,
    context: web::Data<AppContext>,
    who: MartiPrincipal,
    query: CiQuery,
) -> MartiResult {
    require_client_uid(&query)?;

    let resolved = who.require()?;
    let service = ProfileService::new(context.db(), content(&context)?);

    let held = service
        .group_names(&resolved.principal.groups)
        .await
        .map_err(internal)?;

    let user = UserSettings {
        callsign: resolved.user.display_name.clone(),
        ..UserSettings::default()
    };

    let assembled = service
        .enrollment(&hostname(&request, &context), &held, &user, true)
        .await
        .map_err(internal)?;

    packaged("Enrollment", assembled).await
}

/// `GET /Marti/api/device/profile/connection?syncSecago=&clientUid=`.
///
/// # Errors
///
/// As [`enrollment`], plus [`MartiError::InvalidRequest`] for a `syncSecago`
/// that is not a number.
pub async fn connection(
    context: web::Data<AppContext>,
    who: MartiPrincipal,
    query: CiQuery,
) -> MartiResult {
    require_client_uid(&query)?;

    let (service, held) = caller(&context, &who).await?;
    let assembled = service
        .connection(&held, sync_secago(&query)?)
        .await
        .map_err(internal)?;

    packaged("Connection", assembled).await
}

/// `GET /Marti/api/device/profile/tool/{tool}?syncSecago=&clientUid=`.
///
/// # Errors
///
/// As [`connection`].
pub async fn tool(
    path: web::Path<String>,
    context: web::Data<AppContext>,
    who: MartiPrincipal,
    query: CiQuery,
) -> MartiResult {
    require_client_uid(&query)?;

    let name = path.into_inner();
    let (service, held) = caller(&context, &who).await?;
    let assembled = service
        .tool(&name, &held, sync_secago(&query)?)
        .await
        .map_err(internal)?;

    packaged(&name, assembled).await
}

/// `GET /Marti/api/{tls,device}/profile/tool/{tool}/file?relativePath=…`.
///
/// One file comes back raw, several come back as a `multiFile` package that
/// keeps the directories the files were stored under, and nothing left after
/// the `If-Modified-Since` filter is a `304`.
///
/// # Errors
///
/// [`MartiError::InvalidRequest`] without a `clientUid` or `relativePath`, or
/// for a `relativePath` that tries to climb out of the profile;
/// [`MartiError::NotFound`] when no profile serves that tool or none of the
/// paths matched.
pub async fn tool_file(
    request: HttpRequest,
    path: web::Path<String>,
    context: web::Data<AppContext>,
    who: MartiPrincipal,
    query: CiQuery,
) -> MartiResult {
    require_client_uid(&query)?;

    let wanted = relative_paths(&query)?;
    let name = path.into_inner();
    let (service, held) = caller(&context, &who).await?;

    if !service.tool_exists(&name, &held).await.map_err(internal)? {
        return Err(MartiError::NotFound(format!("no profile serves {name}")));
    }

    let assembled = service
        .tool_files(&name, &held, &wanted, sync_secago(&query)?)
        .await
        .map_err(internal)?;

    if assembled.is_empty() {
        return Err(MartiError::NotFound(format!(
            "no file of {name} matched that relativePath"
        )));
    }

    let since = request
        .headers()
        .get(actix_web::http::header::IF_MODIFIED_SINCE);
    let filtered = drop_unchanged(assembled, since);

    // Everything matched but nothing changed: the client already has it.
    let Some(assembled) = filtered else {
        return Ok(HttpResponse::NotModified().finish());
    };

    if let [only] = assembled.files.as_slice() {
        return Ok(download(
            only.content_type(),
            only.basename(),
            Bytes::from(only.data.clone()),
            assembled.last_modified,
        ));
    }

    let last_modified = assembled.last_modified;
    let body = multifile_package(assembled.files).await.map_err(internal)?;

    Ok(download(ZIP, PROFILE_FILENAME, body, last_modified))
}

/// `GET /Marti/api/device/profile/{name}/missionpackage`.
///
/// # Errors
///
/// [`MartiError::NotFound`] when there is no profile by that name.
pub async fn mission_package(
    path: web::Path<String>,
    context: web::Data<AppContext>,
    who: MartiPrincipal,
) -> MartiResult {
    let name = path.into_inner();
    let (service, _) = caller(&context, &who).await?;

    let row = service
        .repo()
        .get_by_name(&name)
        .await
        .map_err(internal)?
        .ok_or_else(|| MartiError::NotFound(format!("no profile called {name}")))?;

    let assembled = service.assemble_one(&row).await.map_err(internal)?;

    packaged(&name, assembled).await
}

/// `HEAD /Marti/api/device/profile/{name}/missionpackage`, which TAK Server
/// answers unconditionally and which ATAK only uses to check reachability.
///
/// # Errors
///
/// Never.
pub async fn mission_package_head(_: web::Path<String>) -> MartiResult {
    Ok(HttpResponse::Ok().finish())
}

/// `GET /Marti/api/device/profile` — the administrative listing.
///
/// # Errors
///
/// [`MartiError::Forbidden`] for a caller who does not administer this
/// installation.
pub async fn admin_list(context: web::Data<AppContext>, who: MartiPrincipal) -> MartiResult {
    who.require_admin()?;

    let listed = ProfileService::new(context.db(), content(&context)?)
        .list()
        .await
        .map_err(internal)?;

    Ok(response::ok(kind::PROFILE, listed))
}

/// `GET /Marti/api/device/profile/{name}`.
///
/// # Errors
///
/// As [`admin_list`], plus [`MartiError::NotFound`].
pub async fn admin_get(
    path: web::Path<String>,
    context: web::Data<AppContext>,
    who: MartiPrincipal,
) -> MartiResult {
    who.require_admin()?;

    let name = path.into_inner();
    let service = ProfileService::new(context.db(), content(&context)?);

    let row = ProfilesRepo::new(context.db())
        .get_by_name(&name)
        .await
        .map_err(internal)?
        .ok_or_else(|| MartiError::NotFound(format!("no profile called {name}")))?;

    let described = service.describe(row).await.map_err(internal)?;

    Ok(response::ok(kind::PROFILE, described))
}

/// The administrative corners rustak manages through `/api/v1` instead.
///
/// # Errors
///
/// Always [`MartiError::NotImplemented`].
pub async fn unsupported() -> MartiResult {
    Err(MartiError::NotImplemented(
        "profile directories are managed through the rustak admin API",
    ))
}

/// The service and the caller's channels, or a refusal.
async fn caller<'a>(
    context: &'a web::Data<AppContext>,
    who: &MartiPrincipal,
) -> Result<(ProfileService<'a>, Vec<rustak_api::GroupName>), MartiError> {
    let resolved = who.require()?;
    let service = ProfileService::new(context.db(), content(context)?);

    let held = service
        .group_names(&resolved.principal.groups)
        .await
        .map_err(internal)?;

    Ok((service, held))
}

/// The content store, or a refusal a client can read.
fn content(
    context: &web::Data<AppContext>,
) -> Result<std::sync::Arc<crate::store::ContentStore>, MartiError> {
    context.content().map_err(internal)
}

/// `204` when there is nothing, and the package otherwise.
async fn packaged(name: &str, assembled: Assembled) -> MartiResult {
    if assembled.is_empty() {
        return Ok(HttpResponse::NoContent().finish());
    }

    let last_modified = assembled.last_modified;
    // Built on a blocking thread and kept until the profile changes: this is
    // reached from `GET .../profile/connection`, which every device asks for on
    // every connection.
    let body = profile_package(name, assembled.files)
        .await
        .map_err(internal)?;

    Ok(download(ZIP, PROFILE_FILENAME, body, last_modified))
}

/// A download, with the **unquoted** `Content-Disposition` ATAK parses and the
/// `Last-Modified` it will echo back as `If-Modified-Since`.
fn download(
    content_type: impl TryInto<HeaderValue>,
    filename: &str,
    body: Bytes,
    last_modified: Option<chrono::DateTime<chrono::Utc>>,
) -> HttpResponse {
    let mut response = HttpResponse::Ok();

    response
        .insert_header((
            CONTENT_TYPE,
            content_type
                .try_into()
                .unwrap_or(HeaderValue::from_static("application/octet-stream")),
        ))
        .insert_header((
            CONTENT_DISPOSITION,
            HeaderValue::from_str(&format!("attachment; filename={filename}"))
                .unwrap_or(HeaderValue::from_static("attachment")),
        ));

    if let Some(at) = last_modified {
        response.insert_header(LastModified(std::time::SystemTime::from(at).into()));
    }

    response.body(body)
}

/// Drops every file that is not newer than `If-Modified-Since`.
///
/// [`None`] means nothing is left, which is a `304`.
fn drop_unchanged(assembled: Assembled, header: Option<&HeaderValue>) -> Option<Assembled> {
    let Some(since) = header
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<HttpDate>().ok())
    else {
        return Some(assembled);
    };

    let since = chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::from(since));
    let mut kept = Assembled::default();

    for file in assembled.files {
        // Whole seconds on both sides: `Last-Modified` cannot carry the
        // millisecond part, so comparing against the stored value would report
        // a change on every request for a file whose timestamp is not exactly
        // on a second.
        let updated =
            chrono::DateTime::from_timestamp(file.updated.timestamp(), 0).unwrap_or(file.updated);

        if updated > since {
            kept.files.push(file);
        }
    }

    (!kept.files.is_empty()).then_some(Assembled {
        last_modified: assembled.last_modified,
        ..kept
    })
}

/// `clientUid` is required on every one of these endpoints.
fn require_client_uid(query: &CiQuery) -> Result<(), MartiError> {
    if query
        .get("clientUid")
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Ok(());
    }

    Err(MartiError::InvalidRequest("clientUid is required".into()))
}

/// `syncSecago`, defaulting to `-1` — "everything".
fn sync_secago(query: &CiQuery) -> Result<i64, MartiError> {
    Ok(query.parsed::<i64>("syncSecago")?.unwrap_or(-1))
}

/// The `relativePath` values, with a leading `/` stripped and traversal
/// refused.
///
/// ATAK forces a leading `/` on each value and percent-encodes only a literal
/// space, so the values arrive almost verbatim — including any `..` a caller
/// chose to put there.
fn relative_paths(query: &CiQuery) -> Result<Vec<String>, MartiError> {
    let raw = query.all("relativePath");

    if raw.is_empty() {
        return Err(MartiError::InvalidRequest(
            "relativePath is required".into(),
        ));
    }

    let mut paths = Vec::with_capacity(raw.len());

    for value in raw {
        let trimmed = value.trim_start_matches('/');

        if trimmed.split('/').any(|segment| segment == "..") || trimmed.contains('\\') {
            return Err(MartiError::InvalidRequest(format!(
                "relativePath={value} leaves the profile"
            )));
        }

        paths.push(trimmed.to_string());
    }

    Ok(paths)
}

/// The host name a device should scope its host-keyed preferences by.
fn hostname(request: &HttpRequest, context: &AppContext) -> String {
    let config = context.config();

    crate::profiles::service::preferred_host(
        config.marti.public_host.as_deref(),
        crate::web::helpers::request::header_str(request.headers(), "host"),
        config.server.canonical_domain(),
    )
}

/// What a failure inside this server is reported as.
fn internal(err: Error) -> MartiError {
    error!(error = %err, "A device profile could not be assembled.");

    MartiError::Internal("the device profile could not be assembled".into())
}

#[cfg(test)]
mod tests {
    use actix_web::http::header::HeaderValue;

    use crate::profiles::builder::ProfileFileData;

    use super::*;

    #[test]
    fn a_relative_path_that_climbs_out_is_refused() {
        for value in ["/../../etc/passwd", "a/../../b", "..", "a\\b"] {
            let query = CiQuery::parse(&format!("relativePath={value}"));
            assert!(relative_paths(&query).is_err(), "{value}");
        }
    }

    #[test]
    fn a_leading_slash_is_stripped_and_every_value_is_kept() {
        let query = CiQuery::parse("relativePath=/maps&relativePath=/top.pref");

        assert_eq!(
            relative_paths(&query).unwrap(),
            vec!["maps".to_string(), "top.pref".to_string()],
        );
    }

    #[test]
    fn a_missing_relative_path_is_a_bad_request() {
        assert!(relative_paths(&CiQuery::parse("clientUid=A")).is_err());
    }

    #[test]
    fn a_client_uid_is_required_and_may_not_be_blank() {
        assert!(require_client_uid(&CiQuery::parse("clientUid=ANDROID-1")).is_ok());
        assert!(require_client_uid(&CiQuery::parse("clientUid=")).is_err());
        assert!(require_client_uid(&CiQuery::parse("")).is_err());
        assert!(
            require_client_uid(&CiQuery::parse("clientuid=A")).is_ok(),
            "the query is case-insensitive, as every Marti parameter is",
        );
    }

    #[test]
    fn sync_secago_defaults_to_everything() {
        assert_eq!(sync_secago(&CiQuery::parse("")).unwrap(), -1);
        assert_eq!(sync_secago(&CiQuery::parse("syncSecago=60")).unwrap(), 60);
        assert!(sync_secago(&CiQuery::parse("syncSecago=soon")).is_err());
    }

    #[test]
    fn nothing_newer_than_the_client_already_has_is_a_304() {
        let old = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let new = chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap();

        let assembled = Assembled {
            files: vec![
                ProfileFileData::new("a", Vec::new(), old),
                ProfileFileData::new("b", Vec::new(), new),
            ],
            last_modified: Some(new),
        };

        let header = |at: chrono::DateTime<chrono::Utc>| {
            HeaderValue::from_str(&HttpDate::from(std::time::SystemTime::from(at)).to_string())
                .unwrap()
        };

        assert!(
            drop_unchanged(assembled.clone(), Some(&header(new))).is_none(),
            "everything the client asked about is older than what it already has",
        );

        let kept = drop_unchanged(assembled.clone(), Some(&header(old))).unwrap();
        assert_eq!(kept.files.len(), 1, "only the newer file is still owed");

        assert_eq!(
            drop_unchanged(assembled, None).unwrap().files.len(),
            2,
            "a client that said nothing is owed everything",
        );
    }
}
