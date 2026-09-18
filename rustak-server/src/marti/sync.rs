//! `/Marti/sync/*` — the legacy Enterprise Sync servlets, and their uploads.
//!
//! The oldest part of the TAK surface and the part every client still uses:
//! ATAK's "send to server", its package browser, CloudTAK's file upload and
//! every attachment either of them shares go through these six routes. This
//! file registers all six and implements the two that *write*; the four that
//! read are in [`super::sync_read`], which is the split design 04 anticipated
//! for exactly this reason.
//!
//! # Three content types, none of them `application/json`
//!
//! `upload` and `search` answer **`text/json`**, `missionupload` and
//! `missionquery` answer **`text/plain`** carrying a bare URL, and `delete`
//! answers **`text/html`**. Those are not mistakes to tidy: ATAK reads the URL
//! straight out of the `missionupload` body and puts it in the `senderUrl` of
//! the `b-f-t-r` it sends next, so a JSON wrapper or a trailing newline would
//! travel to a peer and fail there.
//!
//! # What a failure looks like
//!
//! These paths are served by the servlet container in a real TAK Server, so
//! their `404` is an HTML document rather than the JSON refusal the rest of the
//! surface uses — and node-tak's error handling sniffs for exactly that. The
//! not-found cases here therefore answer [`MartiError::Html404`].
//!
//! # Nothing is buffered
//!
//! An upload is streamed through [`crate::files::store::ingest`] into the
//! content store while being hashed, and a download is streamed back off the
//! disk. See [`crate::files::store`] for why the size limit is enforced inside
//! the reader rather than around it.

use actix_multipart::Multipart;
use actix_web::http::StatusCode;
use actix_web::http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use actix_web::{HttpRequest, web};
use futures::TryStreamExt as _;

use crate::db::repos::ResourceRow;
use crate::files::store::{IngestError, Ingested};
use crate::files::upload::{self, Upload};
use crate::files::{legacy, metadata};
use crate::prelude::*;

use super::error::{MartiError, MartiResult};
use super::extract::CiQuery;
use super::principal::MartiPrincipal;
use super::response;
use super::sync_read::{self, content_url};

/// Registers the six legacy servlets under `/Marti`.
pub fn routes(config: &mut web::ServiceConfig) {
    config
        .route("/sync/upload", web::post().to(upload_resource))
        .route("/sync/search", web::get().to(sync_read::search_resources))
        .route("/sync/content", web::get().to(sync_read::content))
        .route("/sync/content", web::head().to(sync_read::content))
        .route("/sync/missionupload", web::post().to(mission_upload))
        .route(
            "/sync/missionquery",
            web::get().to(sync_read::mission_query),
        )
        .route("/sync/delete", web::get().to(sync_read::delete))
        .route("/sync/delete", web::post().to(sync_read::delete))
        .route("/sync/delete", web::delete().to(sync_read::delete));
}

/// `POST /Marti/sync/upload` — store a file and describe what was stored.
///
/// # Errors
///
/// [`MartiError::InvalidRequest`] for an empty or oversized body,
/// [`MartiError::Forbidden`] for a channel the caller is not in, and
/// [`MartiError::Internal`] when the store or the database fails.
#[instrument("marti.sync.upload", skip_all)]
pub async fn upload_resource(
    request: HttpRequest,
    who: MartiPrincipal,
    query: CiQuery,
    payload: web::Payload,
    context: web::Data<AppContext>,
) -> MartiResult {
    let viewer = viewer(&who, &context).await?;
    let upload = Upload::parse(&query)?;
    let groups = upload.groups_for(&viewer)?;
    let declared = declared_content_type(&request);

    let (stored, filename, part_type) = read_body(&request, payload, &context).await?;
    let mime = upload
        .mime_type
        .clone()
        .or(part_type)
        .or(declared)
        .unwrap_or_else(|| upload::DEFAULT_MIME.to_string());

    let resource = store_row(
        &context,
        Upload {
            mime_type: Some(mime),
            ..upload
        },
        &stored,
        &who,
        filename,
        groups,
    )
    .await?;

    upload::audit(&**context, "uploaded", who.username(), &resource).await;

    Ok(response::text_json(&legacy::metadata(&resource)))
}

/// `POST /Marti/sync/missionupload` — a data package, answered with its URL.
///
/// # Errors
///
/// [`MartiError::InvalidRequest`] when the body is not multipart or `filename`
/// is missing, and [`MartiError::Forbidden`] for a channel the caller is not
/// in.
#[instrument("marti.sync.missionupload", skip_all)]
pub async fn mission_upload(
    request: HttpRequest,
    who: MartiPrincipal,
    query: CiQuery,
    payload: web::Payload,
    context: web::Data<AppContext>,
) -> MartiResult {
    if !is_multipart(&request) {
        return Err(MartiError::InvalidRequest(
            "Data package upload must use multipart/form-data POST; \
             the part should be named 'assetfile'"
                .to_string(),
        ));
    }

    let Some(filename) = query.get("filename").map(str::to_string) else {
        return Err(MartiError::InvalidRequest(
            "filename is required".to_string(),
        ));
    };

    let viewer = viewer(&who, &context).await?;
    let mut upload = Upload::parse(&query)?;
    let groups = upload.groups_for(&viewer)?;

    // The defaults that put a package in the public list, and the client's own
    // `hash` dropped: we compute our own while streaming.
    upload.name = Some(filename.clone());
    upload.tool = upload
        .tool
        .or_else(|| Some(upload::DEFAULT_TOOL.to_string()));
    upload.mime_type = upload
        .mime_type
        .or_else(|| Some(upload::PACKAGE_MIME.to_string()));
    // `Upload::parse` reads the plural `keywords`; this route's documented
    // parameter is the **singular** `keyword`, repeated — which is the shape
    // node-tak's `Files.uploadPackage` sends (`compat/files.md` §5, research
    // `03` §3.12). Reading only the plural meant every keyword CloudTAK passed
    // was dropped and a package could not be found by the keyword it was
    // uploaded with. R-02 M4.
    for keyword in query.strings("keyword") {
        if !upload
            .keywords
            .iter()
            .any(|held| held.eq_ignore_ascii_case(&keyword))
        {
            upload.keywords.push(keyword);
        }
    }

    if !upload
        .keywords
        .iter()
        .any(|keyword| keyword.eq_ignore_ascii_case(upload::MISSION_PACKAGE))
    {
        upload.keywords.push(upload::MISSION_PACKAGE.to_string());
    }

    let (stored, part_name, _) = read_body(&request, payload, &context).await?;

    // A package is addressed by its hash, so re-uploading one a peer already
    // sent is the same package rather than a second copy. A row already holding
    // this hash under *another* name is somebody else's listing, so that case
    // gets its own row with a minted uid rather than overwriting theirs.
    let existing = context.db().resources().by_uid(&stored.hash).await?;
    let resource = match existing {
        Some(resource) if resource.name == filename => resource,
        existing => {
            upload.uid = existing.is_none().then(|| stored.hash.clone());

            let resource = store_row(
                &context,
                upload,
                &stored,
                &who,
                part_name.or(Some(filename)),
                groups,
            )
            .await?;
            upload::audit(&**context, "uploaded", who.username(), &resource).await;

            resource
        }
    };

    Ok(response::text(
        StatusCode::OK,
        content_url(&request, &context, &resource.hash),
    ))
}

/// Reads the upload's body, from a multipart part or from the body itself.
async fn read_body(
    request: &HttpRequest,
    payload: web::Payload,
    context: &AppContext,
) -> Result<(Ingested, Option<String>, Option<String>), MartiError> {
    // The resolved ceiling rather than the configuration value, so that a limit
    // an operator set from the admin UI is the one actually enforced — and the
    // one `/files/api/config` told the client about.
    let limit_mb = crate::files::limits::limit_mb(&context.config(), context.db()).await?;
    let limit = u64::from(limit_mb) * 1_000_000;

    if let Some(claimed) = claimed_length(request)
        && claimed > limit
    {
        return Err(too_large(limit_mb));
    }

    let store = context.content()?;

    if !is_multipart(request) {
        let stored = files_ingest(&store, payload.map_err(io_error), limit, limit_mb).await?;

        return Ok((stored, None, None));
    }

    let mut multipart = Multipart::new(request.headers(), payload);
    let Some(field) = upload::take_part(&mut multipart).await? else {
        return Err(MartiError::InvalidRequest(
            "no 'assetfile' or 'resource' part was sent".to_string(),
        ));
    };

    let filename = upload::part_filename(&field);
    let part_type = upload::part_content_type(&field);
    let stored = files_ingest(&store, field.map_err(io_error), limit, limit_mb).await?;

    Ok((stored, filename, part_type))
}

/// [`crate::files::ingest`] with the refusals mapped to TAK's own messages.
async fn files_ingest<S>(
    store: &crate::services::ContentStore,
    body: S,
    limit: u64,
    limit_mb: u32,
) -> Result<Ingested, MartiError>
where
    S: futures::Stream<Item = Result<actix_web::web::Bytes, std::io::Error>> + Unpin,
{
    match crate::files::ingest(store, body, limit).await {
        Ok(stored) => Ok(stored),
        Err(IngestError::TooLarge) => Err(too_large(limit_mb)),
        Err(IngestError::Empty) => Err(MartiError::InvalidRequest(
            "HTTP request body has no content.".to_string(),
        )),
        Err(IngestError::Failed(err)) => Err(err.into()),
    }
}

/// Writes the row an ingest earned.
async fn store_row(
    context: &AppContext,
    upload: Upload,
    stored: &Ingested,
    who: &MartiPrincipal,
    filename: Option<String>,
    groups: Vec<String>,
) -> Result<ResourceRow, MartiError> {
    let submitter = who
        .identity
        .as_ref()
        .map(|resolved| (resolved.user.username.as_str(), resolved.user.id));

    Ok(context
        .db()
        .resources()
        .upsert(upload.into_resource(stored, submitter, filename, groups))
        .await?)
}

/// Whether the request carries a multipart body.
fn is_multipart(request: &HttpRequest) -> bool {
    request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim_start().starts_with("multipart/"))
}

/// The `Content-Type` a raw upload declared, which becomes its `MIMEType`.
fn declared_content_type(request: &HttpRequest) -> Option<String> {
    request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.starts_with("multipart/"))
}

/// The size the client claimed, which is checked before a byte is read.
fn claimed_length(request: &HttpRequest) -> Option<u64> {
    request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse().ok())
}

/// TAK's own wording, which some tooling matches on.
fn too_large(limit_mb: u32) -> MartiError {
    MartiError::InvalidRequest(format!(
        "Uploaded file exceeds server's size limit of {limit_mb} MB!"
    ))
}

/// Body streams arrive with their own error types; the reader wants one.
fn io_error(err: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(err.to_string())
}

/// The caller's channels, for the visibility rule.
///
/// Shared with [`super::sync_read`] and [`super::files`], which apply the same
/// rule to the same rows.
///
/// # Errors
///
/// [`MartiError::Internal`] if the channel index cannot be read.
pub async fn viewer(
    who: &MartiPrincipal,
    context: &AppContext,
) -> Result<metadata::Viewer, MartiError> {
    Ok(metadata::viewer_for(context.db(), who.username(), who.principal()).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_size_refusal_carries_the_configured_number() {
        assert_eq!(
            too_large(400).message(),
            "Invalid Request: Uploaded file exceeds server's size limit of 400 MB!",
        );
    }

    #[test]
    fn a_raw_upload_keeps_the_content_type_it_declared_and_a_multipart_one_does_not() {
        let raw = actix_web::test::TestRequest::default()
            .insert_header((CONTENT_TYPE, "image/jpeg"))
            .to_http_request();
        let form = actix_web::test::TestRequest::default()
            .insert_header((CONTENT_TYPE, "multipart/form-data; boundary=x"))
            .to_http_request();

        assert_eq!(declared_content_type(&raw).as_deref(), Some("image/jpeg"));
        assert!(!is_multipart(&raw));
        assert_eq!(
            declared_content_type(&form),
            None,
            "the part's own type is what a multipart upload carries",
        );
        assert!(is_multipart(&form));
    }

    #[test]
    fn a_claimed_length_is_read_before_a_byte_of_the_body_is() {
        let claimed = actix_web::test::TestRequest::default()
            .insert_header((CONTENT_LENGTH, "2048"))
            .to_http_request();

        assert_eq!(claimed_length(&claimed), Some(2048));
        assert_eq!(
            claimed_length(&actix_web::test::TestRequest::default().to_http_request()),
            None,
            "a chunked upload carries no claim, so the reader is the only bound",
        );
    }
}
