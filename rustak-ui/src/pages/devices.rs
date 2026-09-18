//! Every client that has enrolled with this installation.
//!
//! The whole-installation form of [`super::panels::DevicesPanel`]: no username,
//! so the server answers an administrator with everything, and the owner is
//! named on each row because that is the column this page exists for.

use yew::prelude::*;

use super::panels::DevicesPanel;

#[function_component(Devices)]
pub fn devices() -> Html {
    html! { <DevicesPanel title="Enrolled devices" filterable=true show_owner=true /> }
}
