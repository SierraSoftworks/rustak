//! Device profiles: what a client is configured with when it enrols and every
//! time it connects.
//!
//! Administrative throughout. A profile is a bundle of typed preference
//! entries and uploaded files, and the two flags decide when it is delivered
//! while the channel list decides to whom.
//!
//! # Preferences are replaced, never patched
//!
//! `PUT /profiles/{id}/prefs` takes the whole list. The `.pref` document a
//! device receives renders entries in the order they were stored, so a delta
//! would have to carry a position as well as a value — and two administrators
//! editing at once would interleave rather than conflict. Sending the whole
//! list means the second save loses to the first *visibly*, which is the
//! failure worth having.

use rustak_api::{
    PrefCatalogEntry, PrefEntry, Profile, ProfileCreate, ProfileFile, ProfileId, ProfileUpdate,
};

use crate::api::download::{Download, get_download, upload};
use crate::api::{ApiError, delete_empty, get_json, patch_json, post_json, put_json};
#[cfg(debug_assertions)]
use crate::fixtures;
use crate::fixtures::demo;

/// Every profile this installation has.
pub async fn list() -> Result<Vec<Profile>, ApiError> {
    demo!(Ok(fixtures::profiles()));

    get_json("/profiles").await
}

/// One profile.
pub async fn get(id: ProfileId) -> Result<Profile, ApiError> {
    demo!(fixtures::profile(id));

    get_json(&format!("/profiles/{}", id.get())).await
}

/// Creates one. Everything but the name has a default.
pub async fn create(request: &ProfileCreate) -> Result<Profile, ApiError> {
    demo!(fixtures::create_profile(request));

    post_json("/profiles", request).await
}

/// Changes one. Every absent field is left alone, and a change that would do
/// nothing is refused by the server rather than silently accepted.
pub async fn patch(id: ProfileId, change: &ProfileUpdate) -> Result<Profile, ApiError> {
    demo!(fixtures::patch_profile(id, change));

    patch_json(&format!("/profiles/{}", id.get()), change).await
}

/// Deletes one, with its preferences and its files.
pub async fn remove(id: ProfileId) -> Result<(), ApiError> {
    demo!(fixtures::delete_profile(id));

    delete_empty(&format!("/profiles/{}", id.get())).await
}

/// The preference entries, in delivery order.
pub async fn prefs(id: ProfileId) -> Result<Vec<PrefEntry>, ApiError> {
    demo!(fixtures::profile_prefs(id));

    get_json(&format!("/profiles/{}/prefs", id.get())).await
}

/// Replaces them all. The server refuses a blank key, a duplicate key, or a
/// value the entry's class could not hold.
pub async fn set_prefs(id: ProfileId, entries: &[PrefEntry]) -> Result<Vec<PrefEntry>, ApiError> {
    demo!(fixtures::set_profile_prefs(id, entries));

    put_json(&format!("/profiles/{}/prefs", id.get()), &entries.to_vec()).await
}

/// The curated catalogue the editor autocompletes keys from.
///
/// Not every preference ATAK has — a list of thousands would be a worse
/// starting point than none — but the twenty an operator configuring a server
/// actually reaches for, each with its class and ATAK's own default.
pub async fn pref_catalog() -> Result<Vec<PrefCatalogEntry>, ApiError> {
    demo!(Ok(fixtures::pref_catalog()));

    get_json("/profiles/pref-catalog").await
}

/// The files attached to a profile, named by the path a device stores them at.
pub async fn files(id: ProfileId) -> Result<Vec<ProfileFile>, ApiError> {
    demo!(fixtures::profile_files(id));

    get_json(&format!("/profiles/{}/files", id.get())).await
}

/// Attaches a file. `name` overrides the delivered path when the caller has
/// one to give.
pub async fn upload_file(
    id: ProfileId,
    file: &web_sys::File,
    name: Option<&str>,
) -> Result<ProfileFile, ApiError> {
    demo!(fixtures::add_profile_file(
        id,
        name.filter(|name| !name.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| file.name()),
        file.size() as u64,
    ));

    upload(&format!("/profiles/{}/files", id.get()), file, name).await
}

/// Detaches one.
pub async fn delete_file(id: ProfileId, file: i64) -> Result<(), ApiError> {
    demo!(fixtures::delete_profile_file(id, file));

    delete_empty(&format!("/profiles/{}/files/{file}", id.get())).await
}

/// The Mission Package a device would actually receive.
///
/// The point of it is that it is assembled by the same code path as the real
/// delivery, so what an administrator opens is what a client would import —
/// rather than a rendering of the editor's own state.
pub async fn preview(id: ProfileId) -> Result<Download, ApiError> {
    demo!(fixtures::profile_preview(id));

    get_download(&format!("/profiles/{}/preview", id.get()), "profile.zip").await
}
