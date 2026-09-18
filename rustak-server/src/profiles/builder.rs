//! Turning a list of files into the Mission Package a device unpacks.
//!
//! Two layouts, and the difference is not cosmetic. The package a profile
//! endpoint returns puts every file in its **own numbered directory**
//! (`file0/`, `file1/`, …) and flattens the name; the package
//! `/tool/{tool}/file` returns when more than one file matched **recreates the
//! directory hierarchy** the files were stored under. ATAK's sorters key off
//! the path, so a file that arrives at the wrong depth is sorted somewhere the
//! client will not look for it.
//!
//! # Deterministic bytes
//!
//! Every entry is written with the same 1980-01-01 timestamp, so two builds of
//! the same profile produce the same zip. That is what makes a golden test
//! possible, and it keeps `Last-Modified` — which is computed from the
//! profile's own `updated`, not from the archive — the only thing that tells a
//! client something changed.

use std::io::{Cursor, Write as _};

use chrono::{DateTime, Utc};
use rustak_core::prelude::*;
use zip::write::SimpleFileOptions;

use crate::files::package::{ContentEntry, MANIFEST_PATH, Manifest, write_manifest};

/// The name every packaged profile response is downloaded as.
pub const PROFILE_FILENAME: &str = "profile.zip";

/// The manifest name `/tool/{tool}/file` uses when it returns several files.
pub const MULTI_FILE: &str = "multiFile";

/// One file about to be packaged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileFileData {
    /// The path the file is delivered under. May contain `/`.
    pub name: String,
    pub data: Vec<u8>,
    /// When the file last changed, which is what `Last-Modified` is computed
    /// from.
    pub updated: DateTime<Utc>,
}

impl ProfileFileData {
    /// A file with the given contents.
    pub fn new(name: impl Into<String>, data: Vec<u8>, updated: DateTime<Utc>) -> Self {
        Self {
            name: name.into(),
            data,
            updated,
        }
    }

    /// The last path segment, which is what the `fileN/` layout delivers under.
    pub fn basename(&self) -> &str {
        self.name.rsplit('/').next().unwrap_or(&self.name)
    }

    /// The content type a client should be told, guessed from the extension.
    pub fn content_type(&self) -> &'static str {
        guess_content_type(&self.name)
    }
}

/// Builds the package `/enrollment`, `/connection` and `/tool/{tool}` return.
///
/// `name` is what the import dialog shows: `Enrollment`, `Connection`, or the
/// tool's own name.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the archive cannot be written,
/// which means something is wrong with this process rather than with the
/// request.
pub fn build_profile_package(name: &str, files: &[ProfileFileData]) -> Result<Vec<u8>, Error> {
    let mut manifest = Manifest::new(uuid::Uuid::new_v4().to_string(), name)
        .parameter("onReceiveImport", "true")
        .parameter("onReceiveDelete", "true");

    let mut entries: Vec<(String, &[u8])> = Vec::with_capacity(files.len());

    for (index, file) in files.iter().enumerate() {
        let entry = format!("file{index}/{}", file.basename());
        manifest = manifest.content(ContentEntry::new(entry.clone()));
        entries.push((entry, &file.data));
    }

    write_package(&manifest, &entries)
}

/// Builds the package `/tool/{tool}/file` returns when more than one file
/// matched, which keeps each file where it was stored.
///
/// # Errors
///
/// As [`build_profile_package`].
pub fn build_multifile_package(files: &[ProfileFileData]) -> Result<Vec<u8>, Error> {
    let mut manifest = Manifest::new(uuid::Uuid::new_v4().to_string(), MULTI_FILE)
        .parameter("onReceiveImport", "true")
        .parameter("onReceiveDelete", "true");

    let mut entries: Vec<(String, &[u8])> = Vec::with_capacity(files.len());

    for file in files {
        let entry = file.name.trim_start_matches('/').to_string();
        manifest = manifest.content(ContentEntry::new(entry.clone()));
        entries.push((entry, &file.data));
    }

    write_package(&manifest, &entries)
}

/// Writes a zip holding `entries` plus `MANIFEST/manifest.xml`.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the archive cannot be written.
pub fn write_package(manifest: &Manifest, entries: &[(String, &[u8])]) -> Result<Vec<u8>, Error> {
    let rendered = write_manifest(manifest);
    let mut all: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(name, data)| (name.as_str(), *data))
        .collect();

    all.push((MANIFEST_PATH, rendered.as_bytes()));

    write_zip(&all)
}

/// Writes a plain zip, with a directory entry ahead of every nested file.
///
/// The directory entries are what TAK Server's own package writer emits, and
/// at least one importer in the wild refuses an archive without them.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error when the archive cannot be written.
pub fn write_zip(entries: &[(&str, &[u8])]) -> Result<Vec<u8>, Error> {
    let mut buffer = Vec::new();
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::default());

    {
        let mut writer = zip::ZipWriter::new(Cursor::new(&mut buffer));
        let mut written: Vec<String> = Vec::new();

        for (name, data) in entries {
            for directory in ancestors(name) {
                if written.iter().any(|seen| seen == &directory) {
                    continue;
                }

                writer.add_directory(&directory, options).map_err(failed)?;
                written.push(directory);
            }

            writer.start_file(*name, options).map_err(failed)?;
            writer.write_all(data).map_err(|err| {
                human_errors::system(
                    format!("A profile package could not be written: {err}."),
                    ADVICE,
                )
            })?;
        }

        writer.finish().map_err(failed)?;
    }

    Ok(buffer)
}

/// Every directory prefix of `path`, outermost first, each with a trailing `/`.
fn ancestors(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut prefix = String::new();

    let segments: Vec<&str> = path.split('/').collect();
    for segment in &segments[..segments.len().saturating_sub(1)] {
        prefix.push_str(segment);
        prefix.push('/');
        out.push(prefix.clone());
    }

    out
}

/// The content type to declare for a single file returned raw.
///
/// A small table rather than a dependency: these are the extensions a profile
/// actually carries, and `application/octet-stream` is a correct answer for
/// everything else.
pub fn guess_content_type(name: &str) -> &'static str {
    let extension = name
        .rsplit('.')
        .next()
        .filter(|extension| *extension != name)
        .unwrap_or_default()
        .to_ascii_lowercase();

    match extension.as_str() {
        "pref" | "xml" | "kml" => "application/xml",
        "zip" => "application/zip",
        "json" => "application/json",
        "txt" | "csv" => "text/plain",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "p12" | "pfx" => "application/x-pkcs12",
        "kmz" => "application/vnd.google-earth.kmz",
        _ => "application/octet-stream",
    }
}

/// Advice for a failure nobody outside this process can act on.
const ADVICE: &[&str] = &["This is unexpected; please report it with the surrounding log entries."];

/// What a zip we could not write is reported as.
fn failed(err: zip::result::ZipError) -> Error {
    human_errors::system(
        format!("A profile package could not be written: {err}."),
        ADVICE,
    )
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use super::*;
    use crate::files::package::read_manifest;

    fn file(name: &str, data: &str) -> ProfileFileData {
        ProfileFileData::new(name, data.as_bytes().to_vec(), Utc::now())
    }

    fn names(zip: &[u8]) -> Vec<String> {
        zip::ZipArchive::new(Cursor::new(zip.to_vec()))
            .unwrap()
            .file_names()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn every_file_gets_its_own_numbered_directory() {
        let built = build_profile_package(
            "Enrollment",
            &[
                file("rustak-enrollment.pref", "<preferences/>"),
                file("maps/source.xml", "<x/>"),
            ],
        )
        .unwrap();

        let listed = names(&built);

        assert!(listed.contains(&"file0/rustak-enrollment.pref".to_string()));
        assert!(
            listed.contains(&"file1/source.xml".to_string()),
            "the fileN layout flattens the name: {listed:?}",
        );
        assert!(listed.contains(&"MANIFEST/manifest.xml".to_string()));
        assert!(listed.contains(&"file0/".to_string()), "{listed:?}");
    }

    #[test]
    fn the_manifest_carries_the_four_parameters_atak_reads() {
        let built = build_profile_package("Connection", &[file("a.pref", "x")]).unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(built)).unwrap();
        let (manifest, prefix) = read_manifest(&mut archive).unwrap();

        assert_eq!(prefix, "");
        assert_eq!(manifest.name(), Some("Connection"));
        assert_eq!(manifest.uid().map(str::len), Some(36));
        assert_eq!(manifest.config("onReceiveImport"), Some("true"));
        assert_eq!(
            manifest.config("onReceiveDelete"),
            Some("true"),
            "a profile package deletes itself after import; a mission archive does not",
        );
        assert_eq!(manifest.contents.len(), 1);
        assert_eq!(manifest.contents[0].zip_entry, "file0/a.pref");
    }

    #[test]
    fn a_multi_file_package_keeps_the_directories_the_files_were_stored_under() {
        let built =
            build_multifile_package(&[file("maps/source.xml", "<x/>"), file("top.pref", "y")])
                .unwrap();

        let listed = names(&built);
        assert!(
            listed.contains(&"maps/source.xml".to_string()),
            "{listed:?}"
        );
        assert!(listed.contains(&"top.pref".to_string()));

        let mut archive = zip::ZipArchive::new(Cursor::new(built)).unwrap();
        let (manifest, _) = read_manifest(&mut archive).unwrap();

        assert_eq!(manifest.name(), Some(MULTI_FILE));
    }

    #[test]
    fn two_builds_of_the_same_files_lay_out_the_same_archive() {
        // The entry timestamps are fixed at 1980-01-01, so nothing but the
        // minted manifest identifier varies between builds — which is what
        // makes a golden test of the layout worth writing.
        let files = [file("a.pref", "x"), file("maps/source.xml", "<x/>")];
        let first = build_profile_package("Enrollment", &files).unwrap();
        let second = build_profile_package("Enrollment", &files).unwrap();

        assert_eq!(names(&first), names(&second));

        let mut first = zip::ZipArchive::new(Cursor::new(first)).unwrap();
        let mut second = zip::ZipArchive::new(Cursor::new(second)).unwrap();

        for name in ["file0/a.pref", "file1/source.xml"] {
            let mut left = Vec::new();
            let mut right = Vec::new();
            first.by_name(name).unwrap().read_to_end(&mut left).unwrap();
            second
                .by_name(name)
                .unwrap()
                .read_to_end(&mut right)
                .unwrap();

            assert_eq!(left, right, "{name}");
        }
    }

    #[test]
    fn the_bytes_written_come_back_out() {
        let built = build_profile_package("Enrollment", &[file("a.pref", "hello")]).unwrap();
        let mut archive = zip::ZipArchive::new(Cursor::new(built)).unwrap();

        let mut body = String::new();
        archive
            .by_name("file0/a.pref")
            .unwrap()
            .read_to_string(&mut body)
            .unwrap();

        assert_eq!(body, "hello");
    }

    #[test]
    fn a_content_type_is_guessed_from_the_extension() {
        assert_eq!(guess_content_type("a.pref"), "application/xml");
        assert_eq!(guess_content_type("a.P12"), "application/x-pkcs12");
        assert_eq!(
            guess_content_type("noextension"),
            "application/octet-stream"
        );
        assert_eq!(guess_content_type("a.unknown"), "application/octet-stream");
    }
}
