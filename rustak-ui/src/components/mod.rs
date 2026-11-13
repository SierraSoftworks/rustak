//! The shared, presentational components.
//!
//! The look comes from yewi. What is here is either a thin adapter that takes
//! what a page has to hand and renders the yewi control for it, or a widget
//! that is this console's own (a channel picker, a preference editor).
//!
//! All text is rendered through Yew's `{value}` interpolation, which
//! HTML-escapes its content, and JSON is rendered as a plain text node inside
//! `<pre><code>` — so a value that came from a client, a certificate or an audit
//! record cannot inject markup into the page an administrator is reading.

mod buttons;
mod chrome;
mod confirm;
mod controls;
mod create_drawer;
mod detail_drawer;
mod feedback;
mod file_drop;
mod groups_picker;
mod layout;
mod prefs_editor;
mod role_badge;
mod schema_form;
mod secret_reveal;
mod shell;
mod split_button;
mod tabs;
mod tree_select;
mod xml_view;

#[allow(unused_imports)]
pub use buttons::{Button, ButtonGroup, ButtonKind};
pub use chrome::{PageActions, PageHeading, use_page_controls, use_page_search};
pub use confirm::ConfirmButton;
pub use controls::{
    Field, NumberInput, SecretInput, Select, SelectOption, Switch, TextArea, TextInput,
};
pub use create_drawer::CreateDrawer;
pub use detail_drawer::DetailDrawer;
#[allow(unused_imports)]
pub use feedback::{
    Alert, AlertKind, Callout, Center, EmptyState, LoadingNote, RefreshButton, Section, Stat,
    StatusPill, StatusTone, Tag,
};
pub use file_drop::FileDrop;
pub use groups_picker::GroupsPicker;
pub use layout::Layout;
#[allow(unused_imports)]
pub use prefs_editor::{PrefsEditor, problem_with};
#[allow(unused_imports)]
pub use role_badge::{RoleBadge, role_description, role_label, role_options};
pub use schema_form::SchemaNode;
#[allow(unused_imports)]
pub use secret_reveal::{Copyable, SecretReveal};
pub use shell::AdminShell;
pub use split_button::{MenuAction, MenuItem, SplitButton};
pub use tabs::TabBar;
// Its only caller today is the control gallery, which a release build does not
// contain; the CoT browser that needs it lands with the next brief.
#[allow(unused_imports)]
pub use tree_select::{Entry as TreeEntry, Tree, TreeSelect};
pub use xml_view::XmlView;
