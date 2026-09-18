//! Replacing the keywords on a mission, a filed item or a filed resource.
//!
//! Three targets, one operation: keywords are always *replaced* rather than
//! merged, because that is what every client's tag editor sends and because a
//! merge would make removing the last keyword impossible.
//!
//! The mission-level routes have no GUID spelling upstream and CloudTAK falls
//! back to the name family for them, so they are mounted by name only.

use crate::db::repos::MissionPatch;
use crate::marti::MartiError;

use super::model::{KeywordTarget, Mission};
use super::service::MissionService;

impl MissionService {
    /// Replaces the keywords on whatever the request named.
    ///
    /// Returns the mission either way, because all three routes answer with a
    /// mission payload — a client that tagged an item still refreshes the
    /// whole Data Sync from the response.
    ///
    /// # Errors
    ///
    /// [`MartiError::NotFound`] when the item or resource named is not filed
    /// under this mission.
    pub async fn set_keywords(
        &self,
        mission: &Mission,
        target: KeywordTarget,
        keywords: Vec<String>,
        creator_uid: Option<&str>,
    ) -> Result<Mission, MartiError> {
        match target {
            KeywordTarget::Mission => {
                let updated = self
                    .db()
                    .missions()
                    .update(
                        mission.id,
                        MissionPatch {
                            keywords: Some(normalise(keywords)),
                            ..MissionPatch::default()
                        },
                    )
                    .await?
                    .ok_or_else(|| MartiError::NotFound(format!("Mission {}", mission.name)))?;

                // Broadcast rather than addressed: a change to the mission
                // itself is visible to people who have not subscribed to it.
                let updated = Mission::from_row(updated);
                self.notify_broadcast(&updated, crate::stream::ChangeKind::Keyword, creator_uid);

                Ok(updated)
            }
            KeywordTarget::Uid(uid) => {
                let filed = self
                    .db()
                    .mission_contents()
                    .set_uid_keywords(mission.id, uid.clone(), normalise(keywords))
                    .await?;

                if !filed {
                    return Err(MartiError::NotFound(format!("Mission item {uid}")));
                }

                self.notify_subscribers(
                    mission,
                    crate::stream::ChangeKind::UidKeyword,
                    creator_uid,
                )
                .await?;

                Ok(mission.clone())
            }
            KeywordTarget::Hash(hash) => {
                let changed = self
                    .db()
                    .resources()
                    .set_keywords(&hash, normalise(keywords))
                    .await?;

                if changed == 0 {
                    return Err(MartiError::NotFound(format!("Resource {hash}")));
                }

                self.notify_subscribers(
                    mission,
                    crate::stream::ChangeKind::ResourceKeyword,
                    creator_uid,
                )
                .await?;

                Ok(mission.clone())
            }
        }
    }

    /// Removes one keyword from a mission, leaving the rest.
    ///
    /// # Errors
    ///
    /// As [`set_keywords`](Self::set_keywords).
    pub async fn remove_keyword(
        &self,
        mission: &Mission,
        keyword: &str,
        creator_uid: Option<&str>,
    ) -> Result<Mission, MartiError> {
        let kept: Vec<String> = mission
            .keywords
            .iter()
            .filter(|existing| !existing.eq_ignore_ascii_case(keyword))
            .cloned()
            .collect();

        self.set_keywords(mission, KeywordTarget::Mission, kept, creator_uid)
            .await
    }
}

/// Trims, drops the empties and removes duplicates, keeping the first spelling.
///
/// A client that sends `["a", "", " a "]` means one keyword, and storing three
/// would make the tag list grow every time somebody saved it.
fn normalise(keywords: Vec<String>) -> Vec<String> {
    let mut kept: Vec<String> = Vec::with_capacity(keywords.len());

    for keyword in keywords {
        let keyword = keyword.trim().to_string();

        if !keyword.is_empty()
            && !kept
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&keyword))
        {
            kept.push(keyword);
        }
    }

    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keywords_are_trimmed_deduplicated_and_kept_in_order() {
        assert_eq!(
            normalise(vec![
                " alpha ".to_string(),
                String::new(),
                "ALPHA".to_string(),
                "bravo".to_string(),
            ]),
            vec!["alpha".to_string(), "bravo".to_string()]
        );
    }

    #[test]
    fn an_empty_list_stays_empty() {
        assert!(normalise(Vec::new()).is_empty());
        assert!(normalise(vec!["  ".to_string()]).is_empty());
    }
}
