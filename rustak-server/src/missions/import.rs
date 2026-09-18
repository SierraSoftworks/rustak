//! Importing a Mission Package into a mission.
//!
//! `PUT …/contents/missionpackage` takes the same zip ATAK exports from its own
//! Data Sync screen: a `MANIFEST/manifest.xml` and the files it names. Each
//! entry becomes either a filed map item (a `.cot`, whose uid is what gets
//! filed) or a stored resource, and every one of them appends an `ADD_CONTENT`
//! change so the import looks exactly like the same files being attached one at
//! a time.
//!
//! # Only a malformed manifest is a refusal
//!
//! An entry we cannot read — a `zipEntry` naming a file the zip does not hold,
//! a `.cot` that will not parse — is skipped and logged. A package is somebody
//! pressing "share" on a device with an intermittent link, and losing the nine
//! files that did arrive because the tenth is truncated helps nobody.

use std::io::Read as _;

use chrono::Utc;

use crate::db::repos::{
    MissionChangeRow, MissionContentRow, MissionUidRow, NewChange, NewResource,
};
use crate::files::package;
use crate::marti::MartiError;
use crate::prelude::*;

use super::contents::{ADD_CONTENT, details_of};
use super::model::Mission;
use super::service::MissionService;

/// What a package entry is stored as when the manifest does not say.
const DEFAULT_MIME: &str = "application/octet-stream";

/// The manifest parameter ATAK marks a CoT entry with.
const IS_COT: &str = "isCoT";

/// The most entries one package may declare.
///
/// A manifest is a list somebody's device wrote; a list of a million names is
/// not one of those, and refusing it costs nothing legitimate.
const MAX_ENTRIES: usize = 4_096;

impl MissionService {
    /// Files every entry of a Mission Package under a mission.
    ///
    /// # Errors
    ///
    /// [`MartiError::Duplicate`] — the `409` this route answers with — for a
    /// zip we cannot open or a manifest we cannot read, which are the only two
    /// failures that make the whole package unusable.
    pub async fn import_package(
        &self,
        mission: &Mission,
        bytes: Vec<u8>,
        creator_uid: Option<&str>,
    ) -> Result<Vec<MissionChangeRow>, MartiError> {
        // The limit applies to what comes *out* of the zip, not to what went
        // in: deflate reaches a thousand to one, so a package inside the upload
        // ceiling can still inflate to more memory than the server has.
        let limit = crate::files::limits::limit_bytes(&self.context.config(), self.db()).await?;

        // Inflated on a blocking thread. Deflate is CPU-bound and an actix
        // worker running it is a worker not answering anything else — including
        // the health check the orchestrator restarts the pod over.
        let extracted = tokio::task::spawn_blocking(move || extract(bytes, limit))
            .await
            .map_err(|err| {
                MartiError::Internal(format!("A mission package could not be read: {err}."))
            })??;

        let Extracted {
            manifest,
            prefix,
            mut files,
        } = extracted;

        let mut changes = Vec::new();

        for entry in manifest.contents.iter().filter(|entry| !entry.ignore) {
            let path = format!("{prefix}{}", entry.zip_entry);
            let Some(bytes) = files.remove(&path) else {
                warn!(entry = %path, "Skipped a package entry the zip does not hold.");
                continue;
            };

            let is_cot = entry.param(IS_COT).is_some_and(|value| value == "true")
                || path.to_lowercase().ends_with(".cot");

            match is_cot {
                true => changes.extend(self.file_event(mission, &bytes, creator_uid).await?),
                false => changes.push(
                    self.file_entry(mission, entry, &path, bytes, creator_uid)
                        .await?,
                ),
            }
        }

        let recorded = self.db().mission_changes().record_all(changes).await?;

        self.notify_content(mission, &recorded, creator_uid).await?;

        Ok(recorded)
    }

    /// Files the map item a `.cot` entry describes.
    async fn file_event(
        &self,
        mission: &Mission,
        bytes: &[u8],
        creator_uid: Option<&str>,
    ) -> Result<Option<NewChange>, MartiError> {
        let Ok(event) = rustak_cot::xml::parse(bytes) else {
            warn!("Skipped a package entry that is not a CoT event.");

            return Ok(None);
        };

        let details = serde_json::to_value(details_of(&event)).ok();
        let at = Utc::now();

        self.db()
            .mission_contents()
            .upsert_uid(MissionUidRow {
                creator_uid: creator_uid.map(str::to_string),
                details: details.clone(),
                ..MissionUidRow::new(mission.id, event.uid.clone(), at)
            })
            .await?;

        Ok(Some(
            NewChange::new(mission.id, ADD_CONTENT, at)
                .by(creator_uid)
                .about_uid(event.uid)
                .with_detail(details),
        ))
    }

    /// Stores one package entry and files it as a resource.
    async fn file_entry(
        &self,
        mission: &Mission,
        entry: &package::ContentEntry,
        path: &str,
        bytes: Vec<u8>,
        creator_uid: Option<&str>,
    ) -> Result<NewChange, MartiError> {
        let size = i64::try_from(bytes.len()).unwrap_or(i64::MAX);
        let stored = self.context.content()?.put_bytes(&bytes).await?;
        let filename = path.rsplit('/').next().unwrap_or(path).to_string();
        let at = Utc::now();

        let resource = self
            .db()
            .resources()
            .upsert(NewResource {
                hash: stored.hash.clone(),
                uid: entry
                    .param("uid")
                    .map(str::to_string)
                    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                name: entry
                    .param("name")
                    .map(str::to_string)
                    .unwrap_or_else(|| filename.clone()),
                filename: Some(filename.clone()),
                mime_type: entry
                    .param("mimeType")
                    .map(str::to_string)
                    .unwrap_or_else(|| mime_for(&filename).to_string()),
                size,
                tool: "public".to_string(),
                creator_uid: creator_uid.map(str::to_string),
                submitter_id: None,
                submitter: None,
                expiration: None,
                is_mission_package: false,
                groups: mission.groups.clone(),
                mission_name: Some(mission.name.clone()),
                latitude: None,
                longitude: None,
                altitude: None,
                remarks: None,
                permissions: None,
                contacts: None,
                download_path: Some(filename),
                plugin_class_name: None,
                keywords: entry
                    .param("keywords")
                    .map(|keywords| {
                        keywords
                            .split(',')
                            .map(str::trim)
                            .filter(|keyword| !keyword.is_empty())
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
            })
            .await?;

        self.db()
            .mission_contents()
            .upsert_content(MissionContentRow {
                creator_uid: creator_uid.map(str::to_string),
                ..MissionContentRow::new(mission.id, resource.id, at)
            })
            .await?;

        Ok(NewChange::new(mission.id, ADD_CONTENT, at)
            .by(creator_uid)
            .about_hash(stored.hash))
    }
}

/// A package after it has been inflated, ready for the async filing.
struct Extracted {
    manifest: package::Manifest,
    /// The directory every `zipEntry` is relative to.
    prefix: String,
    /// Each entry's bytes, by its full path in the zip.
    files: std::collections::HashMap<String, Vec<u8>>,
}

/// Opens a package and inflates every entry its manifest names, under a cap.
///
/// Synchronous on purpose: the caller runs it on a blocking thread.
fn extract(bytes: Vec<u8>, limit: u64) -> Result<Extracted, MartiError> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|err| MartiError::Duplicate(format!("that package is not a zip: {err}")))?;
    let (manifest, prefix) = package::read_manifest(&mut archive)
        .map_err(|err| MartiError::Duplicate(err.description()))?;

    if manifest.contents.len() > MAX_ENTRIES {
        return Err(MartiError::Duplicate(format!(
            "that package declares {} entries, which is more than the {MAX_ENTRIES} we will read",
            manifest.contents.len()
        )));
    }

    let mut files = std::collections::HashMap::new();
    let mut inflated = 0u64;

    for entry in manifest.contents.iter().filter(|entry| !entry.ignore) {
        let path = format!("{prefix}{}", entry.zip_entry);
        let Some(bytes) = read_entry(&mut archive, &path, limit - inflated.min(limit)) else {
            continue;
        };

        inflated = inflated.saturating_add(bytes.len() as u64);

        if inflated > limit {
            return Err(MartiError::Duplicate(format!(
                "that package inflates to more than the {limit} byte upload limit"
            )));
        }

        files.insert(path, bytes);
    }

    Ok(Extracted {
        manifest,
        prefix,
        files,
    })
}

/// One entry's bytes, or nothing when the zip does not hold it or it would take
/// the package past what is left of the budget.
fn read_entry(
    archive: &mut zip::ZipArchive<std::io::Cursor<Vec<u8>>>,
    path: &str,
    remaining: u64,
) -> Option<Vec<u8>> {
    let entry = archive.by_name(path).ok()?;
    let mut bytes = Vec::new();

    // One byte past the budget, so the caller can tell "exactly full" from
    // "over" without trusting the header's declared size.
    entry
        .take(remaining.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;

    Some(bytes)
}

/// A content type guessed from a filename.
///
/// Deliberately short: the manifest usually says, and a wrong guess here only
/// changes which icon a file manager draws.
fn mime_for(filename: &str) -> &'static str {
    match filename
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "kml" => "application/vnd.google-earth.kml+xml",
        "kmz" => "application/vnd.google-earth.kmz",
        "xml" | "cot" => "application/xml",
        "txt" => "text/plain",
        _ => DEFAULT_MIME,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A package holding one entry of `size` bytes.
    fn package_of(size: usize) -> Vec<u8> {
        use crate::files::package::{ContentEntry, Manifest};
        use crate::profiles::builder::write_package;

        let manifest = Manifest::new("uid-1", "Test").content(ContentEntry::new("big.bin"));
        // Incompressible, so the zip cannot shrink it below the cap by itself.
        let data: Vec<u8> = (0..size).map(|index| (index % 251) as u8).collect();

        write_package(&manifest, &[("big.bin".to_string(), data.as_slice())]).unwrap()
    }

    #[test]
    fn a_package_that_inflates_past_the_upload_limit_is_refused() {
        // M1/H6: the input used to be capped at actix's accidental 256 KiB and
        // the decompressed total at nothing at all, so a zip bomb inflated on
        // the worker thread that was reading it.
        let bytes = package_of(64 * 1024);

        let Err(err) = extract(bytes, 8 * 1024) else {
            panic!("a package past the limit should be refused");
        };

        assert!(
            matches!(&err, MartiError::Duplicate(message) if message.contains("inflates")),
            "{err:?}",
        );
    }

    #[test]
    fn a_package_inside_the_limit_is_read_whole() {
        let extracted = extract(package_of(1_024), 1_000_000).unwrap();

        assert_eq!(extracted.manifest.contents.len(), 1);
        assert_eq!(extracted.files.get("big.bin").map(Vec::len), Some(1_024));
    }

    #[test]
    fn a_content_type_is_guessed_from_the_extension() {
        assert_eq!(mime_for("photo.JPG"), "image/jpeg");
        assert_eq!(
            mime_for("route.kml"),
            "application/vnd.google-earth.kml+xml"
        );
        assert_eq!(mime_for("noextension"), DEFAULT_MIME);
        assert_eq!(mime_for(""), DEFAULT_MIME);
    }
}
