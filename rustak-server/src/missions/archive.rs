//! A mission as a Mission Package: everything in it, in one zip.
//!
//! `GET …/archive` is what an operator downloads to take a Data Sync somewhere
//! else, and what `DELETE` stores before it tombstones a mission — so the
//! archive is also the undo. The layout is TAK's own: `cot/<uid>.cot` per map
//! item, `contents/<n>_<name>` per file, and `MANIFEST/manifest.xml` describing
//! both plus the mission's channels and the importer's role.
//!
//! # It round-trips through the ordinary import path
//!
//! Nothing here is archive-specific on the way back in: an archive is a Mission
//! Package, so importing one is [`import`](super::import) with no special case.
//! That is what makes "archive a mission, import it into a new one, compare the
//! uids and hashes" a test rather than a hope.
//!
//! # The `mission_uid` parameter is a connection string
//!
//! `<host>-8443-ssl-<name>` and `<host>:8443:ssl` are how ATAK names the server
//! a mission came from, port and all. They are written from the configured
//! public host because a package taken off one server and imported on another
//! has to say which one it meant.

use crate::files::package::{ContentEntry, Manifest, RoleXml};
use crate::marti::{MartiError, time};
use crate::prelude::*;
use crate::profiles::builder::write_package;

use super::model::Mission;
use super::roles::Role;
use super::service::MissionService;

/// The keyword an archived mission's stored resource carries.
pub const ARCHIVED_MISSION: &str = "ARCHIVED_MISSION";

/// The tool an archived mission's stored resource belongs to.
pub const ARCHIVE_TOOL: &str = "archive";

/// The port a mission connection string names, which is the streaming one.
const STREAM_PORT: u16 = 8443;

impl MissionService {
    /// Builds the Mission Package for a mission.
    ///
    /// `host` is the public name this server is reachable by, which the
    /// manifest's two connection parameters are written from.
    ///
    /// # Errors
    ///
    /// A system error if a read fails or the archive cannot be written.
    pub async fn archive(&self, mission: &Mission, host: &str) -> Result<Vec<u8>, MartiError> {
        let mut manifest = configuration(mission, host);
        let mut entries: Vec<(String, Vec<u8>)> = Vec::new();

        for item in self.db().mission_contents().uids(mission.id).await? {
            // An item whose event has aged out of the store is still part of
            // the mission, and an archive that dropped it would be an archive
            // that loses work. The cached details are enough to rebuild one.
            let xml = match crate::cot_store::latest_xml(self.db(), &item.uid).await? {
                Some(xml) => super::changes::without_marti(&xml),
                None => from_cached(&item),
            };

            let entry = format!("cot/{}.cot", sanitise(&item.uid));
            manifest = manifest.content(
                ContentEntry::new(entry.clone())
                    .with("uid", item.uid.clone())
                    .with("name", format!("{}.cot", item.uid)),
            );
            entries.push((entry, xml.into_bytes()));
        }

        let filed_contents = self.db().mission_contents().contents(mission.id).await?;

        // Fetched only when there is something to read: an installation whose
        // blob store is not open yet can still archive a mission of markers.
        let content = match filed_contents.is_empty() {
            true => None,
            false => Some(self.context.content()?),
        };

        for (index, filed) in filed_contents.into_iter().enumerate() {
            let Some(resource) = self.db().resources().by_id(filed.resource_id).await? else {
                continue;
            };

            let Some(store) = content.as_ref() else {
                continue;
            };

            let Some(bytes) = read_content(store, &resource.hash).await else {
                continue;
            };

            let label = resource
                .filename
                .clone()
                .unwrap_or_else(|| resource.name.clone());
            let entry = format!("contents/{index}_{}", sanitise(&label));

            manifest = manifest.content(
                ContentEntry::new(entry.clone())
                    .with("uid", resource.uid.clone())
                    .with("name", label),
            );
            entries.push((entry, bytes));
        }

        manifest.groups = mission.effective_groups();
        manifest.role = Some(RoleXml {
            name: Role::Owner.as_str().to_string(),
            permissions: Role::Owner
                .permissions()
                .iter()
                .map(|permission| permission.as_str().to_string())
                .collect(),
        });

        let borrowed: Vec<(String, &[u8])> = entries
            .iter()
            .map(|(name, data)| (name.clone(), data.as_slice()))
            .collect();

        Ok(write_package(&manifest, &borrowed)?)
    }

    /// Archives a mission and stores the zip as an ordinary resource.
    ///
    /// Called on the way into a delete, and deliberately infallible: a mission
    /// that cannot be archived — no blob store, no disk — still has to be
    /// deletable, and the failure is logged rather than blocking the operator.
    pub async fn store_archive(&self, mission: &Mission) {
        let host = self
            .context
            .config()
            .marti
            .public_host
            .clone()
            .unwrap_or_else(|| self.context.config().server.name.clone());

        match self.write_archive_resource(mission, &host).await {
            Ok(hash) => debug!(
                mission = mission.name,
                hash, "Archived a mission before retiring it."
            ),
            Err(err) => warn!(
                mission = mission.name,
                error = %err,
                "Could not archive a mission before retiring it; deleting it anyway."
            ),
        }
    }

    /// Builds the archive, stores the bytes and records the resource row.
    async fn write_archive_resource(
        &self,
        mission: &Mission,
        host: &str,
    ) -> Result<String, MartiError> {
        let bytes = self.archive(mission, host).await?;
        let stored = self.context.content()?.put_bytes(&bytes).await?;
        let name = Self::archive_filename(mission);

        self.db()
            .resources()
            .upsert(crate::db::repos::NewResource {
                hash: stored.hash.clone(),
                uid: uuid::Uuid::new_v4().to_string(),
                name: name.clone(),
                filename: Some(name),
                mime_type: "application/zip".to_string(),
                size: i64::try_from(bytes.len()).unwrap_or_default(),
                tool: ARCHIVE_TOOL.to_string(),
                creator_uid: mission.creator_uid.clone(),
                submitter_id: None,
                submitter: None,
                expiration: None,
                is_mission_package: true,
                groups: mission.effective_groups(),
                mission_name: Some(mission.name.clone()),
                latitude: None,
                longitude: None,
                altitude: None,
                remarks: None,
                permissions: None,
                contacts: None,
                download_path: None,
                plugin_class_name: None,
                keywords: vec![ARCHIVED_MISSION.to_string()],
            })
            .await?;

        Ok(stored.hash)
    }

    /// The public host name an archive's connection parameters are written
    /// from.
    ///
    /// `[marti] public_host` when an operator set one, and the server's own
    /// name otherwise — a package taken off this server has to say which
    /// server it meant, and a container's hostname is not that.
    pub fn archive_host(&self) -> String {
        let config = self.context.config();

        config
            .marti
            .public_host
            .clone()
            .unwrap_or_else(|| config.server.name.clone())
    }

    /// The filename an archive is downloaded as.
    pub fn archive_filename(mission: &Mission) -> String {
        format!("{}_{}.zip", mission.name, mission.guid)
    }
}

/// The `<Configuration>` half of the manifest, in the order TAK writes it.
///
/// The order is not cosmetic: at least one importer reads the parameters
/// positionally when a name it expects is missing, so a reordered manifest is
/// one that imports with the wrong description.
fn configuration(mission: &Mission, host: &str) -> Manifest {
    Manifest::new(mission.guid.to_string(), mission.name.clone())
        .parameter("mission_guid", mission.guid.to_string())
        // Never the hash itself: an archive travels, and a password hash that
        // travelled would be a password offered for cracking.
        .parameter(
            "password_hash",
            if mission.is_password_protected() {
                "true"
            } else {
                ""
            },
        )
        .parameter(
            "creatorUid",
            mission.creator_uid.clone().unwrap_or_default(),
        )
        .parameter("create_time", time::cot_date(mission.create_time))
        .parameter("expiration", mission.expiration.unwrap_or(-1).to_string())
        .parameter("chatroom", mission.chat_room.clone().unwrap_or_default())
        .parameter("description", mission.description.clone())
        .parameter("tool", mission.tool.clone())
        .parameter("onReceiveImport", "true")
        .parameter("onReceiveDelete", "false")
        .parameter("mission_name", mission.name.clone())
        .parameter("mission_label", mission.name.clone())
        .parameter(
            "mission_uid",
            format!("{host}-{STREAM_PORT}-ssl-{}", mission.name),
        )
        .parameter("mission_server", format!("{host}:{STREAM_PORT}:ssl"))
}

/// Reads a stored blob, treating a missing one as "skip this entry".
///
/// A resource row whose bytes have been swept is not a reason to refuse the
/// whole archive: the rest of the mission is still worth taking away.
async fn read_content(
    content: &std::sync::Arc<crate::services::ContentStore>,
    hash: &str,
) -> Option<Vec<u8>> {
    use tokio::io::AsyncReadExt as _;

    let mut file = content.open(hash).await.ok()?;
    let mut bytes = Vec::new();

    file.read_to_end(&mut bytes).await.ok()?;

    Some(bytes)
}

/// A zip entry name with the characters a path may not carry removed.
///
/// A mission item's uid and a resource's filename both come from a client, and
/// either could name a path outside the archive if it were written verbatim.
fn sanitise(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '\0' => '_',
            other => other,
        })
        .collect::<String>()
        .trim_start_matches('.')
        .to_string()
}

/// Percent-encodes a filename for a `Content-Disposition` header.
///
/// Only the unreserved set survives; everything else — spaces, quotes, the
/// non-ASCII a mission name may carry — becomes `%XX`. A header that carried
/// them raw would be truncated at the first space by some clients and refused
/// outright by others.
pub fn encode_filename(name: &str) -> String {
    let mut encoded = String::with_capacity(name.len());

    for byte in name.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(*byte));
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }

    encoded
}

/// A minimal event for a filed item the CoT store no longer holds.
///
/// Built from the details cached when the item was filed, which is what a
/// listing renders it with anyway. A client importing the archive gets the
/// marker back where it was, with the callsign it had.
fn from_cached(item: &crate::db::repos::MissionUidRow) -> String {
    let details = item
        .details
        .clone()
        .and_then(|detail| serde_json::from_value::<super::dto::UidDetailsJson>(detail).ok())
        .unwrap_or_default();

    let kind = match details.kind.is_empty() {
        true => "a-u-G".to_string(),
        false => details.kind.clone(),
    };
    let at = rustak_cot::CotTime::from_datetime(item.timestamp);
    let location = details
        .location
        .unwrap_or(super::dto::LocationJson { lat: 0.0, lon: 0.0 });

    let mut event = rustak_cot::Event::builder(kind, item.uid.clone())
        .point(location.lat, location.lon)
        .time(at)
        .start(at)
        .stale_after(std::time::Duration::from_secs(86_400))
        .build();

    if let Some(callsign) = details.callsign {
        use rustak_cot::detail::TypedDetail as _;

        event
            .detail
            .push(rustak_cot::detail::Contact::new(callsign).to_element());
    }

    String::from_utf8_lossy(&rustak_cot::xml::write(&event)).into_owned()
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    async fn fixture() -> (AppContext, MissionService, Mission) {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let service = MissionService::new(context.clone());
        let row = context
            .db()
            .missions()
            .create(crate::db::repos::NewMission::new(
                "Operation Kettle",
                Role::Subscriber.as_str(),
            ))
            .await
            .unwrap();

        (context, service, Mission::from_row(row))
    }

    fn names(zip: &[u8]) -> Vec<String> {
        let mut archive = zip::ZipArchive::new(Cursor::new(zip)).expect("a readable archive");

        (0..archive.len())
            .map(|index| archive.by_index(index).unwrap().name().to_string())
            .collect()
    }

    fn manifest(zip: &[u8]) -> String {
        use std::io::Read as _;

        let mut archive = zip::ZipArchive::new(Cursor::new(zip)).expect("a readable archive");
        let mut file = archive
            .by_name("MANIFEST/manifest.xml")
            .expect("every package carries a manifest");
        let mut text = String::new();
        file.read_to_string(&mut text).unwrap();

        text
    }

    #[tokio::test]
    async fn an_empty_mission_still_archives_with_its_manifest() {
        let (_context, service, mission) = fixture().await;

        let zip = service.archive(&mission, "tak.example.com").await.unwrap();
        let rendered = manifest(&zip);

        assert!(names(&zip).contains(&"MANIFEST/manifest.xml".to_string()));
        assert!(rendered.contains(r#"<Parameter name="mission_name" value="Operation Kettle"/>"#));
        assert!(rendered.contains(
            r#"<Parameter name="mission_uid" value="tak.example.com-8443-ssl-Operation Kettle"/>"#
        ));
        assert!(
            rendered
                .contains(r#"<Parameter name="mission_server" value="tak.example.com:8443:ssl"/>"#)
        );
        assert!(rendered.contains(r#"<Parameter name="onReceiveImport" value="true"/>"#));
        assert!(rendered.contains(r#"<Parameter name="onReceiveDelete" value="false"/>"#));
    }

    #[tokio::test]
    async fn the_manifest_names_the_missions_channels_and_the_owner_role() {
        let (context, service, _mission) = fixture().await;
        let row = context
            .db()
            .missions()
            .update(
                1,
                crate::db::repos::MissionPatch {
                    groups: Some(vec!["Blue".to_string()]),
                    ..crate::db::repos::MissionPatch::default()
                },
            )
            .await
            .unwrap()
            .unwrap();

        let zip = service
            .archive(&Mission::from_row(row), "tak.example.com")
            .await
            .unwrap();
        let rendered = manifest(&zip);

        assert!(rendered.contains(r#"<Group name="Blue"/>"#));
        assert!(rendered.contains(r#"<Role name="MISSION_OWNER">"#));
        assert!(rendered.contains(r#"<Permission name="MISSION_READ"/>"#));
    }

    #[tokio::test]
    async fn a_password_protected_mission_archives_without_its_hash() {
        // The archive travels; a hash that travelled would be a password
        // offered up for cracking.
        let (context, service, _mission) = fixture().await;
        let row = context
            .db()
            .missions()
            .update(
                1,
                crate::db::repos::MissionPatch {
                    password_hash: Some(Some("$argon2id$v=19$secret".to_string())),
                    ..crate::db::repos::MissionPatch::default()
                },
            )
            .await
            .unwrap()
            .unwrap();

        let rendered = manifest(
            &service
                .archive(&Mission::from_row(row), "tak.example.com")
                .await
                .unwrap(),
        );

        assert!(rendered.contains(r#"<Parameter name="password_hash" value="true"/>"#));
        assert!(!rendered.contains("argon2id"));
    }

    #[tokio::test]
    async fn a_filed_item_becomes_a_cot_entry_named_after_its_uid() {
        let (context, service, mission) = fixture().await;
        let event = rustak_cot::Event::builder("a-f-G-U-C", "UID-MARKER")
            .point(51.5, -0.12)
            .build();

        let encoded = rustak_cot::codec::EncodedEvent::new(event);
        let principal = Principal::new(
            UserId::from(1),
            Username::parse("alice").unwrap(),
            PrincipalKind::Person,
            AuthMethod::SetupToken,
        );
        let record = crate::cot_store::CotRecord {
            // No account row in a mock context, and the foreign key is real.
            user_id: None,
            ..crate::cot_store::CotRecord::new(&encoded, &principal, None)
        };

        crate::cot_store::latest::upsert_batch(context.db(), vec![record])
            .await
            .expect("the store keeps the latest copy");
        context
            .db()
            .mission_contents()
            .upsert_uid(crate::db::repos::MissionUidRow::new(
                mission.id,
                "UID-MARKER",
                chrono::Utc::now(),
            ))
            .await
            .unwrap();

        let zip = service.archive(&mission, "tak.example.com").await.unwrap();

        assert!(
            names(&zip).contains(&"cot/UID-MARKER.cot".to_string()),
            "{:?}",
            names(&zip),
        );
        assert!(manifest(&zip).contains(r#"zipEntry="cot/UID-MARKER.cot""#));
    }

    #[test]
    fn a_uid_that_looks_like_a_path_cannot_escape_the_archive() {
        assert_eq!(sanitise("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(sanitise("c:\\windows\\system32"), "c__windows_system32");
        assert_eq!(sanitise("plain.pdf"), "plain.pdf");
    }

    #[test]
    fn a_filename_is_percent_encoded_for_the_header() {
        assert_eq!(
            encode_filename("Operation Kettle.zip"),
            "Operation%20Kettle.zip",
        );
        assert_eq!(encode_filename("a\"b"), "a%22b");
        assert_eq!(encode_filename("plain-name_1.zip"), "plain-name_1.zip");
    }

    #[test]
    fn the_download_is_named_after_the_mission_and_its_guid() {
        let name = MissionService::archive_filename(&Mission {
            id: 1,
            guid: uuid::Uuid::nil(),
            name: "Operation Kettle".to_string(),
            description: String::new(),
            chat_room: None,
            base_layer: None,
            bbox: None,
            bounding_polygon: Vec::new(),
            path: None,
            classification: None,
            tool: "public".to_string(),
            keywords: Vec::new(),
            creator_uid: None,
            create_time: chrono::DateTime::UNIX_EPOCH,
            last_edited: None,
            default_role: Role::Subscriber,
            invite_only: false,
            password_hash: None,
            expiration: None,
            groups: Vec::new(),
            parent_id: None,
            deleted_at: None,
        });

        assert_eq!(
            name,
            "Operation Kettle_00000000-0000-0000-0000-000000000000.zip"
        );
    }
}
