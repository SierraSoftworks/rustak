//! `/Marti/api/sync/*` — changing a resource's metadata, and the modern search.
//!
//! # Four fields, and only four
//!
//! `tool`, `mimetype`, `keywords` and `expiration` are the whole of what this
//! API may change. Not `name`, not `creatorUid`, not `groups`: a client that
//! could rewrite those could move somebody else's upload into a channel they
//! chose, or re-own it. TAK refuses any other path segment with a `400` and so
//! do we — the segment is compared case-insensitively, because ATAK sends
//! `tool` and the browser page sends `Tool`.
//!
//! The three mutating routes answer an **empty `200`** on success and a `404`
//! when the hash matches nothing, with no body either way. node-tak's
//! `Files.update` ignores the body entirely and branches on the status.
//!
//! # `/Marti/api/sync/search` is not `/Marti/sync/search`
//!
//! Same word, different endpoint: this one is enveloped, its keys are
//! lowerCamelCase and its `size` is a number. It is the route node-tak's
//! `Files.list()` calls and the one the interop suite probes to decide whether
//! this milestone has landed, so its shape is checked by the schema in that
//! suite as well as here.

use actix_web::http::StatusCode;
use actix_web::web;

use crate::db::repos::MutableField;
use crate::files::upload;
use crate::files::{metadata, search};
use crate::prelude::*;

use super::error::{MartiError, MartiResult};
use super::extract::CiQuery;
use super::principal::MartiPrincipal;
use super::{response, response::kind};

/// Registers the metadata routes inside the `/Marti/api` scope.
///
/// `search` is registered before `metadata/{hash}/{field}` so that the literal
/// cannot be read as a hash, and the two dedicated field routes before the
/// general one they would otherwise fall into.
pub fn routes(config: &mut web::ServiceConfig) {
    config
        .route("/sync/search", web::get().to(search_resources))
        .route(
            "/sync/metadata/{hash}/keywords",
            web::put().to(set_keywords),
        )
        .route(
            "/sync/metadata/{hash}/expiration",
            web::put().to(set_expiration),
        )
        .route("/sync/metadata/{hash}/{field}", web::put().to(set_field));
}

/// `GET /Marti/api/sync/search` — the enveloped `Resource` listing.
///
/// # Errors
///
/// [`MartiError::InvalidRequest`] for a parameter that will not parse.
#[instrument("marti.sync.api_search", skip_all)]
pub async fn search_resources(
    who: MartiPrincipal,
    query: CiQuery,
    context: web::Data<AppContext>,
) -> MartiResult {
    let viewer = metadata::viewer_for(context.db(), who.username(), who.principal()).await?;
    let found = search::run(context.db(), &viewer, search::SearchQuery::parse(&query)?).await?;
    let data: Vec<metadata::ResourceJson> = found.iter().map(metadata::resource_json).collect();

    Ok(response::ok(kind::RESOURCE, data))
}

/// `PUT /Marti/api/sync/metadata/{hash}/{tool|mimetype}` — the raw text value.
///
/// # Errors
///
/// [`MartiError::InvalidRequest`] for any other field name,
/// [`MartiError::NotFound`] when no resource holds that hash, and
/// [`MartiError::Forbidden`] when the caller did not submit it.
#[instrument("marti.sync.metadata.field", skip_all)]
pub async fn set_field(
    who: MartiPrincipal,
    path: web::Path<(String, String)>,
    body: web::Bytes,
    context: web::Data<AppContext>,
) -> MartiResult {
    let (hash, field) = path.into_inner();
    let Some(field) = MutableField::parse(&field) else {
        return Err(MartiError::InvalidRequest(format!(
            "{field} is not a metadata field this server changes"
        )));
    };

    let resource = writable(&who, &context, &hash).await?;
    let value = String::from_utf8_lossy(&body).trim().to_string();

    if value.is_empty() {
        return Err(MartiError::InvalidRequest(
            "a value is required".to_string(),
        ));
    }

    context
        .db()
        .resources()
        .set_field(&hash, field, value)
        .await?;
    upload::audit(&**context, "changed", who.username(), &resource).await;

    Ok(response::text(StatusCode::OK, ""))
}

/// `PUT /Marti/api/sync/metadata/{hash}/keywords` — a JSON array of strings.
///
/// # Errors
///
/// [`MartiError::InvalidBody`] when the body is not an array of strings, and
/// as [`set_field`] otherwise.
#[instrument("marti.sync.metadata.keywords", skip_all)]
pub async fn set_keywords(
    who: MartiPrincipal,
    path: web::Path<String>,
    body: web::Bytes,
    context: web::Data<AppContext>,
) -> MartiResult {
    let hash = path.into_inner();
    let resource = writable(&who, &context, &hash).await?;
    let keywords: Vec<String> = serde_json::from_slice(&body)?;

    context
        .db()
        .resources()
        .set_keywords(&hash, keywords)
        .await?;
    upload::audit(&**context, "changed", who.username(), &resource).await;

    Ok(response::text(StatusCode::OK, ""))
}

/// `PUT /Marti/api/sync/metadata/{hash}/expiration?expiration=` — epoch millis.
///
/// # Errors
///
/// [`MartiError::InvalidRequest`] when the parameter is missing or not a
/// number, and as [`set_field`] otherwise.
#[instrument("marti.sync.metadata.expiration", skip_all)]
pub async fn set_expiration(
    who: MartiPrincipal,
    path: web::Path<String>,
    query: CiQuery,
    context: web::Data<AppContext>,
) -> MartiResult {
    let hash = path.into_inner();
    let Some(at) = query.parsed::<i64>("expiration")? else {
        return Err(MartiError::InvalidRequest(
            "expiration is required".to_string(),
        ));
    };

    let resource = writable(&who, &context, &hash).await?;

    // A negative value is TAK's "never", which the column stores as NULL.
    context
        .db()
        .resources()
        .set_expiration(&hash, (at >= 0).then_some(at))
        .await?;
    upload::audit(&**context, "changed", who.username(), &resource).await;

    Ok(response::text(StatusCode::OK, ""))
}

/// The resource a caller may change, or the reason they may not.
///
/// # Errors
///
/// [`MartiError::NotFound`] when the hash matches nothing and
/// [`MartiError::Forbidden`] when it matches something somebody else uploaded.
pub async fn writable(
    who: &MartiPrincipal,
    context: &AppContext,
    hash: &str,
) -> Result<crate::db::repos::ResourceRow, MartiError> {
    let viewer = metadata::viewer_for(context.db(), who.username(), who.principal()).await?;
    let Some(resource) = context.db().resources().by_hash(hash).await? else {
        return Err(MartiError::NotFound(hash.to_string()));
    };

    if !viewer.can_write(&resource) {
        return Err(MartiError::Forbidden(
            "only the person who uploaded a file may change it".to_string(),
        ));
    }

    Ok(resource)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_field_names_clients_send_are_read_in_any_case() {
        for spelling in ["tool", "Tool", "TOOL"] {
            assert_eq!(MutableField::parse(spelling), Some(MutableField::Tool));
        }

        for refused in ["name", "creatorUid", "groups", "hash", ""] {
            assert_eq!(
                MutableField::parse(refused),
                None,
                "{refused} must not be changeable through this API",
            );
        }
    }
}
