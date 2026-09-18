//! `/Marti/api/files/*` — the modern file-manager surface.
//!
//! # Why the values are display strings
//!
//! Every element of `/metadata` is a flat map of strings whose `Size` is
//! humanised (`"12kB"`) and whose `Time` is a Java `Date.toString()`. That is a
//! screen, not a schema — but CloudTAK reads `Hash`, `Groups` and `Time` out of
//! the same map to fill in a package's channels column, so the keys are
//! contractual even though the values are for people. See
//! [`crate::files::legacy`] for the renderings.
//!
//! # Where this differs from the legacy servlets
//!
//! These routes are enveloped and their refusals are the ordinary JSON ones,
//! because they are a Spring controller upstream rather than a servlet. Only
//! `/Marti/sync/*` answers HTML.
//!
//! `DELETE` is the one place we are deliberately stricter than TAK, which
//! swallows every exception and always answers `200`: deleting somebody else's
//! upload is a `403` here, so a client that tried finds out.

use actix_web::http::StatusCode;
use actix_web::http::header::{CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE};
use actix_web::{HttpResponse, web};

use crate::db::repos::{ResourceFilter, ResourceRow};
use crate::files::store;
use crate::files::upload;
use crate::files::{legacy, metadata};
use crate::prelude::*;

use super::error::{MartiError, MartiResult};
use super::extract::CiQuery;
use super::principal::MartiPrincipal;
use super::sync_read;
use super::{response, response::kind};

/// Registers the file-manager routes inside the `/Marti/api` scope.
///
/// The two `metadata` literals come before `{hash}`, which would otherwise
/// match them, and `{hash}/metadata` before the bare `{hash}`.
pub fn routes(config: &mut web::ServiceConfig) {
    config
        .route("/files/metadata/count", web::get().to(count))
        .route("/files/metadata", web::get().to(listing))
        .route("/files/{hash}/metadata", web::put().to(update_metadata))
        .route("/files/{hash}", web::get().to(download))
        .route("/files/{hash}", web::head().to(describe))
        .route("/files/{hash}", web::delete().to(remove));
}

/// `GET /Marti/api/files/metadata` — the file manager's listing.
///
/// # Errors
///
/// [`MartiError::InvalidRequest`] for a page or limit that will not parse.
#[instrument("marti.files.metadata", skip_all)]
pub async fn listing(
    who: MartiPrincipal,
    query: CiQuery,
    context: web::Data<AppContext>,
) -> MartiResult {
    let viewer = viewer(&who, &context).await?;
    let found = viewer.filter(context.db().resources().list(filter(&query)?).await?);
    let data: Vec<_> = found.iter().map(legacy::files_entry).collect();

    Ok(response::ok(kind::FILES, data))
}

/// `GET /Marti/api/files/metadata/count` — how many the listing would hold.
///
/// # Errors
///
/// As [`listing`].
#[instrument("marti.files.count", skip_all)]
pub async fn count(
    who: MartiPrincipal,
    query: CiQuery,
    context: web::Data<AppContext>,
) -> MartiResult {
    let viewer = viewer(&who, &context).await?;
    // Counted after the visibility rule rather than in SQL, so that the number
    // matches the listing a client is about to page through.
    let found = viewer.filter(
        context
            .db()
            .resources()
            .list(ResourceFilter {
                limit: None,
                offset: None,
                ..filter(&query)?
            })
            .await?,
    );

    Ok(response::ok(kind::COUNT, found.len()))
}

/// `GET /Marti/api/files/{hash}` — the bytes, as an attachment.
///
/// # Errors
///
/// [`MartiError::NotFound`] when nothing readable holds that hash.
#[instrument("marti.files.download", skip_all)]
pub async fn download(
    who: MartiPrincipal,
    path: web::Path<String>,
    context: web::Data<AppContext>,
) -> MartiResult {
    let hash = path.into_inner();
    let resource = readable(&who, &context, &hash).await?;
    let content = context.content()?;
    let opened = store::open_range(&content, &resource.hash, 0, None).await?;

    Ok(HttpResponse::build(StatusCode::OK)
        .insert_header((CONTENT_TYPE, resource.mime_type.clone()))
        .insert_header((
            CONTENT_DISPOSITION,
            format!(
                "attachment; filename={}",
                sync_read::urlencode(&resource.name)
            ),
        ))
        .insert_header((CONTENT_LENGTH, opened.length.to_string()))
        .streaming(store::chunks(opened.file, opened.length)))
}

/// `HEAD /Marti/api/files/{hash}` — the same map without the channels.
///
/// # Errors
///
/// As [`download`].
#[instrument("marti.files.describe", skip_all)]
pub async fn describe(
    who: MartiPrincipal,
    path: web::Path<String>,
    context: web::Data<AppContext>,
) -> MartiResult {
    let resource = readable(&who, &context, &path.into_inner()).await?;

    Ok(response::ok(kind::DATA, legacy::metadata_entry(&resource)))
}

/// `DELETE /Marti/api/files/{hash}` — remove every row holding it.
///
/// # Errors
///
/// [`MartiError::NotFound`] when nothing holds that hash, and
/// [`MartiError::Forbidden`] when the caller did not submit it.
#[instrument("marti.files.delete", skip_all)]
pub async fn remove(
    who: MartiPrincipal,
    path: web::Path<String>,
    context: web::Data<AppContext>,
) -> MartiResult {
    let hash = path.into_inner();
    let viewer = viewer(&who, &context).await?;
    let doomed = context
        .db()
        .resources()
        .list(ResourceFilter::by_hash(&hash))
        .await?;

    if doomed.is_empty() {
        return Err(MartiError::NotFound(hash));
    }

    for resource in &doomed {
        if !viewer.can_write(resource) {
            return Err(MartiError::Forbidden(
                "only the person who uploaded a file may remove it".to_string(),
            ));
        }
    }

    sync_read::remove(&context, &who, doomed).await?;

    Ok(response::status::<()>(StatusCode::OK, kind::DATA, None))
}

/// `PUT /Marti/api/files/{hash}/metadata?user&expiration&keywords` — re-own it.
///
/// # Errors
///
/// As [`remove`], plus [`MartiError::InvalidRequest`] for an expiry that is not
/// a number.
#[instrument("marti.files.update_metadata", skip_all)]
pub async fn update_metadata(
    who: MartiPrincipal,
    path: web::Path<String>,
    query: CiQuery,
    context: web::Data<AppContext>,
) -> MartiResult {
    let hash = path.into_inner();
    let resource = super::sync_metadata::writable(&who, &context, &hash).await?;
    let resources = context.db().resources();

    if let Some(user) = query.get("user").filter(|user| !user.is_empty()) {
        resources.set_submitter(&hash, user.to_string()).await?;
    }

    if let Some(at) = query.parsed::<i64>("expiration")? {
        resources
            .set_expiration(&hash, (at >= 0).then_some(at))
            .await?;
    }

    let keywords = query.strings("keywords");
    if !keywords.is_empty() {
        resources.set_keywords(&hash, keywords).await?;
    }

    upload::audit(&**context, "changed", who.username(), &resource).await;

    Ok(response::status::<()>(StatusCode::OK, kind::DATA, None))
}

/// The listing filter `page`, `limit`, `mission`, `missionPackage`, `name`,
/// `sort` and `ascending` add up to.
///
/// `page` and `limit` are `-1` for "unpaged", which is what every client sends
/// and what TAK's own defaults are.
fn filter(query: &CiQuery) -> Result<ResourceFilter, MartiError> {
    let limit = query.parsed::<i64>("limit")?.unwrap_or(-1);
    let page = query.parsed::<i64>("page")?.unwrap_or(-1);
    let limit = u32::try_from(limit).ok().filter(|limit| *limit > 0);

    Ok(ResourceFilter {
        name: query
            .get("name")
            .filter(|name| !name.is_empty())
            .map(str::to_string),
        mission_name: query
            .get("mission")
            .filter(|mission| !mission.is_empty())
            .map(str::to_string),
        keywords: if query.flag("missionpackage").get() {
            vec![upload::MISSION_PACKAGE.to_string()]
        } else {
            Vec::new()
        },
        limit,
        offset: limit.and_then(|limit| {
            u32::try_from(page.max(0))
                .ok()
                .map(|page| page.saturating_mul(limit))
        }),
        // `sort` names a column upstream; every client that sends one sends
        // the submission time, which is the order this listing is already in.
        ascending: query.flag("ascending").get() && query.has("ascending"),
        ..ResourceFilter::default()
    })
}

/// The resource a caller may see, or the reason they may not.
async fn readable(
    who: &MartiPrincipal,
    context: &AppContext,
    hash: &str,
) -> Result<ResourceRow, MartiError> {
    let viewer = viewer(who, context).await?;
    let Some(resource) = context.db().resources().by_hash(hash).await? else {
        return Err(MartiError::NotFound(hash.to_string()));
    };

    if !viewer.can_read(&resource) {
        return Err(MartiError::NotFound(hash.to_string()));
    }

    Ok(resource)
}

/// The caller's channels, for the visibility rule.
async fn viewer(
    who: &MartiPrincipal,
    context: &AppContext,
) -> Result<metadata::Viewer, MartiError> {
    Ok(metadata::viewer_for(context.db(), who.username(), who.principal()).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter_of(raw: &str) -> ResourceFilter {
        filter(&CiQuery::parse(raw)).expect("a well formed query")
    }

    #[test]
    fn the_unpaged_defaults_every_client_sends_are_unpaged() {
        let unpaged = filter_of("page=-1&limit=-1");

        assert_eq!(unpaged.limit, None);
        assert_eq!(unpaged.offset, None);
        assert_eq!(filter_of("").limit, None);
    }

    #[test]
    fn a_page_is_turned_into_an_offset() {
        let second = filter_of("page=2&limit=10");

        assert_eq!(second.limit, Some(10));
        assert_eq!(second.offset, Some(20));
    }

    #[test]
    fn the_package_flag_becomes_the_keyword_that_marks_one() {
        assert_eq!(
            filter_of("missionPackage=true").keywords,
            vec![upload::MISSION_PACKAGE.to_string()],
        );
        assert!(filter_of("missionPackage=false").keywords.is_empty());
    }

    #[test]
    fn an_empty_name_or_mission_is_not_a_filter() {
        // CloudTAK sends `name=` and `mission=` unconditionally; treating the
        // empty string as a value would answer every listing with nothing.
        let empty = filter_of("name=&mission=");

        assert_eq!(empty.name, None);
        assert_eq!(empty.mission_name, None);
        assert_eq!(filter_of("name=pkg.zip").name.as_deref(), Some("pkg.zip"));
    }

    #[test]
    fn the_listing_is_newest_first_unless_a_client_asked_otherwise() {
        assert!(!filter_of("").ascending);
        assert!(filter_of("ascending=true").ascending);
        assert!(!filter_of("ascending=false").ascending);
    }
}
