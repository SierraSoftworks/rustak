//! What this server keeps, and how much it will accept.
//!
//! The upload ceiling is the one setting here somebody may change from the
//! console, and it lives on [`FilesCard`] with the rest of what Enterprise
//! Sync tells clients about itself. Figures for the database, the content
//! store and the disk arrive with the endpoint that can read them.

use yew::prelude::*;

use super::settings_files::FilesCard;

#[function_component(Storage)]
pub fn storage() -> Html {
    html! { <FilesCard /> }
}
