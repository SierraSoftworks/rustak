//! `/api/v1/profiles/{id}/files`: the files a profile delivers beside its
//! preferences.
//!
//! Map sources, overlays, a `.pref` an operator wrote by hand — anything small
//! enough to be configuration. A data package belongs in Enterprise Sync, which
//! streams; these are read whole into memory, so the size limit below is the
//! line between the two.
//!
//! # A filename is a delivered path
//!
//! It decides where the file lands on the device and what a `relativePath`
//! query matches, so it is sanitised on the way in: a name that could climb out
//! of the profile is refused here rather than filtered at every reader.

use actix_multipart::Multipart;
use actix_web::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use actix_web::{HttpResponse, web};
use futures::TryStreamExt as _;
use rustak_api::{ProfileFile, ProfileId};
use tokio::io::AsyncReadExt as _;

use crate::prelude::*;
use crate::profiles::builder::guess_content_type;
use crate::profiles::repo::ProfilesRepo;

use super::error::{ApiError, ApiResult, json_ok};
use super::extract::Administrative;
use super::profiles::{record, row};
use super::subject::failed;

/// How large a single profile file may be.
///
/// A profile file is a preference document, a map source or a small overlay —
/// a data package belongs in Enterprise Sync, which streams. This is generous
/// for the former and refuses the latter before it reaches memory.
const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;

/// `GET /api/v1/profiles/{id}/files`.
///
/// # Errors
///
/// A `404`, and a `500` when a read fails.
pub async fn list(
    context: web::Data<AppContext>,
    id: web::Path<ProfileId>,
    _: Administrative,
) -> ApiResult {
    row(&context, *id).await?;

    let listed: Vec<ProfileFile> = ProfilesRepo::new(context.db())
        .files(*id)
        .await
        .map_err(|err| failed(&context, &err))?
        .into_iter()
        .map(|file| ProfileFile {
            id: file.id,
            name: file.path,
            size: file.size,
            mime_type: file.mime_type,
            updated: file.updated_at,
        })
        .collect();

    Ok(json_ok(&listed))
}

/// `POST /api/v1/profiles/{id}/files` — `multipart/form-data`, part `file`.
///
/// # Errors
///
/// A `400` for a malformed body, a missing filename or a file over the limit;
/// a `404`; a `500` when the write fails.
pub async fn upload(
    context: web::Data<AppContext>,
    id: web::Path<ProfileId>,
    payload: Multipart,
    caller: Administrative,
) -> ApiResult {
    let row = row(&context, *id).await?;
    let upload = read_upload(payload).await?;

    let stored = context
        .content()
        .map_err(|err| failed(&context, &err))?
        .put_bytes(&upload.data)
        .await
        .map_err(|err| failed(&context, &err))?;

    let file = ProfilesRepo::new(context.db())
        .put_file(
            *id,
            upload.name.clone(),
            stored.hash,
            stored.size,
            upload.mime_type,
        )
        .await
        .map_err(|err| failed(&context, &err))?;

    record(&context, "profile.file.added", &caller, &row.name).await;

    Ok(json_ok(&ProfileFile {
        id: file.id,
        name: file.path,
        size: file.size,
        mime_type: file.mime_type,
        updated: file.updated_at,
    }))
}

/// `GET /api/v1/profiles/{id}/files/{file}`.
///
/// # Errors
///
/// A `404`, and a `500` when the content store cannot be read.
pub async fn download(
    context: web::Data<AppContext>,
    path: web::Path<(ProfileId, i64)>,
    _: Administrative,
) -> ApiResult {
    let (id, file_id) = path.into_inner();
    let file = ProfilesRepo::new(context.db())
        .file(file_id)
        .await
        .map_err(|err| failed(&context, &err))?
        .filter(|file| file.profile_id == id)
        .ok_or_else(|| ApiError::not_found("There is no such file on that profile."))?;

    let mut handle = context
        .content()
        .map_err(|err| failed(&context, &err))?
        .open(&file.hash)
        .await
        .map_err(|err| failed(&context, &err))?;

    let mut data = Vec::with_capacity(file.size as usize);
    handle
        .read_to_end(&mut data)
        .await
        .map_err(|_| ApiError::not_found("That file is no longer stored on this server."))?;

    let name = file.path.rsplit('/').next().unwrap_or("file").to_string();

    Ok(HttpResponse::Ok()
        .insert_header((
            CONTENT_TYPE,
            file.mime_type
                .unwrap_or_else(|| guess_content_type(&file.path).to_string()),
        ))
        .insert_header((
            CONTENT_DISPOSITION,
            format!("attachment; filename=\"{name}\""),
        ))
        .body(data))
}

/// `DELETE /api/v1/profiles/{id}/files/{file}`.
///
/// # Errors
///
/// A `404`, and a `500` when the write fails.
pub async fn remove(
    context: web::Data<AppContext>,
    path: web::Path<(ProfileId, i64)>,
    caller: Administrative,
) -> ApiResult {
    let (id, file_id) = path.into_inner();
    let row = row(&context, id).await?;

    if !ProfilesRepo::new(context.db())
        .delete_file(id, file_id)
        .await
        .map_err(|err| failed(&context, &err))?
    {
        return Err(ApiError::not_found(
            "There is no such file on that profile.",
        ));
    }

    record(&context, "profile.file.removed", &caller, &row.name).await;

    Ok(HttpResponse::NoContent().finish())
}

/// What arrived in a multipart upload.
struct Upload {
    name: String,
    data: Vec<u8>,
    mime_type: Option<String>,
}

/// Reads the `file` part and an optional `filename` field.
async fn read_upload(mut payload: Multipart) -> Result<Upload, ApiError> {
    let mut upload: Option<Upload> = None;
    let mut override_name: Option<String> = None;

    while let Some(mut field) = payload
        .try_next()
        .await
        .map_err(|err| ApiError::bad_request(format!("That upload could not be read: {err}.")))?
    {
        let name = field.name().unwrap_or_default().to_string();
        let filename = field.content_disposition().and_then(|cd| cd.get_filename());
        let filename = filename.map(str::to_string);
        let mime_type = field.content_type().map(ToString::to_string);

        let mut data = Vec::new();
        while let Some(chunk) = field.try_next().await.map_err(|err| {
            ApiError::bad_request(format!("That upload could not be read: {err}."))
        })? {
            if data.len() + chunk.len() > MAX_FILE_BYTES {
                return Err(ApiError::bad_request(format!(
                    "A profile file may be at most {} MB.",
                    MAX_FILE_BYTES / 1024 / 1024
                )));
            }

            data.extend_from_slice(&chunk);
        }

        match name.as_str() {
            "filename" | "name" => {
                override_name = String::from_utf8(data)
                    .ok()
                    .map(|value| value.trim().to_string());
            }
            _ if filename.is_some() || name == "file" => {
                upload = Some(Upload {
                    name: filename.unwrap_or(name),
                    data,
                    mime_type,
                });
            }
            _ => {}
        }
    }

    let mut upload =
        upload.ok_or_else(|| ApiError::bad_request("That upload carried no file part."))?;

    if let Some(name) = override_name.filter(|name| !name.is_empty()) {
        upload.name = name;
    }

    upload.name = sanitise(&upload.name)?;

    Ok(upload)
}

/// A delivered path that cannot climb out of the profile.
fn sanitise(name: &str) -> Result<String, ApiError> {
    let trimmed = name.trim().trim_start_matches('/').replace('\\', "/");

    if trimmed.is_empty()
        || trimmed.split('/').any(|segment| segment == "..")
        || trimmed.ends_with('/')
    {
        return Err(ApiError::bad_request(
            "That is not a filename a device could store.",
        ));
    }

    Ok(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filename_cannot_climb_out_of_the_profile() {
        for name in ["../a.pref", "/../a.pref", "a/../../b", "", "   ", "dir/"] {
            assert!(sanitise(name).is_err(), "{name}");
        }

        assert_eq!(sanitise("/maps/source.xml").unwrap(), "maps/source.xml");
        assert_eq!(sanitise(r"maps\source.xml").unwrap(), "maps/source.xml");
    }
}
