//! `[retention]` — how long we keep things.
//!
//! Every horizon here comes in one of two shapes: an age, or a cap. Where both
//! are given they are both enforced, because either alone leaves a gap — an age
//! limit lets a busy installation fill the disk inside the window, and a cap
//! lets a quiet one keep data indefinitely. The caps are per device or per
//! account rather than global, so that one noisy source cannot evict everybody
//! else's history.

use serde::{Deserialize, Serialize};

fn default_cot_history() -> chrono::Duration {
    chrono::Duration::days(7)
}

/// Frames kept per device before older segments are pruned, whatever their age.
fn default_cot_history_max_rows() -> u64 {
    2_000_000
}

fn default_audit() -> chrono::Duration {
    chrono::Duration::days(90)
}

fn default_audit_max_entries() -> u64 {
    100_000
}

fn default_archived_missions() -> chrono::Duration {
    chrono::Duration::days(30)
}

/// How long a soft-deleted mission is kept before it is really gone.
///
/// A mission that has been deleted still answers `410 Gone` rather than `404`,
/// which is what tells a client holding a stale Data Sync that the mission was
/// removed rather than that it is looking in the wrong place. That distinction
/// is only useful while clients might still ask, so the row is purged after a
/// fortnight — long enough to cover a device that was switched off for a
/// holiday, short enough that a deleted mission is not kept indefinitely.
fn default_missions_purge_after() -> chrono::Duration {
    chrono::Duration::days(14)
}

/// How long an uploaded blob nothing refers to is kept.
///
/// Not zero, because an upload is referenced a moment *after* it is stored: a
/// collector that ran between the two would delete the file out from under the
/// request that had just written it.
fn default_content_orphans() -> chrono::Duration {
    chrono::Duration::hours(24)
}

/// `[retention]`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionConfig {
    /// How long CoT history is kept. Whole segment files are pruned, so the
    /// effective horizon is this plus the tail of the segment that spans it.
    #[serde(
        default = "default_cot_history",
        with = "rustak_core::config::duration::humane"
    )]
    pub cot_history: chrono::Duration,

    /// The most history frames kept for any one device.
    #[serde(default = "default_cot_history_max_rows")]
    pub cot_history_max_rows: u64,

    /// How long audit entries are kept.
    #[serde(
        default = "default_audit",
        with = "rustak_core::config::duration::humane"
    )]
    pub audit: chrono::Duration,

    /// The most audit entries kept for any one account.
    #[serde(default = "default_audit_max_entries")]
    pub audit_max_entries: u64,

    /// How long an archived mission is kept before it is deleted.
    #[serde(
        default = "default_archived_missions",
        with = "rustak_core::config::duration::humane"
    )]
    pub archived_missions: chrono::Duration,

    /// How long a soft-deleted mission keeps answering `410` before its row,
    /// its changes and its subscriptions are removed for good.
    #[serde(
        default = "default_missions_purge_after",
        with = "rustak_core::config::duration::humane"
    )]
    pub missions_purge_after: chrono::Duration,

    /// How long a stored blob that nothing refers to is kept.
    #[serde(
        default = "default_content_orphans",
        with = "rustak_core::config::duration::humane"
    )]
    pub content_orphans: chrono::Duration,
}

impl Default for RetentionConfig {
    /// Written out rather than derived; see [`ServerConfig::default`].
    ///
    /// [`ServerConfig::default`]: super::ServerConfig::default
    fn default() -> Self {
        Self {
            cot_history: default_cot_history(),
            cot_history_max_rows: default_cot_history_max_rows(),
            audit: default_audit(),
            audit_max_entries: default_audit_max_entries(),
            archived_missions: default_archived_missions(),
            missions_purge_after: default_missions_purge_after(),
            content_orphans: default_content_orphans(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_section_is_the_written_out_default() {
        let parsed: RetentionConfig = toml::from_str("").unwrap();

        assert_eq!(parsed, RetentionConfig::default());
        assert_eq!(parsed.cot_history, chrono::Duration::days(7));
        assert_eq!(parsed.cot_history_max_rows, 2_000_000);
        assert_eq!(parsed.audit, chrono::Duration::days(90));
        assert_eq!(parsed.audit_max_entries, 100_000);
        assert_eq!(parsed.archived_missions, chrono::Duration::days(30));
        assert_eq!(parsed.missions_purge_after, chrono::Duration::days(14));
        assert_eq!(parsed.content_orphans, chrono::Duration::hours(24));
    }

    #[test]
    fn a_negative_horizon_is_refused_rather_than_treated_as_zero() {
        // "Keep audit entries for minus ninety days" would otherwise prune
        // everything on the first run of the job.
        let Err(err) = toml::from_str::<RetentionConfig>(r#"audit = "-90d""#) else {
            panic!("a negative retention horizon should be refused");
        };

        assert!(err.to_string().contains("negative"), "{err}");
    }

    #[test]
    fn a_misspelled_key_is_refused_rather_than_ignored() {
        let Err(err) = toml::from_str::<RetentionConfig>(r#"audit_days = 90"#) else {
            panic!("an unknown key should be refused");
        };

        assert!(err.to_string().contains("audit_days"), "{err}");
    }
}
