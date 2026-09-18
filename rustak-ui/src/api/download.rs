//! Files, in both directions.
//!
//! Three endpoints hand an administrator bytes — a profile preview, a mission
//! archive and a configuration package — and one takes them, the profile file
//! upload. None of the four can be a plain link or a plain `<form action>`,
//! because every one of them is behind the bearer token the application holds
//! in `sessionStorage` and a browser attaches no such header to a navigation.
//!
//! So a download is fetched like any other call and then *saved*: the body
//! becomes a `Blob`, `URL.createObjectURL` names it, and a detached anchor
//! carrying `download=` is clicked at it. The object URL is revoked
//! immediately afterwards, because it pins the whole body in memory for as
//! long as the document lives and a zip is not small.
//!
//! # The upload never reads the file
//!
//! `FormData` takes the browser's own `File` object, so a profile file goes
//! from the drop zone to the socket without its bytes ever being copied into
//! the wasm heap. That is what makes a large file cheap here, and it is also
//! why [`upload`] takes a `File` rather than a `Vec<u8>`.

use gloo_net::http::{Request, Response};
use serde::de::DeserializeOwned;
use wasm_bindgen::JsCast;
use web_sys::{Blob, BlobPropertyBag, FormData, HtmlAnchorElement, Url};

use crate::api::{API_BASE, ApiError, error_from_response, json_response};
use crate::auth as session;
use crate::util::window;

/// A file the server has handed us, waiting to be saved.
///
/// Held rather than saved immediately so that a page can report a failure —
/// "that profile would send a device nothing" is a `400` with a message worth
/// reading, and a browser that has already been told to download something
/// cannot show it.
#[derive(Clone, PartialEq)]
pub struct Download {
    pub filename: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

/// The name the server asked for, from `Content-Disposition`.
///
/// Deliberately forgiving: a header we cannot parse costs the caller's own
/// fallback name rather than the download.
fn filename_from(response: &Response, fallback: &str) -> String {
    let Some(header) = response.headers().get("content-disposition") else {
        return fallback.to_string();
    };

    header
        .split(';')
        .map(str::trim)
        .find_map(|part| part.strip_prefix("filename="))
        .map(|name| name.trim_matches('"').trim())
        .filter(|name| !name.is_empty() && !name.contains('/'))
        .map(ToString::to_string)
        .unwrap_or_else(|| fallback.to_string())
}

/// Reads a successful response as a file, or converts the failure.
async fn as_download(response: Response, fallback: &str) -> Result<Download, ApiError> {
    if !response.ok() {
        return Err(error_from_response(response).await);
    }

    let filename = filename_from(&response, fallback);
    let mime = response
        .headers()
        .get("content-type")
        .unwrap_or_else(|| "application/octet-stream".to_string());

    let bytes = response
        .binary()
        .await
        .map_err(|err| ApiError::Server(err.to_string()))?;

    Ok(Download {
        filename,
        mime,
        bytes,
    })
}

/// GETs a path and reads the body as a file.
pub async fn get_download(path: &str, fallback: &str) -> Result<Download, ApiError> {
    let response = super::send::<()>(super::Verb::Get, path, None).await?;

    as_download(response, fallback).await
}

/// POSTs a JSON body and reads the response as a file.
pub async fn post_download<B: serde::Serialize>(
    path: &str,
    body: &B,
    fallback: &str,
) -> Result<Download, ApiError> {
    let response = super::send(super::Verb::Post, path, Some(body)).await?;

    as_download(response, fallback).await
}

/// Sends a `multipart/form-data` body, renewing the token once on a 401.
///
/// A sibling of [`super::send`] rather than a branch inside it: that one sets
/// `Content-Type: application/json` from the body it serialises, and a
/// multipart request must leave the header alone so the browser can write the
/// boundary it generated into it.
async fn send_form(url: &str, form: &FormData) -> Result<Response, ApiError> {
    let build = |token: Option<&str>| -> Result<Request, ApiError> {
        let builder = Request::post(url);
        let builder = match token {
            Some(token) => builder.header("Authorization", &format!("Bearer {token}")),
            None => builder,
        };

        builder
            .body(form.clone())
            .map_err(|err| ApiError::Network(err.to_string()))
    };

    let response = build(session::stored_token().as_deref())?
        .send()
        .await
        .map_err(|err| ApiError::Network(err.to_string()))?;

    if response.status() != 401 {
        return Ok(response);
    }

    if let Ok(fresh) = session::refresh_session().await {
        return build(Some(&fresh))?
            .send()
            .await
            .map_err(|err| ApiError::Network(err.to_string()));
    }

    session::clear_session();
    Ok(response)
}

/// Uploads one file with whatever text fields go beside it.
///
/// The file is always the part named `file`, because that is what both upload
/// endpoints look for first and what a form built by a browser produces.
pub async fn upload_many<T: DeserializeOwned>(
    path: &str,
    file: &web_sys::File,
    fields: &[(&str, String)],
) -> Result<T, ApiError> {
    let form = FormData::new().map_err(|_| {
        ApiError::Network("This browser would not let us assemble the upload.".to_string())
    })?;

    form.append_with_blob_and_filename("file", file, &file.name())
        .map_err(|_| ApiError::Network("That file could not be attached.".to_string()))?;

    for (name, value) in fields {
        form.append_with_str(name, value)
            .map_err(|_| ApiError::Network(format!("'{name}' could not be attached.")))?;
    }

    json_response(send_form(&format!("{API_BASE}{path}"), &form).await?).await
}

/// Uploads one file under an optional name of its own.
///
/// `name` overrides the delivered path when the caller has one — a profile
/// file is stored under the path a device will write it to, which is not
/// always what the file happened to be called on somebody's desktop.
pub async fn upload<T: DeserializeOwned>(
    path: &str,
    file: &web_sys::File,
    name: Option<&str>,
) -> Result<T, ApiError> {
    let fields: Vec<(&str, String)> = name
        .filter(|name| !name.trim().is_empty())
        .map(|name| vec![("filename", name.trim().to_string())])
        .unwrap_or_default();

    upload_many(path, file, &fields).await
}

/// Hands a fetched file to the browser to save.
///
/// # Errors
///
/// When the document will not produce the anchor this needs, which is not a
/// condition any page can recover from but is worth saying rather than
/// silently doing nothing.
pub fn save(download: &Download) -> Result<(), ApiError> {
    let refused = || {
        ApiError::Network(
            "Your browser would not let us save that file. Try again, or check whether \
             downloads are blocked for this site."
                .to_string(),
        )
    };

    let array = js_sys::Uint8Array::from(download.bytes.as_slice());
    let parts = js_sys::Array::of1(&array);
    let options = BlobPropertyBag::new();
    options.set_type(&download.mime);

    let blob =
        Blob::new_with_u8_array_sequence_and_options(&parts, &options).map_err(|_| refused())?;
    let url = Url::create_object_url_with_blob(&blob).map_err(|_| refused())?;

    let document = window().document().ok_or_else(refused)?;
    let anchor: HtmlAnchorElement = document
        .create_element("a")
        .map_err(|_| refused())?
        .dyn_into()
        .map_err(|_| refused())?;

    anchor.set_href(&url);
    anchor.set_download(&download.filename);
    anchor.click();

    // The object URL holds the whole body alive until it is revoked, and these
    // bodies are zips. Revoking straight after the click is safe: the browser
    // has already resolved it by then.
    let _ = Url::revoke_object_url(&url);

    Ok(())
}
