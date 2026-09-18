//! `acme_accounts`: the key every order is signed with.
//!
//! An ACME account is a key pair the authority knows, not a password. Losing it
//! means every certificate it issued becomes unrenewable and a new account has
//! to be registered — which is why it is stored, sealed, beside the database
//! rather than regenerated at each start, and why the row is keyed by
//! (directory, contact): pointing an installation at staging and back again
//! finds the account it used last time instead of registering a third.
//!
//! # The row is written twice, for the same reason the certificate row is
//!
//! The credentials are sealed against
//! [`crate::crypto::SecretContext::AcmeAccount`],
//! whose identity is the row id SQLite assigns on insert. The row is therefore
//! inserted empty and filled in, and an empty row is treated as "no account".

use chrono::Utc;
use instant_acme::{Account, AccountCredentials, NewAccount};

use rustak_core::prelude::*;

use crate::config::AcmeConfig;
use crate::crypto::{Sealed, SecretContext, SecretStore};
use crate::db::Database;
use crate::db::row::{Timestamp, json_col, to_json};

use super::transport::SharedClient;

/// Advice for an account we could not register.
const ADVICE_ACCOUNT: &[&str] = &[
    "Check that this server can reach the directory URL under `[acme] directory`.",
    "Check that `[acme] contact` is an address the authority will accept.",
];

/// A stored account: the row id, and the credentials sealed under it.
struct StoredAccount {
    id: i64,
    credentials: Option<Sealed>,
}

/// Loads the ACME account for this directory and contact, registering one the
/// first time.
///
/// `client` is the HTTP client the exchange goes through; a deployment passes
/// [`Services::http_client`](crate::services::Services::http_client).
///
/// # Errors
///
/// A [`human_errors::Kind::User`] error when the terms of service have not been
/// accepted, and a [`human_errors::Kind::System`] error when the authority
/// cannot be reached, refuses the registration, or the stored credentials
/// cannot be opened.
#[instrument("pki.acme.account", skip_all, err(Display))]
pub async fn ensure(
    db: &Database,
    secrets: &SecretStore,
    config: &AcmeConfig,
    client: reqwest::Client,
) -> Result<Account, Error> {
    if !config.accept_tos {
        return Err(human_errors::user(
            format!(
                "`[acme] accept_tos` is not set, so no account can be registered with {}.",
                config.directory
            ),
            &["Read the authority's terms of service, then set `[acme] accept_tos = true`."],
        ));
    }

    let directory = config.directory.url().to_string();
    let contact = config.contacts().first().cloned().unwrap_or_default();
    let stored = reserve(db, &directory, &contact).await?;

    if let Some(sealed) = &stored.credentials {
        let credentials: AccountCredentials =
            secrets.open_json(sealed, SecretContext::AcmeAccount { account: stored.id })?;

        debug!(account = stored.id, "Reusing the stored ACME account.");

        return Account::builder_with_http(SharedClient::boxed(client))
            .from_credentials(credentials)
            .await
            .map_err(|err| failed("The stored ACME account could not be loaded.", &err));
    }

    register(db, secrets, config, client, stored.id, &directory, &contact).await
}

/// Registers a new account and stores its credentials in the reserved row.
async fn register(
    db: &Database,
    secrets: &SecretStore,
    config: &AcmeConfig,
    client: reqwest::Client,
    id: i64,
    directory: &str,
    contact: &str,
) -> Result<Account, Error> {
    let contacts = config.contacts();
    let contacts: Vec<&str> = contacts.iter().map(String::as_str).collect();

    let (account, credentials) = Account::builder_with_http(SharedClient::boxed(client))
        .create(
            &NewAccount {
                contact: &contacts,
                terms_of_service_agreed: true,
                only_return_existing: false,
            },
            directory.to_string(),
            None,
        )
        .await
        .map_err(|err| failed("We could not register an ACME account.", &err))?;

    let sealed = secrets.seal_json(&credentials, SecretContext::AcmeAccount { account: id })?;
    let account_url = account.id().to_string();

    db.write(move |tx| {
        tx.execute(
            "UPDATE acme_accounts SET account_url = ?2, credentials_sealed = ?3 WHERE id = ?1",
            rusqlite::params![id, account_url, to_json(&sealed)?],
        )
    })
    .await?;

    info!(
        account = id,
        directory,
        // The contact is the operator's own address, already in the
        // configuration file; the key is what is never logged.
        contact = %redacted(contact),
        "Registered a new ACME account."
    );

    Ok(account)
}

/// The row for this directory and contact, creating an empty one if needed.
async fn reserve(db: &Database, directory: &str, contact: &str) -> Result<StoredAccount, Error> {
    let directory = directory.to_string();
    let contact = contact.to_string();

    db.write(move |tx| {
        tx.execute(
            "INSERT INTO acme_accounts (directory_url, contact, account_url, credentials_sealed, created_at) \
             VALUES (?1, ?2, '', '{}', ?3) \
             ON CONFLICT (directory_url, contact) DO NOTHING",
            rusqlite::params![directory, contact, Timestamp::from(Utc::now())],
        )?;

        tx.query_one(
            "SELECT id, credentials_sealed FROM acme_accounts WHERE directory_url = ?1 AND contact = ?2",
            rusqlite::params![directory, contact],
            |row| {
                let id: i64 = row.get(0)?;
                let raw: String = row.get(1)?;

                Ok(StoredAccount {
                    id,
                    credentials: match raw.as_str() {
                        "{}" | "" => None,
                        _ => Some(json_col(row, 1)?),
                    },
                })
            },
        )
    })
    .await
}

/// An ACME failure, as a system error that keeps the authority's own words.
///
/// The authority's problem document is what an operator has to read — "the
/// domain does not resolve", "too many certificates already issued" — so it is
/// carried through rather than generalised away.
pub fn failed(what: &str, err: &instant_acme::Error) -> Error {
    human_errors::system(format!("{what} {err}"), ADVICE_ACCOUNT)
}

/// A contact address with its local part hidden, for a log line.
fn redacted(contact: &str) -> String {
    match contact.rsplit_once('@') {
        Some((_, domain)) => format!("…@{domain}"),
        None => "…".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AcmeDirectory;

    async fn database() -> Database {
        Database::open_in_memory().await.unwrap()
    }

    fn config(directory: &str) -> AcmeConfig {
        AcmeConfig {
            enabled: true,
            directory: AcmeDirectory::Url(directory.to_string()),
            contact: Some("ops@example.com".to_string()),
            accept_tos: true,
            domains: vec!["tak.example.com".to_string()],
            ..AcmeConfig::default()
        }
    }

    #[tokio::test]
    async fn terms_that_were_never_accepted_stop_the_order_before_it_starts() {
        let db = database().await;
        let secrets = SecretStore::ephemeral();
        let mut config = config("https://ca.example.com/directory");
        config.accept_tos = false;

        // `Account` has no `Debug`, so `unwrap_err` is not available here.
        let refused = ensure(&db, &secrets, &config, reqwest::Client::new())
            .await
            .err()
            .expect("terms that were never accepted cannot produce an account");

        assert!(refused.is(human_errors::Kind::User));
        assert!(refused.description().contains("accept_tos"));
    }

    #[tokio::test]
    async fn each_directory_and_contact_gets_its_own_row() {
        let db = database().await;

        let staging = reserve(&db, "https://staging.example.com/d", "mailto:a@example.com")
            .await
            .unwrap();
        let production = reserve(&db, "https://acme.example.com/d", "mailto:a@example.com")
            .await
            .unwrap();
        let again = reserve(&db, "https://staging.example.com/d", "mailto:a@example.com")
            .await
            .unwrap();

        assert_ne!(staging.id, production.id);
        assert_eq!(staging.id, again.id, "the same account is found again");
        assert!(staging.credentials.is_none());
    }

    #[test]
    fn a_contact_is_never_logged_whole() {
        assert_eq!(redacted("mailto:ops@example.com"), "…@example.com");
        assert_eq!(redacted("nothing"), "…");
    }
}
