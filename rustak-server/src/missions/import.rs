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
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
            .map_err(|err| MartiError::Duplicate(format!("that package is not a zip: {err}")))?;
        let (manifest, prefix) = package::read_manifest(&mut archive)
            .map_err(|err| MartiError::Duplicate(err.description()))?;

        let mut changes = Vec::new();

        for entry in manifest.contents.iter().filter(|entry| !entry.ignore) {
            let path = format!("{prefix}{}", entry.zip_entry);
            let Some(bytes) = read_entry(&mut archive, &path) else {
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

/// One entry's bytes, or nothing when the zip does not hold it.
fn read_entry(
    archive: &mut zip::ZipArchive<std::io::Cursor<Vec<u8>>>,
    path: &str,
) -> Option<Vec<u8>> {
    let mut entry = archive.by_name(path).ok()?;
    let mut bytes = Vec::new();

    entry.read_to_end(&mut bytes).ok()?;

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
