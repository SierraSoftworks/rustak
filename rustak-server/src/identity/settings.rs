//! The settings the first-run wizard writes, and how they combine with the
//! configuration file.
//!
//! Two things can say what this server is called and where it is reached:
//! `[server]` in the configuration file, and the wizard. The file wins wherever
//! it says something, because an operator who has written a value into a file
//! they deploy does not expect a web form to have quietly replaced it — and the
//! wizard is how an installation that has *not* been configured by hand gets a
//! host name at all.
//!
//! Everything lives under one key in `settings` rather than a key per field, so
//! that reading "what is this server" is one row and one deserialisation, and
//! so a partially written wizard step cannot leave the two halves disagreeing.

use chrono::Utc;
use rustak_api::{ServerSettings, ServerSettingsRequest};
use rustak_core::prelude::*;

use crate::config::{Config, ServerConfig};
use crate::db::Database;

/// The `settings` row everything here reads and writes.
pub const SERVER_KEY: &str = "server";

/// How many hexadecimal characters a node identifier carries.
///
/// TAK's own node identifiers are 32-character hex strings, and CloudTAK shows
/// them verbatim, so ours is the same shape rather than a UUID with dashes.
const NODE_ID_CHARS: usize = 32;

/// Reads what the wizard has stored, or an empty record on a fresh install.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if the read fails or the stored row
/// is not a [`ServerSettings`].
pub async fn stored(db: &Database) -> Result<ServerSettings, Error> {
    Ok(db
        .settings()
        .get::<ServerSettings>(SERVER_KEY)
        .await?
        .unwrap_or_else(empty))
}

/// What this server actually is, with the configuration file taking precedence.
///
/// # Errors
///
/// As [`stored`].
pub async fn resolve(config: &Config, db: &Database) -> Result<ServerSettings, Error> {
    let stored = stored(db).await?;

    Ok(ServerSettings {
        name: if config.server.name == ServerConfig::default().name {
            stored.name
        } else {
            config.server.name.clone()
        },
        domains: if config.server.domains.is_empty() {
            stored.domains
        } else {
            config.server.domains.clone()
        },
        base_url: config.server.base_url.clone().or(stored.base_url),
        ..stored
    })
}

/// The externally visible base URL, from the configuration or the wizard.
///
/// [`None`] on a fresh install that has been told neither, which is the one
/// state where a request's own `Host` header is all we have to go on.
///
/// # Errors
///
/// As [`stored`].
pub async fn base_url(config: &Config, db: &Database) -> Result<Option<String>, Error> {
    let settings = resolve(config, db).await?;

    Ok(match (&settings.base_url, settings.canonical_domain()) {
        (Some(configured), _) => Some(configured.trim_end_matches('/').to_string()),
        (None, Some(domain)) => Some(format!("https://{domain}")),
        (None, None) => None,
    })
}

/// Records the wizard's answers, minting the node identifier on the first pass.
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when the request names no host, and a
/// [`human_errors::Kind::System`] error if the write fails.
pub async fn save(
    db: &Database,
    request: &ServerSettingsRequest,
    by: Option<&Username>,
) -> Result<ServerSettings, Error> {
    let domains: Vec<String> = request
        .domains
        .iter()
        .map(|domain| domain.trim().trim_end_matches('.').to_ascii_lowercase())
        .filter(|domain| !domain.is_empty())
        .collect();

    if domains.is_empty() {
        return Err(human_errors::user(
            "We need at least one host name to know what to put in enrolment packages and certificates.",
            &["Enter the host name devices will reach this server on, such as tak.example.com."],
        ));
    }

    let previous = stored(db).await?;
    let settings = ServerSettings {
        name: request.name.trim().to_string(),
        domains,
        base_url: request
            .base_url
            .as_deref()
            .map(|url| url.trim().trim_end_matches('/').to_string())
            .filter(|url| !url.is_empty()),
        node_id: previous.node_id.or_else(|| Some(node_id())),
        setup_completed_at: previous.setup_completed_at,
    };

    db.settings().set(SERVER_KEY, settings.clone(), by).await?;

    info!(
        domains = ?settings.domains,
        "Recorded the server's identity from the setup wizard."
    );

    Ok(settings)
}

/// Stamps the wizard as finished, after which its routes answer `410`.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if the write fails.
pub async fn complete(db: &Database, by: Option<&Username>) -> Result<ServerSettings, Error> {
    let mut settings = stored(db).await?;

    if settings.setup_completed_at.is_none() {
        settings.setup_completed_at = Some(Utc::now());
        settings.node_id = settings.node_id.or_else(|| Some(node_id()));

        db.settings().set(SERVER_KEY, settings.clone(), by).await?;
    }

    Ok(settings)
}

/// A record for an installation that has answered nothing yet.
fn empty() -> ServerSettings {
    ServerSettings {
        name: ServerConfig::default().name,
        domains: Vec::new(),
        base_url: None,
        node_id: None,
        setup_completed_at: None,
    }
}

/// A fresh node identifier.
fn node_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..NODE_ID_CHARS].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    async fn database() -> Database {
        Database::open_in_memory().await.unwrap()
    }

    fn request() -> ServerSettingsRequest {
        ServerSettingsRequest {
            name: "Hilltop TAK".to_string(),
            domains: vec!["TAK.Example.com.".to_string(), " ".to_string()],
            base_url: None,
        }
    }

    #[tokio::test]
    async fn a_fresh_install_has_answered_nothing() {
        let db = database().await;
        let settings = stored(&db).await.unwrap();

        assert!(settings.domains.is_empty());
        assert_eq!(settings.node_id, None);
        assert!(!settings.is_setup_complete());
    }

    #[tokio::test]
    async fn the_wizard_normalises_the_host_names_it_is_given() {
        // Somebody typing a name into a form will capitalise it, or paste it
        // with the trailing dot a DNS tool printed. Both are the same host.
        let db = database().await;
        let settings = save(&db, &request(), None).await.unwrap();

        assert_eq!(settings.domains, vec!["tak.example.com".to_string()]);
        assert_eq!(settings.name, "Hilltop TAK");
    }

    #[tokio::test]
    async fn a_host_name_is_required_because_everything_downstream_needs_one() {
        let db = database().await;
        let request = ServerSettingsRequest {
            domains: vec!["   ".to_string()],
            ..request()
        };

        assert!(save(&db, &request, None).await.is_err());
    }

    #[tokio::test]
    async fn the_node_identifier_is_minted_once_and_then_kept() {
        let db = database().await;

        let first = save(&db, &request(), None).await.unwrap();
        let minted = first.node_id.clone().expect("a node id is minted");
        assert_eq!(minted.len(), NODE_ID_CHARS);

        let second = save(&db, &request(), None).await.unwrap();
        assert_eq!(second.node_id, Some(minted));
    }

    #[tokio::test]
    async fn completing_the_wizard_is_recorded_once() {
        let db = database().await;

        save(&db, &request(), None).await.unwrap();

        let first = complete(&db, None).await.unwrap();
        let stamp = first.setup_completed_at.expect("completion is stamped");

        let second = complete(&db, None).await.unwrap();
        assert_eq!(second.setup_completed_at, Some(stamp));
    }

    #[tokio::test]
    async fn the_configuration_file_wins_wherever_it_says_something() {
        let db = database().await;
        save(&db, &request(), None).await.unwrap();

        let mut config = Config::default();
        config.server.domains = vec!["configured.example.com".to_string()];

        let resolved = resolve(&config, &db).await.unwrap();

        assert_eq!(
            resolved.domains,
            vec!["configured.example.com".to_string()],
            "a host name in the file is not something a web form gets to replace",
        );
        assert_eq!(
            resolved.name, "Hilltop TAK",
            "but a name the file left at its default is still the wizard's to set",
        );
    }

    #[tokio::test]
    async fn the_base_url_falls_back_to_the_canonical_host() {
        let db = database().await;
        save(&db, &request(), None).await.unwrap();

        assert_eq!(
            base_url(&Config::default(), &db).await.unwrap().as_deref(),
            Some("https://tak.example.com"),
        );

        let mut config = Config::default();
        config.server.base_url = Some("https://proxy.example.com:8443/".to_string());

        assert_eq!(
            base_url(&config, &db).await.unwrap().as_deref(),
            Some("https://proxy.example.com:8443"),
        );
    }

    #[tokio::test]
    async fn an_installation_that_knows_no_host_says_so() {
        let db = database().await;

        assert_eq!(base_url(&Config::default(), &db).await.unwrap(), None);
    }
}
