//! Who is registered, what they said about themselves, and what we configured
//! them with.
//!
//! The `services` row is the *registration*; the `users` row of kind `service`
//! is the *account*. A sidecar re-registers on every start, so registering is an
//! upsert keyed on the service's name — which is also why the one rule this
//! module really enforces is that a name belongs to the account that first
//! claimed it. Without it, any service token could take over the registration of
//! any other service simply by naming it, and the admin UI would then attribute
//! one plugin's health to another.
//!
//! Everything here is written against [`Services`] rather than against actix, so
//! `web::api::services` stays a parsing layer and these rules can be tested
//! without a request.

use rustak_api::{ServiceDescriptor, ServiceStatus, ServiceSummary};
use rustak_core::prelude::*;

use crate::db::repos::{NewService, ServiceRow, UserRow};
use crate::prelude::*;

/// Why a control-API call could not be answered.
///
/// Three variants rather than a [`human_errors::Error`], because the two that
/// are not ours map to statuses a caller branches on — `409` for a name that is
/// somebody else's, `404` for a service that is not registered — and rendering
/// both as `400` would make "try a different name" indistinguishable from "that
/// service is gone".
#[derive(Debug)]
pub enum RegistryError {
    /// The name is registered to a different account.
    Taken(ServiceName),

    /// Nothing is registered under that name.
    Unknown(ServiceName),

    /// Something of ours failed.
    Unavailable(Error),
}

impl From<Error> for RegistryError {
    fn from(err: Error) -> Self {
        Self::Unavailable(err)
    }
}

/// Registers a service, or refreshes the registration of one that restarted.
///
/// The account is the caller's own: a service registers *itself*, so there is no
/// way to spell "register that one over there". What the descriptor may change
/// is what the sidecar knows about itself — its display name, version,
/// capabilities and endpoints. Its configuration and its last known health
/// survive a restart, because neither is the sidecar's to reset.
///
/// # Errors
///
/// [`RegistryError::Taken`] when another account already holds that name, and
/// [`RegistryError::Unavailable`] when a write fails.
#[instrument("plugins.register", skip_all, fields(service = %descriptor.name, account = %account.username), err(Debug))]
pub async fn register(
    services: &impl Services,
    account: &UserRow,
    descriptor: &ServiceDescriptor,
) -> Result<ServiceRow, RegistryError> {
    let db = services.db();

    if let Some(existing) = db.services().get_by_name(&descriptor.name).await?
        && existing.user_id != account.id
    {
        warn!(
            service = %descriptor.name,
            "An account tried to register under a name another account holds."
        );

        return Err(RegistryError::Taken(descriptor.name.clone()));
    }

    let row = db
        .services()
        .register(NewService {
            name: descriptor.name.clone(),
            user_id: account.id,
            display_name: descriptor.display_name.clone(),
            description: None,
            version: descriptor.version.clone(),
            capabilities: descriptor.capabilities.clone(),
            endpoints: Some(descriptor.endpoints.clone()),
        })
        .await?;

    record(
        services,
        "registered",
        &row,
        &account.username,
        format!("The service '{}' registered.", row.name),
    )
    .await;

    Ok(row)
}

/// The service registered under `name`.
///
/// # Errors
///
/// [`RegistryError::Unknown`] when nothing is registered under it, and
/// [`RegistryError::Unavailable`] when the read fails.
pub async fn require(
    services: &impl Services,
    name: &ServiceName,
) -> Result<ServiceRow, RegistryError> {
    services
        .db()
        .services()
        .get_by_name(name)
        .await?
        .ok_or_else(|| RegistryError::Unknown(name.clone()))
}

/// Every registration, as the admin UI lists them.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if the read fails.
pub async fn list(services: &impl Services) -> Result<Vec<ServiceSummary>, Error> {
    Ok(services
        .db()
        .services()
        .list()
        .await?
        .iter()
        .map(summary)
        .collect())
}

/// Removes a registration, leaving the account that holds it alone.
///
/// Deliberately not a cascade: the account is what holds the certificate and the
/// channel memberships, and an operator who removes a stale registration is
/// almost never asking to revoke a sidecar's identity as well. Removing the
/// account does take the registration with it — that direction is the foreign
/// key's.
///
/// # Errors
///
/// [`RegistryError::Unknown`] when nothing is registered under that name.
#[instrument("plugins.deregister", skip_all, fields(service = %name), err(Debug))]
pub async fn remove(
    services: &impl Services,
    name: &ServiceName,
    actor: &Username,
) -> Result<(), RegistryError> {
    let row = require(services, name).await?;

    services.db().services().delete(row.id).await?;
    record(
        services,
        "deregistered",
        &row,
        actor,
        format!("The service '{}' was removed.", row.name),
    )
    .await;

    Ok(())
}

/// Replaces a service's configuration document.
///
/// An administrator's write and the service's read are the two halves of the
/// per-service key/value store: a plugin fetches this at start-up and on every
/// tick, so a setting changed in the admin UI reaches the sidecar without
/// anybody restarting it.
///
/// # Errors
///
/// [`RegistryError::Unknown`] when nothing is registered under that name.
#[instrument("plugins.configure", skip_all, fields(service = %name), err(Debug))]
pub async fn configure(
    services: &impl Services,
    name: &ServiceName,
    config: serde_json::Value,
    actor: &Username,
) -> Result<ServiceRow, RegistryError> {
    let row = require(services, name).await?;

    services.db().services().set_config(row.id, config).await?;
    record(
        services,
        "configured",
        &row,
        actor,
        format!("The configuration of '{}' was changed.", row.name),
    )
    .await;

    require(services, name).await
}

/// The listing form of one row.
pub fn summary(row: &ServiceRow) -> ServiceSummary {
    ServiceSummary {
        id: row.id,
        descriptor: ServiceDescriptor {
            name: row.name.clone(),
            display_name: row.display_name.clone(),
            version: row.version.clone(),
            capabilities: row.capabilities.clone(),
            endpoints: row.endpoints.clone().unwrap_or_default(),
        },
        status: ServiceStatus {
            state: row.status,
            message: row.status_message.clone(),
            last_heartbeat_at: row.last_heartbeat_at,
        },
        registered_at: row.created_at,
        metrics: row.metrics.clone(),
    }
}

/// Writes the audit entry for something that happened to a registration.
///
/// The detail names the version and the capabilities rather than the whole
/// descriptor, because the endpoints a sidecar reports are the one part of it
/// that changes on every redeployment and would bury the rest.
async fn record(
    services: &impl Services,
    action: &'static str,
    row: &ServiceRow,
    actor: &Username,
    message: String,
) {
    let entry = crate::db::AuditEntry::new(
        rustak_api::AuditCategory::Service,
        action,
        rustak_api::AuditOutcome::Success,
    )
    .subject(&row.name)
    .actor(actor)
    .message(message)
    .detail(serde_json::json!({
        "version": row.version,
        "capabilities": row.capabilities,
    }));

    if let Err(err) = services.audit().record(entry).await {
        warn!(error = %err, "Could not record a service registry change.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repos::NewUser;

    async fn account(context: &AppContext, name: &str) -> UserRow {
        context
            .db()
            .users()
            .create(NewUser::service(Username::parse(name).unwrap()))
            .await
            .unwrap()
    }

    fn descriptor(name: &str) -> ServiceDescriptor {
        ServiceDescriptor {
            version: Some("1.2.3".into()),
            ..ServiceDescriptor::new(ServiceName::parse(name).unwrap())
        }
    }

    #[tokio::test]
    async fn registering_twice_refreshes_the_same_row() {
        // A sidecar registers on every start, so a restart must not leave two
        // rows or fail the second start-up.
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let account = account(&context, "svc.weather").await;

        let first = register(&context, &account, &descriptor("weather"))
            .await
            .unwrap();
        let again = register(
            &context,
            &account,
            &ServiceDescriptor {
                version: Some("1.3.0".into()),
                ..descriptor("weather")
            },
        )
        .await
        .unwrap();

        assert_eq!(first.id, again.id);
        assert_eq!(again.version.as_deref(), Some("1.3.0"));
        assert_eq!(list(&context).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn one_service_cannot_register_under_another_services_name() {
        // The rule this module exists for: a name belongs to the account that
        // claimed it, or a service token is a way to impersonate any sidecar.
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let weather = account(&context, "svc.weather").await;
        let adsb = account(&context, "svc.adsb").await;

        register(&context, &weather, &descriptor("weather"))
            .await
            .unwrap();
        let stolen = register(&context, &adsb, &descriptor("weather")).await;

        assert!(matches!(stolen, Err(RegistryError::Taken(name)) if name.as_str() == "weather"));
        assert_eq!(list(&context).await.unwrap().len(), 1);
        assert_eq!(
            weather_row(&context).await.user_id,
            weather.id,
            "the registration still belongs to the account that claimed it",
        );
    }

    #[tokio::test]
    async fn a_configuration_survives_the_service_restarting() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let account = account(&context, "svc.weather").await;
        let name = ServiceName::parse("weather").unwrap();
        register(&context, &account, &descriptor("weather"))
            .await
            .unwrap();

        configure(
            &context,
            &name,
            serde_json::json!({ "interval": 60 }),
            &account.username,
        )
        .await
        .unwrap();
        register(&context, &account, &descriptor("weather"))
            .await
            .unwrap();

        let read = require(&context, &name).await.unwrap();
        assert_eq!(read.config, serde_json::json!({ "interval": 60 }));
    }

    #[tokio::test]
    async fn removing_a_registration_leaves_the_account_behind() {
        let context = AppContext::new_mock(|_| {}).await.unwrap();
        let account = account(&context, "svc.weather").await;
        let name = ServiceName::parse("weather").unwrap();
        register(&context, &account, &descriptor("weather"))
            .await
            .unwrap();

        remove(&context, &name, &account.username).await.unwrap();

        assert!(list(&context).await.unwrap().is_empty());
        assert!(
            context
                .db()
                .users()
                .get(account.id)
                .await
                .unwrap()
                .is_some(),
            "the certificate and the channels stay",
        );
        assert!(matches!(
            remove(&context, &name, &account.username).await,
            Err(RegistryError::Unknown(_))
        ));
    }

    async fn weather_row(context: &AppContext) -> ServiceRow {
        require(context, &ServiceName::parse("weather").unwrap())
            .await
            .unwrap()
    }
}
