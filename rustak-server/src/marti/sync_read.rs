//! `/Marti/sync/*` — the four legacy servlets that read or remove.
//!
//! Search, download, the "do you already have this?" probe and the delete that
//! answers an HTML status page. Registered by [`super::sync`], which owns the
//! route table for the whole `/Marti/sync` prefix; these are here because the
//! six handlers plus their plumbing do not fit one file, which is the split
//! design 04 names in its own risk table.
//!
//! # Not found is an HTML document here
//!
//! These paths are a servlet container's in a real TAK Server, so a miss is one
//! of its error pages rather than the JSON refusal the rest of the surface
//! uses. node-tak sniffs for exactly that, and wraps it with a parsed summary.
//!
//! An unreadable resource answers the **same** `404` as a missing one: telling
//! a caller that a hash exists but is not theirs is an oracle over the whole
//! store.

use actix_web::http::StatusCode;
use actix_web::http::header::{CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE};
use actix_web::{HttpRequest, HttpResponse, web};

use crate::db::repos::{ResourceFilter, ResourceRow};
use crate::files::store;
use crate::files::upload;
use crate::files::{legacy, search};
use crate::prelude::*;

use super::error::{MartiError, MartiResult};
use super::extract::CiQuery;
use super::principal::MartiPrincipal;
use super::sync::viewer;
use super::{headers, response};

/// What `/Marti/sync/delete` answers with, however it went.
const DELETE_PREFIX: &str = "<html><head><title>Enterprise Sync Status</title></head>\
                             <h1>Success</h1><p>Deleted ";

/// The rest of that document.
const DELETE_SUFFIX: &str = " resource(s).</p></html>";

/// `GET /Marti/sync/search` — the Title-case listing ATAK's browser reads.
///
/// # Errors
///
/// [`MartiError::InvalidRequest`] for a parameter that will not parse.
#[instrument("marti.sync.search", skip_all)]
pub async fn search_resources(
    who: MartiPrincipal,
    query: CiQuery,
    context: web::Data<AppContext>,
) -> MartiResult {
    let viewer = viewer(&who, &context).await?;
    let found = search::run(context.db(), &viewer, search::SearchQuery::parse(&query)?).await?;

    Ok(response::text_json(&legacy::search_results(&found)))
}

/// `GET|HEAD /Marti/sync/content` — the bytes, streamed off the disk.
///
/// # Errors
///
/// [`MartiError::Html404`] when nothing is stored under that hash or uid, which
/// is the HTML document a real TAK Server's container answers with.
#[instrument("marti.sync.content", skip_all)]
pub async fn content(
    request: HttpRequest,
    who: MartiPrincipal,
    query: CiQuery,
    context: web::Data<AppContext>,
) -> MartiResult {
    let viewer = viewer(&who, &context).await?;
    let resource = addressed(&context, &query).await?;

    if !viewer.can_read(&resource) {
        // The same answer as a file that is not there: telling an unauthorised
        // caller that a hash exists is an oracle over the whole store.
        return Err(MartiError::Html404);
    }

    let (offset, length, ranged) = range(&request, &query)?;
    let content = context.content()?;
    let opened = store::open_range(&content, &resource.hash, offset, length).await?;
    // TAK Server's condition is `length > 0 && offset + length < totalSize`
    // (research `06` line 1479), so an `?offset=` with no `length` — which is
    // exactly how ATAK's `GetFileTransferOperation` resumes a failed download —
    // reaches the end of the file and answers `200`. `Opened::is_partial` also
    // counts `offset > 0`, which is right for a real `Range:` header and wrong
    // here. R-02 M5.
    let partial = match ranged {
        true => opened.is_partial(),
        false => opened.length > 0 && opened.offset + opened.length < opened.total,
    };
    let disposition = format!(
        "inline; filename=\"{}\"",
        urlencode(
            resource
                .download_path
                .as_deref()
                .unwrap_or(resource.name.as_str())
        )
    );

    let mut builder = HttpResponse::build(if partial {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    });

    builder
        .insert_header(headers::api_version_header())
        .insert_header((CONTENT_TYPE, resource.mime_type.clone()))
        .insert_header((CONTENT_DISPOSITION, disposition))
        .insert_header((CONTENT_LENGTH, opened.length.to_string()));

    if partial {
        builder.insert_header(("content-range", opened.content_range()));
    }

    if request.method() == actix_web::http::Method::HEAD {
        return Ok(builder.finish());
    }

    Ok(builder.streaming(store::chunks(opened.file, opened.length)))
}

/// `GET /Marti/sync/missionquery` — "do you already have this package?".
///
/// # Errors
///
/// [`MartiError::Html404`] when it is not stored.
#[instrument("marti.sync.missionquery", skip_all)]
pub async fn mission_query(
    request: HttpRequest,
    who: MartiPrincipal,
    query: CiQuery,
    context: web::Data<AppContext>,
) -> MartiResult {
    let viewer = viewer(&who, &context).await?;
    let resource = addressed(&context, &query).await?;

    if !viewer.can_read(&resource) {
        return Err(MartiError::Html404);
    }

    Ok(response::text(
        StatusCode::OK,
        content_url(&request, &context, &resource.hash),
    ))
}

/// `GET|POST|DELETE /Marti/sync/delete` — all three verbs, as TAK serves them.
///
/// # Errors
///
/// [`MartiError::InvalidRequest`] for a `PrimaryKey` that is not a number, and
/// [`MartiError::Forbidden`] for a resource the caller did not submit.
#[instrument("marti.sync.delete", skip_all)]
pub async fn delete(
    who: MartiPrincipal,
    query: CiQuery,
    context: web::Data<AppContext>,
) -> MartiResult {
    let viewer = viewer(&who, &context).await?;

    let doomed: Vec<ResourceRow> = match query.get("hash") {
        Some(hash) => {
            context
                .db()
                .resources()
                .list(ResourceFilter::by_hash(hash))
                .await?
        }
        None => {
            let mut found = Vec::new();

            for id in query.list::<i64>("primarykey")?.into_inner() {
                found.extend(context.db().resources().by_id(id).await?);
            }

            found
        }
    };

    for resource in &doomed {
        if !viewer.can_write(resource) {
            return Err(MartiError::Forbidden(
                "only the person who uploaded a file may remove it".to_string(),
            ));
        }
    }

    let removed = remove(&context, &who, doomed).await?;

    Ok(HttpResponse::build(StatusCode::OK)
        .insert_header((CONTENT_TYPE, response::HTML))
        .body(format!("{DELETE_PREFIX}{removed}{DELETE_SUFFIX}")))
}

/// Removes rows and then the bytes nothing else wants.
///
/// Shared with `/Marti/api/files/{hash}`, which deletes the same way.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if the database or the store fails.
pub async fn remove(
    context: &AppContext,
    who: &MartiPrincipal,
    doomed: Vec<ResourceRow>,
) -> Result<usize, Error> {
    let ids: Vec<i64> = doomed.iter().map(|resource| resource.id).collect();
    let removed = context.db().resources().delete(ids).await?;

    for resource in &doomed {
        upload::audit(context, "removed", who.username(), resource).await;

        if let Err(err) = store::forget(context, &resource.hash).await {
            warn!(error = %err, "Could not remove a stored file whose last row went.");
        }
    }

    Ok(removed)
}

/// The URL a client hands to its peers, per design decision D12.
///
/// Built from `[marti] public_host` when an operator set one, because the URL
/// travels: ATAK puts it in the `senderUrl` of the file-share message it sends
/// next, and a peer behind a different NAT has to be able to resolve it.
///
/// Everything below that is [`public_base_url`], which reads the authority an
/// h2 client sent and falls back to the configured URL rather than to
/// `[server] name` — a display name, which is what CI-01 saw a peer handed.
///
/// [`public_base_url`]: crate::web::helpers::request::public_base_url
pub fn content_url(request: &HttpRequest, context: &AppContext, hash: &str) -> String {
    let config = context.config();
    let base = match &config.marti.public_host {
        Some(host) => format!("https://{host}"),
        None => crate::web::helpers::request::public_base_url(&config.server, request),
    };

    format!("{base}/Marti/sync/content?hash={hash}")
}

/// The resource a `hash` or `uid` parameter addresses.
async fn addressed(context: &AppContext, query: &CiQuery) -> Result<ResourceRow, MartiError> {
    // `Hash` wins over `uid` when both are given, which is TAK's own precedence.
    let found = match query.get("hash") {
        Some(hash) => context.db().resources().by_hash(hash).await?,
        None => match query.get("uid") {
            Some(uid) => context.db().resources().by_uid(uid).await?,
            None => None,
        },
    };

    found.ok_or(MartiError::Html404)
}

/// The `offset`/`length` parameters, or the `Range` header they stand in for.
///
/// The third value says which of the two it was, because the two answer
/// different statuses for the same bytes: a real `Range:` request that did not
/// ask for the whole file is a `206`, while `?offset=` is TAK's own resume
/// parameter and is a `200` whenever the range runs to the end of the file.
fn range(request: &HttpRequest, query: &CiQuery) -> Result<(u64, Option<u64>, bool), MartiError> {
    if let Some(offset) = query.parsed::<u64>("offset")? {
        return Ok((
            offset,
            query.parsed::<u64>("length")?.filter(|n| *n > 0),
            false,
        ));
    }

    let Some(header) = request
        .headers()
        .get(actix_web::http::header::RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().strip_prefix("bytes="))
    else {
        return Ok((0, query.parsed::<u64>("length")?.filter(|n| *n > 0), false));
    };

    // Only the first range of a possibly multi-range header, and only the
    // `start-[end]` form: a suffix range would need the length before the file
    // is open, and no TAK client sends one.
    let (start, end) = header.split_once('-').unwrap_or((header, ""));
    let start: u64 = start.trim().parse().unwrap_or(0);
    let length = end
        .trim()
        .parse::<u64>()
        .ok()
        .map(|end| end.saturating_sub(start) + 1);

    Ok((start, length, true))
}

/// Percent-encodes a filename for a `Content-Disposition` header.
///
/// Conservative on purpose: a quote or a newline in a stored name would let a
/// client's own filename close the header and add another.
pub fn urlencode(name: &str) -> String {
    name.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' | b'~' => {
                (byte as char).to_string()
            }
            b' ' => "%20".to_string(),
            other => format!("%{other:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filename_cannot_break_out_of_the_disposition_header() {
        assert_eq!(urlencode("package.zip"), "package.zip");
        assert_eq!(urlencode("my report.zip"), "my%20report.zip");
        assert_eq!(
            urlencode("a\"; filename=\"b"),
            "a%22%3B%20filename%3D%22b",
            "a quote in a stored name would otherwise add a second header value",
        );
    }

    #[test]
    fn the_delete_document_is_the_one_legacy_tooling_reads() {
        let body = format!("{DELETE_PREFIX}2{DELETE_SUFFIX}");

        assert_eq!(
            body,
            "<html><head><title>Enterprise Sync Status</title></head>\
             <h1>Success</h1><p>Deleted 2 resource(s).</p></html>",
        );
    }

    #[test]
    fn a_range_is_read_from_the_parameters_first_and_the_header_second() {
        let parameters = actix_web::test::TestRequest::default().to_http_request();
        let query = CiQuery::parse("offset=3&length=4");

        assert_eq!(
            range(&parameters, &query).unwrap(),
            (3, Some(4), false),
            "`?offset=` is TAK's own resume parameter, not a `Range:` request",
        );

        let header = actix_web::test::TestRequest::default()
            .insert_header((actix_web::http::header::RANGE, "bytes=10-19"))
            .to_http_request();

        assert_eq!(
            range(&header, &CiQuery::default()).unwrap(),
            (10, Some(10), true),
            "an inclusive end becomes a length",
        );
        assert_eq!(
            range(&parameters, &CiQuery::default()).unwrap(),
            (0, None, false),
            "no range at all is the whole file",
        );
    }
}
