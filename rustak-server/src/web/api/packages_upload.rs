//! `POST /api/v1/packages`: the streaming half of the package API.
//!
//! Split from [`super::packages`] because an upload is a different shape of
//! problem from a listing: it walks a multipart body, hands one part straight
//! to the content store and never holds a whole file. Keeping the two in one
//! file would have put the reading of a form and the reading of four hundred
//! megabytes side by side under one set of imports.
//!
//! # The file part is streamed, and the rest of the form is not
//!
//! `file` goes to [`files::store::ingest`](crate::files::store::ingest) as it
//! arrives, hashed on the way; `name`, `tool`, `keywords` and `groups` are
//! short text fields and are read into memory with a ceiling, because a form
//! field big enough to matter is a client doing something else.
//!
//! Fields may arrive in any order, so the parts are walked to the end whatever
//! comes first — a browser's `FormData` puts the file last as often as not.
//!
//! # The ceiling is checked twice
//!
//! Once against `Content-Length`, because refusing before the upload starts is
//! much kinder, and once inside the reader, because a chunked body carries no
//! length and one that does can lie.

use actix_multipart::{Field, Multipart};
use actix_web::http::header::CONTENT_LENGTH;
use actix_web::{HttpRequest, web};
use futures::TryStreamExt as _;

use crate::files::store::{IngestError, Ingested};
use crate::files::upload::{self, Upload};
use crate::prelude::*;

use super::error::{ApiError, ApiResult};
use super::extract::Administrative;
use super::packages::{created, row, viewer};
use super::subject::failed;

/// The most one non-file form field may carry.
///
/// A name, a tool or a channel list is tens of bytes; a kilobyte of it is a
/// client sending something this endpoint did not ask for.
const MAX_FIELD_BYTES: usize = 8 * 1024;

/// `POST /api/v1/packages` — `multipart/form-data`.
///
/// Parts: `file` (required), `name`, `tool`, and `keywords`/`groups` repeated
/// once per value or given as one comma-separated field.
///
/// # Errors
///
/// A `400` for a body that is not multipart, carries no file, is empty or is
/// over the configured ceiling, and a `500` when the store or the database
/// fails.
#[instrument("api.packages.upload", skip_all)]
pub async fn create(
    request: HttpRequest,
    payload: Multipart,
    context: web::Data<AppContext>,
    caller: Administrative,
) -> ApiResult {
    // The resolved ceiling, which is the same number `/files/api/config` tells
    // a client and the same one `/Marti/sync/upload` enforces.
    let limit_mb = crate::files::limits::limit_mb(&context.config(), context.db())
        .await
        .map_err(|err| failed(&context, &err))?;
    let limit = u64::from(limit_mb) * 1_000_000;

    if claimed_length(&request).is_some_and(|claimed| claimed > limit) {
        return Err(too_large(limit_mb));
    }

    let form = read(payload, &context, limit, limit_mb).await?;
    let Some(stored) = form.stored else {
        return Err(ApiError::bad_request("That upload carried no 'file' part."));
    };

    // An administrator may address any channel, so there is no refusal here —
    // only the rule every other upload path shares: with no channels named, an
    // upload inherits the caller's, which is what makes it visible to the
    // people they work with rather than to nobody.
    let groups = match form.groups.is_empty() {
        true => viewer(&context, &caller).await?.held_groups,
        false => form.groups.clone(),
    };

    let upload = Upload {
        name: form.name.clone(),
        mime_type: form.mime_type.clone().or_else(|| {
            Some(match stored.is_zip {
                true => upload::PACKAGE_MIME.to_string(),
                false => upload::DEFAULT_MIME.to_string(),
            })
        }),
        keywords: form.keywords.clone(),
        groups: form.groups.clone(),
        tool: form.tool.clone(),
        ..Upload::default()
    };

    let stored_row = context
        .db()
        .resources()
        .upsert(upload.into_resource(
            &stored.ingested,
            Some((caller.user.username.as_str(), caller.user.id)),
            form.filename.clone(),
            groups,
        ))
        .await
        .map_err(|err| failed(&context, &err))?;

    upload::audit(
        &**context,
        "uploaded",
        Some(caller.user.username.as_str()),
        &stored_row,
    )
    .await;

    // Read back so that the answer carries the keywords as they were stored.
    let described = row(&context, &stored_row.hash).await?;

    Ok(created(&described))
}

/// What a stored part turned out to be.
struct StoredPart {
    ingested: Ingested,
    /// Whether the part declared itself a zip, which decides the default
    /// content type when the form named none.
    is_zip: bool,
}

/// What the whole form carried.
#[derive(Default)]
struct Form {
    stored: Option<StoredPart>,
    /// Whether the part being read looked like a zip.
    pending_zip: bool,
    filename: Option<String>,
    mime_type: Option<String>,
    name: Option<String>,
    tool: Option<String>,
    keywords: Vec<String>,
    groups: Vec<String>,
}

/// Walks the multipart body, streaming the file and collecting the rest.
async fn read(
    mut payload: Multipart,
    context: &web::Data<AppContext>,
    limit: u64,
    limit_mb: u32,
) -> Result<Form, ApiError> {
    let mut form = Form::default();

    while let Some(field) = payload.try_next().await.map_err(malformed)? {
        let name = field.name().unwrap_or_default().to_string();
        let filename = upload::part_filename(&field);

        // A part with a filename is the file however it was named, which is
        // what a form built by hand and a form built by a browser agree on.
        if form.stored.is_none() && (name == "file" || filename.is_some()) {
            let declared = upload::part_content_type(&field);
            form.is_zip_from(declared.as_deref(), filename.as_deref());
            form.filename = filename;
            form.mime_type = declared.filter(|value| value != "application/octet-stream");

            let is_zip = form.pending_zip;
            let ingested = ingest(field, context, limit, limit_mb).await?;
            form.stored = Some(StoredPart { ingested, is_zip });

            continue;
        }

        let value = text(field).await?;

        match name.as_str() {
            "name" => form.name = non_empty(value),
            "tool" => form.tool = non_empty(value),
            "keywords" | "keywords[]" => form.keywords.extend(split(&value)),
            "groups" | "groups[]" => form.groups.extend(split(&value)),
            _ => debug!(part = %name, "Ignoring an unexpected part of a package upload."),
        }
    }

    Ok(form)
}

impl Form {
    /// Remembers whether the file part looked like a zip.
    fn is_zip_from(&mut self, declared: Option<&str>, filename: Option<&str>) {
        self.pending_zip = declared.is_some_and(|value| value.contains("zip"))
            || filename.is_some_and(|name| name.to_lowercase().ends_with(".zip"));
    }
}

/// Streams one part into the content store.
async fn ingest(
    field: Field,
    context: &web::Data<AppContext>,
    limit: u64,
    limit_mb: u32,
) -> Result<Ingested, ApiError> {
    let store = context.content().map_err(|err| failed(context, &err))?;

    match crate::files::ingest(&store, field.map_err(io_error), limit).await {
        Ok(stored) => Ok(stored),
        Err(IngestError::TooLarge) => Err(too_large(limit_mb)),
        Err(IngestError::Empty) => Err(ApiError::bad_request("That upload had no content.")),
        Err(IngestError::Failed(err)) => Err(failed(context, &err)),
    }
}

/// Reads one short text field, refusing one that is not short.
async fn text(mut field: Field) -> Result<String, ApiError> {
    let mut data = Vec::new();

    while let Some(chunk) = field.try_next().await.map_err(malformed)? {
        if data.len() + chunk.len() > MAX_FIELD_BYTES {
            return Err(ApiError::bad_request(
                "One of that upload's form fields is far larger than it should be.",
            ));
        }

        data.extend_from_slice(&chunk);
    }

    String::from_utf8(data)
        .map(|value| value.trim().to_string())
        .map_err(|_| ApiError::bad_request("One of that upload's form fields is not text."))
}

/// A repeated field's value, which may itself be a comma-separated list.
fn split(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

/// `None` for a field somebody left blank.
fn non_empty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

/// The size the client claimed, checked before a byte is read.
fn claimed_length(request: &HttpRequest) -> Option<u64> {
    request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse().ok())
}

/// What an upload past the ceiling is refused with.
fn too_large(limit_mb: u32) -> ApiError {
    ApiError::bad_request(format!(
        "That file is larger than this server's {limit_mb} MB upload limit."
    ))
}

/// What a body we could not parse as multipart is refused with.
fn malformed(err: actix_multipart::MultipartError) -> ApiError {
    debug!(error = %err, "Refused a package upload we could not read.");

    ApiError::bad_request(
        "A package upload must be a multipart/form-data POST with a part named 'file'.",
    )
}

/// The stream adapter `ingest` takes.
fn io_error(err: actix_multipart::MultipartError) -> std::io::Error {
    std::io::Error::other(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repeated_field_is_read_in_either_spelling() {
        assert_eq!(split("Blue,Red"), vec!["Blue", "Red"]);
        assert_eq!(split(" Blue , "), vec!["Blue"]);
        assert!(split("").is_empty());
    }

    #[test]
    fn a_blank_field_is_the_same_as_an_absent_one() {
        assert_eq!(non_empty(String::new()), None);
        assert_eq!(
            non_empty("patrol.zip".to_string()).as_deref(),
            Some("patrol.zip")
        );
    }

    #[test]
    fn a_zip_is_recognised_from_either_the_type_or_the_name() {
        let mut form = Form::default();

        form.is_zip_from(Some("application/x-zip-compressed"), None);
        assert!(form.pending_zip);

        form.is_zip_from(None, Some("Patrol.ZIP"));
        assert!(
            form.pending_zip,
            "a filename is matched without regard to case"
        );

        form.is_zip_from(Some("text/plain"), Some("notes.txt"));
        assert!(!form.pending_zip);
    }
}
