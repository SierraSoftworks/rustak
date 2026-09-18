//! The audit log.

use rustak_api::{AuditCategory, AuditRecord};

use crate::api::{ApiError, get_json};
// The fixtures themselves exist only in debug builds; the macro is always in
// scope so that a release build still compiles the call sites away.
#[cfg(debug_assertions)]
use crate::fixtures;
use crate::fixtures::demo;

/// How many records a page of the activity list asks for.
pub const DEFAULT_LIMIT: usize = 100;

/// The most recent audit records, newest first, optionally narrowed to one
/// category.
pub async fn list(
    category: Option<AuditCategory>,
    limit: usize,
) -> Result<Vec<AuditRecord>, ApiError> {
    demo!(Ok(fixtures::audit(category, limit)));

    let mut query = format!("/audit?limit={limit}");
    if let Some(category) = category {
        query.push_str(&format!("&category={}", category.as_str()));
    }

    get_json(&query).await
}
