//! [`MissionService`]: everything a mission route does, minus the parsing.
//!
//! The route files below `marti/missions/` read query parameters and hand them
//! here as one value; nothing in them touches a repository, mints a token or
//! decides who may do what. That split is what lets the mission rules be tested
//! without an HTTP client, and what keeps each route file inside the crate's
//! file-length budget.
//!
//! # A mission is never really deleted
//!
//! `DELETE` sets `deleted_at` and appends a `DELETE_MISSION` change. A client
//! that still holds a token then gets `410 Gone` rather than `404`, which is
//! the difference between "forget this" and "you spelled it wrong" — and the
//! row is still there for the archive an operator asks for afterwards.

use rustak_api::identity::GroupName;
use rustak_core::identity::password;

use crate::db::Database;
use crate::db::repos::{MissionFilter, MissionPatch};
use crate::files::Viewer;
use crate::marti::{CiQuery, MartiError, MartiPrincipal, MissionRef};
use crate::prelude::*;

use super::model::{DEFAULT_PAGE_SIZE, DEFAULT_TOOL, Mission};
use super::roles::Role;

/// The change appended when a mission is created.
pub const CREATE_MISSION: &str = "CREATE_MISSION";

/// The change appended when a mission is deleted.
pub const DELETE_MISSION: &str = "DELETE_MISSION";

/// Everything a mission route does.
#[derive(Clone)]
pub struct MissionService {
    pub(super) context: AppContext,
}

impl MissionService {
    /// A service over one application context.
    pub fn new(context: AppContext) -> Self {
        Self { context }
    }

    /// The database, for the repositories this module reaches.
    pub(super) fn db(&self) -> &Database {
        self.context.db()
    }

    /// The mission this reference names.
    ///
    /// # Errors
    ///
    /// [`MartiError::NotFound`] when nothing holds that name or guid, and
    /// [`MartiError::Gone`] when it has been deleted.
    pub async fn resolve(&self, reference: &MissionRef) -> Result<Mission, MartiError> {
        let row = match reference {
            MissionRef::Name(name) => self.db().missions().by_name(name).await?,
            MissionRef::Guid(guid) => self.db().missions().by_guid(*guid).await?,
        };

        let label = match reference {
            MissionRef::Name(name) => name.clone(),
            MissionRef::Guid(guid) => guid.to_string(),
        };

        match row {
            None => Err(MartiError::NotFound(format!("Mission {label}"))),
            Some(row) if row.is_deleted() => {
                Err(MartiError::Gone(format!("Mission {label} was deleted")))
            }
            Some(row) => Ok(Mission::from_row(row)),
        }
    }

    /// The mission with this exact name, deleted rows included.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn by_name(&self, name: &str) -> Result<Option<Mission>, MartiError> {
        Ok(self
            .db()
            .missions()
            .by_name(name)
            .await?
            .map(Mission::from_row))
    }

    /// The application context, for the handles the stream side installs.
    pub fn context(&self) -> &AppContext {
        &self.context
    }

    /// One mission by primary key, for the parent and child routes.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn by_id(&self, id: i64) -> Result<Option<Mission>, MartiError> {
        Ok(self.db().missions().by_id(id).await?.map(Mission::from_row))
    }

    /// What the caller may see, by channel.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the channel index cannot be
    /// read.
    pub async fn viewer(&self, who: &MartiPrincipal) -> Result<Viewer, MartiError> {
        Ok(crate::files::viewer_for(self.db(), who.username(), who.principal()).await?)
    }

    /// The missions a caller may list, filtered, sorted and paged.
    ///
    /// Invite-only missions are never listed, whatever the flags say: an
    /// invitation is the only way to learn one exists.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if a read fails.
    pub async fn list(
        &self,
        who: &MartiPrincipal,
        filter: ListFilter,
    ) -> Result<Vec<Mission>, MartiError> {
        let viewer = self.viewer(who).await?;
        let rows = self
            .db()
            .missions()
            .list(MissionFilter::tool(filter.tool.clone()))
            .await?;

        let mut found: Vec<Mission> = rows
            .into_iter()
            .map(Mission::from_row)
            .filter(|mission| !mission.invite_only)
            .filter(|mission| filter.password_protected || !mission.is_password_protected())
            .filter(|mission| filter.default_role || mission.default_role == Role::Subscriber)
            .filter(|mission| viewer.can_read_groups(&mission.groups))
            .filter(|mission| matches_name(mission, filter.name_filter.as_deref()))
            .collect();

        if let Some(uid) = filter.uid_filter.clone() {
            let holding = self.db().mission_contents().missions_with_uid(uid).await?;

            found.retain(|mission| holding.contains(&mission.id));
        }

        sort_and_page(&mut found, &filter);

        Ok(found)
    }

    /// The missions filed under this one.
    ///
    /// # Errors
    ///
    /// A [`human_errors::Kind::System`] error if the read fails.
    pub async fn children(&self, mission: &Mission) -> Result<Vec<Mission>, MartiError> {
        Ok(self
            .db()
            .missions()
            .list(MissionFilter {
                parent_id: Some(mission.id),
                ..MissionFilter::default()
            })
            .await?
            .into_iter()
            .map(Mission::from_row)
            .collect())
    }

    /// Files a mission under another, or unfiles it.
    ///
    /// # Errors
    ///
    /// [`MartiError::NotFound`] when the child has gone away in the meantime.
    pub async fn set_parent(
        &self,
        child: &Mission,
        parent: Option<&Mission>,
    ) -> Result<Mission, MartiError> {
        let updated = self
            .db()
            .missions()
            .update(
                child.id,
                MissionPatch {
                    parent_id: Some(parent.map(|parent| parent.id)),
                    ..MissionPatch::default()
                },
            )
            .await?
            .ok_or_else(|| MartiError::NotFound(format!("Mission {}", child.name)))?;

        Ok(Mission::from_row(updated))
    }
}

/// Which missions a listing wants, after the caller's own visibility.
#[derive(Debug, Clone, PartialEq)]
pub struct ListFilter {
    /// The owning tool; `"public"` when the caller named none.
    pub tool: String,
    /// Whether password-protected missions are included.
    pub password_protected: bool,
    /// Whether missions whose default role is not `MISSION_SUBSCRIBER` are
    /// included.
    pub default_role: bool,
    /// A case-insensitive substring of the name.
    pub name_filter: Option<String>,
    /// A map item the mission must hold.
    pub uid_filter: Option<String>,
    /// `name` or `createTime`.
    pub sort: Option<String>,
    pub ascending: bool,
    /// Zero-indexed page, for `/pagedmissions`.
    pub page: Option<u32>,
    pub page_size: Option<u32>,
}

impl Default for ListFilter {
    fn default() -> Self {
        Self {
            tool: DEFAULT_TOOL.to_string(),
            password_protected: false,
            default_role: false,
            name_filter: None,
            uid_filter: None,
            sort: None,
            ascending: true,
            page: None,
            page_size: None,
        }
    }
}

impl ListFilter {
    /// Reads the parameters a listing carries.
    ///
    /// `paged` says which of the two defaults applies: the unpaged endpoint
    /// excludes password-protected and non-default-role missions unless asked,
    /// and the paged one includes them unless asked not to. That is genuinely
    /// how TAK Server behaves, and CloudTAK relies on both.
    ///
    /// # Errors
    ///
    /// [`MartiError::InvalidRequest`] for a page or page size that will not
    /// parse.
    pub fn from_query(query: &CiQuery, paged: bool) -> Result<Self, MartiError> {
        let flag = |key: &str, default: bool| match query.get(key) {
            Some(value) => value == "true",
            None => default,
        };

        Ok(Self {
            tool: query
                .get("tool")
                .map_or_else(|| DEFAULT_TOOL.to_string(), str::to_string),
            password_protected: flag("passwordProtected", paged),
            default_role: flag("defaultRole", paged),
            name_filter: query.get("nameFilter").map(str::to_string),
            uid_filter: query.get("uidFilter").map(str::to_string),
            sort: query.get("sort").map(str::to_string),
            ascending: flag("ascending", true),
            page: paged
                .then(|| query.parsed::<u32>("page"))
                .transpose()?
                .flatten(),
            page_size: match paged {
                true => Some(
                    query
                        .parsed::<u32>("pagesize")?
                        .unwrap_or(DEFAULT_PAGE_SIZE),
                ),
                false => None,
            },
        })
    }
}

/// Whether a mission's name contains a filter, ignoring case.
fn matches_name(mission: &Mission, filter: Option<&str>) -> bool {
    filter.is_none_or(|filter| {
        mission
            .name
            .to_lowercase()
            .contains(&filter.trim().to_lowercase())
    })
}

/// Applies a listing's sort order and page window in place.
fn sort_and_page(found: &mut Vec<Mission>, filter: &ListFilter) {
    match filter.sort.as_deref() {
        Some("createTime") => found.sort_by_key(|mission| mission.create_time),
        _ => found.sort_by_key(|mission| mission.name.to_lowercase()),
    }

    if !filter.ascending {
        found.reverse();
    }

    if let Some(size) = filter.page_size {
        let start = (filter.page.unwrap_or(0) as usize).saturating_mul(size as usize);

        *found = found
            .iter()
            .skip(start)
            .take(size as usize)
            .cloned()
            .collect();
    }
}

/// `-1` and anything below it mean "never", which is stored as nothing.
pub(super) fn expiration_of(requested: Option<i64>) -> Option<i64> {
    requested.filter(|seconds| *seconds > 0)
}

/// The channels a mission may be created in, given who is asking.
///
/// A caller who names channels they do not hold is refused — except when they
/// named exactly `__ANON__`, which is what a client sends when it means "the
/// default"; their own channels are substituted instead of failing a create
/// nobody asked to scope.
pub(super) fn resolve_groups(
    viewer: &Viewer,
    requested: Option<Vec<String>>,
) -> Result<Vec<String>, MartiError> {
    let requested = requested
        .filter(|groups| !groups.is_empty())
        .unwrap_or_else(|| vec![GroupName::ANON.to_string()]);

    if viewer.is_admin
        || requested
            .iter()
            .all(|group| viewer.held_groups.iter().any(|held| held == group))
    {
        return Ok(requested);
    }

    if requested.len() == 1 && requested[0] == GroupName::ANON {
        return Ok(viewer.held_groups.clone());
    }

    Err(MartiError::Forbidden(
        "a mission cannot be shared with a channel you are not a member of".to_string(),
    ))
}

/// Hashes a mission password, off the async worker threads.
pub(super) async fn hash_password(password: &str) -> Result<String, MartiError> {
    let hashed = password::hash_blocking(Secret::new(password)).await?;

    Ok(hashed.as_str().to_string())
}

/// Turns a unique-index violation on the name into the refusal a client reads.
pub(super) fn duplicate_name(err: Error) -> MartiError {
    if err.description().contains("UNIQUE") {
        return MartiError::Duplicate("a mission with that name already exists".to_string());
    }

    MartiError::from(err)
}
