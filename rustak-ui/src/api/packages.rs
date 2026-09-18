//! The data packages this server hands out.
//!
//! # Reading is not administrative; writing is
//!
//! A listing follows the same visibility rule `/Marti/sync/search` does,
//! because it is the same row: somebody who can browse their channels'
//! packages through a TAK client sees the same set here. Uploading, patching
//! and deleting stay administrative, because a package is *delivered* to
//! devices — and `install_on_enrollment` puts one on every device that enrols.
//!
//! # A package that is not yours is a `404`
//!
//! Not a `403`. Telling a caller that a hash exists but is out of their
//! channels is an oracle over everything this server holds, and
//! `/Marti/sync/content` already made the same call — the two surfaces must
//! not disagree.

use rustak_api::{PackageSummary, PackageUpdate};

use crate::api::download::{Download, get_download, upload_many};
use crate::api::{ApiError, delete_empty, get_json, patch_json};
#[cfg(debug_assertions)]
use crate::fixtures;
use crate::fixtures::demo;
use crate::util::urlencode;

/// What a listing is narrowed by.
#[derive(Clone, Default, PartialEq)]
pub struct PackageFilter {
    /// Only the files carrying the `missionpackage` keyword, which is what a
    /// client's data-package browser lists.
    pub mission_package: bool,

    /// Part of a name or of a keyword, matched without regard to case.
    pub query: String,
}

/// The packages this caller may see, newest first.
pub async fn list(filter: &PackageFilter) -> Result<Vec<PackageSummary>, ApiError> {
    demo!(Ok(fixtures::packages(filter)));

    let mut query = Vec::new();
    if filter.mission_package {
        query.push("missionPackage=true".to_string());
    }
    if !filter.query.trim().is_empty() {
        query.push(format!("q={}", urlencode(filter.query.trim())));
    }

    match query.is_empty() {
        true => get_json("/packages").await,
        false => get_json(&format!("/packages?{}", query.join("&"))).await,
    }
}

/// Changes one. Every absent field is left alone, and an empty change is
/// refused by the server rather than silently accepted.
///
/// A hash may back several rows — the same photograph attached to two map
/// items, the same package uploaded by two people — and the server writes
/// every one of them, which is what keeps a client's browser and this page
/// showing the same thing.
pub async fn patch(hash: &str, change: &PackageUpdate) -> Result<PackageSummary, ApiError> {
    demo!(fixtures::patch_package(hash, change));

    patch_json(&format!("/packages/{}", urlencode(hash)), change).await
}

/// Removes every row with this hash, and then the bytes nothing else wants.
pub async fn remove(hash: &str) -> Result<(), ApiError> {
    demo!(fixtures::delete_package(hash));

    delete_empty(&format!("/packages/{}", urlencode(hash))).await
}

/// The bytes, as an attachment.
pub async fn content(package: &PackageSummary) -> Result<Download, ApiError> {
    demo!(fixtures::package_content(package));

    get_download(
        &format!("/packages/{}/content", urlencode(&package.hash)),
        package.filename.as_deref().unwrap_or(package.name.as_str()),
    )
    .await
}

/// Uploads one, with the metadata the form carries.
///
/// The bytes travel as the browser's own `File`, so a four-hundred-megabyte
/// package never enters the wasm heap.
pub async fn create(
    file: &web_sys::File,
    name: Option<&str>,
    tool: Option<&str>,
    keywords: &[String],
    groups: &[String],
) -> Result<PackageSummary, ApiError> {
    demo!(fixtures::create_package(
        name.filter(|name| !name.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| file.name()),
        file.size() as i64,
        tool,
        keywords,
        groups,
    ));

    let mut fields: Vec<(&str, String)> = Vec::new();
    if let Some(name) = name.filter(|name| !name.trim().is_empty()) {
        fields.push(("name", name.trim().to_string()));
    }
    if let Some(tool) = tool.filter(|tool| !tool.trim().is_empty()) {
        fields.push(("tool", tool.trim().to_string()));
    }
    if !keywords.is_empty() {
        fields.push(("keywords", keywords.join(",")));
    }
    if !groups.is_empty() {
        fields.push(("groups", groups.join(",")));
    }

    upload_many("/packages", file, &fields).await
}
