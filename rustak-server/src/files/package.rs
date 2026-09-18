//! `MANIFEST/manifest.xml`: reading and writing a Mission Package manifest.
//!
//! One shape, three users. A data package uploaded through Enterprise Sync
//! carries one, a mission archive carries one with two extra sections, and
//! every device-profile bundle this server builds carries one. They are all
//! the same document, so they are all built and read here.
//!
//! # Where the entries are relative to
//!
//! `MANIFEST/manifest.xml` is not necessarily at the zip root: a package built
//! by right-clicking a folder and compressing it is nested one level deep, and
//! ATAK reads those. Every `zipEntry` is therefore relative to *the directory
//! containing `MANIFEST/`*, which [`crate::files::package::read_manifest`]
//! returns alongside the manifest so a caller can resolve them.
//!
//! # The reader is deliberately tolerant
//!
//! ATAK's importer needs `Configuration` to carry `name` and `uid`, and treats
//! a `Content` without a `zipEntry` as invalid. We mint a `uid` for a package
//! that has none rather than refusing it — a missing identifier costs nothing
//! to invent and refusing the package would lose the files — but a `Content`
//! we cannot locate is dropped, because there is nothing to invent there.

use std::io::{Read, Seek};

use quick_xml::Reader;
use quick_xml::events::Event as XmlEvent;
use rustak_core::prelude::*;

/// The `version` attribute every manifest we write carries.
pub const MANIFEST_VERSION: u8 = 2;

/// Where the manifest lives inside a package, below its prefix directory.
pub const MANIFEST_PATH: &str = "MANIFEST/manifest.xml";

/// One `<Content>`: a file in the zip, and what the importer should do with it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContentEntry {
    /// The zip entry, relative to the directory holding `MANIFEST/`.
    pub zip_entry: String,
    /// Whether the importer should skip this entry.
    pub ignore: bool,
    /// `<Parameter>` children, in document order.
    pub params: Vec<(String, String)>,
}

impl ContentEntry {
    /// An entry the importer should act on.
    pub fn new(zip_entry: impl Into<String>) -> Self {
        Self {
            zip_entry: zip_entry.into(),
            ignore: false,
            params: Vec::new(),
        }
    }

    /// Adds a `<Parameter>` child.
    #[must_use]
    pub fn with(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.params.push((name.into(), value.into()));
        self
    }

    /// The value of one parameter, when it is there.
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// A mission archive's `<Role>` section.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoleXml {
    pub name: String,
    pub permissions: Vec<String>,
}

/// A whole `MissionPackageManifest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub version: u8,
    /// `<Configuration>` parameters, in document order. `uid` and `name` are
    /// the two ATAK requires.
    pub configuration: Vec<(String, String)>,
    pub contents: Vec<ContentEntry>,
    /// Mission archives only.
    pub groups: Vec<String>,
    /// Mission archives only.
    pub role: Option<RoleXml>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            version: MANIFEST_VERSION,
            configuration: Vec::new(),
            contents: Vec::new(),
            groups: Vec::new(),
            role: None,
        }
    }
}

impl Manifest {
    /// A manifest named `name`, identified by `uid`.
    pub fn new(uid: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            configuration: vec![
                ("uid".to_string(), uid.into()),
                ("name".to_string(), name.into()),
            ],
            ..Self::default()
        }
    }

    /// Adds a `<Configuration>` parameter.
    #[must_use]
    pub fn parameter(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.configuration.push((name.into(), value.into()));
        self
    }

    /// Adds a `<Content>`.
    #[must_use]
    pub fn content(mut self, entry: ContentEntry) -> Self {
        self.contents.push(entry);
        self
    }

    /// The value of one `<Configuration>` parameter.
    pub fn config(&self, name: &str) -> Option<&str> {
        self.configuration
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The identifier ATAK keys the imported package on.
    pub fn uid(&self) -> Option<&str> {
        self.config("uid")
    }

    /// What the package is called in the import dialog.
    pub fn name(&self) -> Option<&str> {
        self.config("name")
    }
}

/// Renders a manifest as the exact bytes a package carries.
///
/// No pretty-printing: whitespace between elements is data to some parsers and
/// noise to none, so there is none. Attribute values are escaped, which the
/// reference implementation does not do — an unescaped `&` in a filename would
/// otherwise produce a document ATAK's own parser refuses.
pub fn write_manifest(manifest: &Manifest) -> String {
    let mut out = format!(
        "<MissionPackageManifest version=\"{}\"><Configuration>",
        manifest.version
    );

    for (name, value) in &manifest.configuration {
        out.push_str(&parameter(name, value));
    }

    out.push_str("</Configuration><Contents>");

    for entry in &manifest.contents {
        out.push_str(&format!(
            "<Content ignore=\"{}\" zipEntry=\"{}\"",
            entry.ignore,
            escape(&entry.zip_entry),
        ));

        if entry.params.is_empty() {
            out.push_str("/>");
            continue;
        }

        out.push('>');
        for (name, value) in &entry.params {
            out.push_str(&parameter(name, value));
        }
        out.push_str("</Content>");
    }

    out.push_str("</Contents>");

    if !manifest.groups.is_empty() {
        out.push_str("<Groups>");
        for group in &manifest.groups {
            out.push_str(&format!("<Group name=\"{}\"/>", escape(group)));
        }
        out.push_str("</Groups>");
    }

    if let Some(role) = &manifest.role {
        out.push_str(&format!("<Role name=\"{}\">", escape(&role.name)));
        for permission in &role.permissions {
            out.push_str(&format!("<Permission name=\"{}\"/>", escape(permission)));
        }
        out.push_str("</Role>");
    }

    out.push_str("</MissionPackageManifest>");
    out
}

/// Finds and parses the manifest of a package.
///
/// Returns the manifest and the prefix every `zipEntry` is relative to, which
/// is `""` for a package whose `MANIFEST/` is at the root.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when the package has no manifest, when
/// the manifest cannot be read as XML, or when it names no `Configuration`
/// `name` — all three are things the person who uploaded the file can fix.
pub fn read_manifest<R: Read + Seek>(
    zip: &mut zip::ZipArchive<R>,
) -> Result<(Manifest, String), Error> {
    let path = zip
        .file_names()
        .find(|name| name.ends_with(MANIFEST_PATH))
        .map(str::to_string)
        .ok_or_else(|| {
            human_errors::user(
                "That package has no MANIFEST/manifest.xml, so we cannot tell what is in it.",
                &["Export the package from ATAK, or add a manifest, and upload it again."],
            )
        })?;

    let prefix = path[..path.len() - MANIFEST_PATH.len()].to_string();

    let mut xml = String::new();
    zip.by_name(&path)
        .and_then(|mut entry| Ok(entry.read_to_string(&mut xml)?))
        .map_err(|err| {
            human_errors::user(
                format!("That package's manifest could not be read: {err}."),
                &["Export the package from ATAK and upload it again."],
            )
        })?;

    Ok((parse_manifest(&xml)?, prefix))
}

/// Parses a manifest document.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error for malformed XML or a `Configuration`
/// with no `name`.
pub fn parse_manifest(xml: &str) -> Result<Manifest, Error> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().check_end_names = false;
    reader.config_mut().expand_empty_elements = true;

    let mut manifest = Manifest::default();
    let mut in_contents = false;
    let mut current: Option<ContentEntry> = None;
    let mut role: Option<RoleXml> = None;

    loop {
        let event = reader.read_event().map_err(malformed)?;

        match event {
            XmlEvent::Start(tag) => match tag.name().into_inner() {
                "MissionPackageManifest" => {
                    if let Some(version) = attr(&tag, "version") {
                        manifest.version = version.parse().unwrap_or(MANIFEST_VERSION);
                    }
                }
                "Contents" => in_contents = true,
                "Content" => {
                    current = Some(ContentEntry {
                        zip_entry: attr(&tag, "zipEntry").unwrap_or_default(),
                        ignore: attr(&tag, "ignore").as_deref() == Some("true"),
                        params: Vec::new(),
                    });
                }
                "Parameter" => {
                    let Some(name) = attr(&tag, "name") else {
                        continue;
                    };
                    let value = attr(&tag, "value").unwrap_or_default();

                    match current.as_mut() {
                        Some(entry) => entry.params.push((name, value)),
                        None if !in_contents => manifest.configuration.push((name, value)),
                        None => {}
                    }
                }
                "Group" => {
                    if let Some(name) = attr(&tag, "name") {
                        manifest.groups.push(name);
                    }
                }
                "Role" => {
                    role = Some(RoleXml {
                        name: attr(&tag, "name")
                            .or_else(|| attr(&tag, "type"))
                            .unwrap_or_default(),
                        permissions: Vec::new(),
                    });
                }
                "Permission" => {
                    if let (Some(role), Some(name)) = (role.as_mut(), attr(&tag, "name")) {
                        role.permissions.push(name);
                    }
                }
                _ => {}
            },
            XmlEvent::End(tag) => match tag.name().into_inner() {
                // A `Content` with no `zipEntry` names no file, so there is
                // nothing an importer could do with it.
                "Content" => {
                    if let Some(entry) = current.take()
                        && !entry.zip_entry.is_empty()
                    {
                        manifest.contents.push(entry);
                    }
                }
                "Contents" => in_contents = false,
                _ => {}
            },
            XmlEvent::Eof => break,
            _ => {}
        }
    }

    manifest.role = role;

    if manifest.name().is_none() {
        return Err(human_errors::user(
            "That package's manifest does not say what the package is called.",
            &["A manifest needs a Configuration Parameter named 'name'."],
        ));
    }

    // ATAK wants a uid and we can invent one; refusing the package would lose
    // every file in it over an identifier nobody reads.
    if manifest.uid().is_none() {
        manifest
            .configuration
            .insert(0, ("uid".to_string(), uuid::Uuid::new_v4().to_string()));
    }

    Ok(manifest)
}

/// One `<Parameter name= value=/>`.
fn parameter(name: &str, value: &str) -> String {
    format!(
        "<Parameter name=\"{}\" value=\"{}\"/>",
        escape(name),
        escape(value),
    )
}

/// Reads an attribute, unescaped.
fn attr(tag: &quick_xml::events::BytesStart<'_>, name: &str) -> Option<String> {
    tag.attributes().flatten().find_map(|attribute| {
        (attribute.key.into_inner() == name).then(|| unescape(&attribute.value))
    })
}

/// Turns the five XML entities back into the characters they stand for.
fn unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Escapes a value for an XML attribute.
pub fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());

    for character in value.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }

    out
}

/// What a manifest we cannot parse is reported as.
fn malformed(err: quick_xml::Error) -> Error {
    human_errors::user(
        format!("That package's manifest is not valid XML: {err}."),
        &["Export the package from ATAK and upload it again."],
    )
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn a_manifest_renders_with_no_whitespace_between_elements() {
        let manifest = Manifest::new("abc", "Enrollment")
            .parameter("onReceiveImport", "true")
            .parameter("onReceiveDelete", "true")
            .content(ContentEntry::new("file0/rustak-enrollment.pref"));

        assert_eq!(
            write_manifest(&manifest),
            concat!(
                r#"<MissionPackageManifest version="2"><Configuration>"#,
                r#"<Parameter name="uid" value="abc"/>"#,
                r#"<Parameter name="name" value="Enrollment"/>"#,
                r#"<Parameter name="onReceiveImport" value="true"/>"#,
                r#"<Parameter name="onReceiveDelete" value="true"/>"#,
                r#"</Configuration><Contents>"#,
                r#"<Content ignore="false" zipEntry="file0/rustak-enrollment.pref"/>"#,
                r#"</Contents></MissionPackageManifest>"#,
            ),
        );
    }

    #[test]
    fn a_manifest_round_trips_through_its_own_rendering() {
        let manifest = Manifest::new("abc", "Archive")
            .content(ContentEntry::new("file0/a.cot").with("isCoT", "true"))
            .content(ContentEntry::new("file1/b & c.jpg").with("contentType", "image/jpeg"));

        let parsed = parse_manifest(&write_manifest(&manifest)).unwrap();

        assert_eq!(parsed, manifest);
        assert_eq!(parsed.contents[1].zip_entry, "file1/b & c.jpg");
        assert_eq!(parsed.contents[0].param("isCoT"), Some("true"));
    }

    #[test]
    fn groups_and_a_role_round_trip() {
        let manifest = Manifest {
            groups: vec!["Blue".to_string(), "Red".to_string()],
            role: Some(RoleXml {
                name: "MISSION_OWNER".to_string(),
                permissions: vec!["MISSION_WRITE".to_string()],
            }),
            ..Manifest::new("abc", "Archive")
        };

        assert_eq!(
            parse_manifest(&write_manifest(&manifest)).unwrap(),
            manifest
        );
    }

    #[test]
    fn a_missing_uid_is_minted_rather_than_refused() {
        let parsed = parse_manifest(concat!(
            r#"<MissionPackageManifest version="2"><Configuration>"#,
            r#"<Parameter name="name" value="Package"/>"#,
            r#"</Configuration><Contents/></MissionPackageManifest>"#,
        ))
        .unwrap();

        assert_eq!(parsed.name(), Some("Package"));
        assert!(parsed.uid().is_some_and(|uid| uid.len() == 36));
    }

    #[test]
    fn a_content_with_no_zip_entry_is_dropped() {
        let parsed = parse_manifest(concat!(
            r#"<MissionPackageManifest version="2"><Configuration>"#,
            r#"<Parameter name="uid" value="x"/><Parameter name="name" value="P"/>"#,
            r#"</Configuration><Contents>"#,
            r#"<Content ignore="false"/>"#,
            r#"<Content ignore="true" zipEntry="a.txt"/>"#,
            r#"</Contents></MissionPackageManifest>"#,
        ))
        .unwrap();

        assert_eq!(parsed.contents.len(), 1);
        assert!(parsed.contents[0].ignore);
    }

    #[test]
    fn a_manifest_with_no_name_is_refused() {
        assert!(
            parse_manifest(
                r#"<MissionPackageManifest version="2"><Configuration/><Contents/></MissionPackageManifest>"#,
            )
            .is_err(),
        );
    }

    #[test]
    fn a_nested_package_reports_the_prefix_its_entries_are_relative_to() {
        let mut buffer = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(Cursor::new(&mut buffer));
            let options = zip::write::SimpleFileOptions::default();

            writer.start_file("bundle/file0/a.txt", options).unwrap();
            std::io::Write::write_all(&mut writer, b"a").unwrap();
            writer
                .start_file("bundle/MANIFEST/manifest.xml", options)
                .unwrap();
            std::io::Write::write_all(
                &mut writer,
                write_manifest(
                    &Manifest::new("u", "Bundle").content(ContentEntry::new("file0/a.txt")),
                )
                .as_bytes(),
            )
            .unwrap();
            writer.finish().unwrap();
        }

        let mut archive = zip::ZipArchive::new(Cursor::new(buffer)).unwrap();
        let (manifest, prefix) = read_manifest(&mut archive).unwrap();

        assert_eq!(prefix, "bundle/");
        assert_eq!(manifest.contents[0].zip_entry, "file0/a.txt");
    }

    #[test]
    fn a_zip_with_no_manifest_is_refused() {
        let mut buffer = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(Cursor::new(&mut buffer));
            writer
                .start_file("a.txt", zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.finish().unwrap();
        }

        assert!(read_manifest(&mut zip::ZipArchive::new(Cursor::new(buffer)).unwrap()).is_err());
    }
}
