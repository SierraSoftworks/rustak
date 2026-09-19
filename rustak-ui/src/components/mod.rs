//! The shared, presentational components.
//!
//! All text is rendered through Yew's `{value}` interpolation, which
//! HTML-escapes its content, and JSON is rendered as a plain text node inside
//! `<pre><code>` — so a value that came from a client, a certificate or an audit
//! record cannot inject markup into the page an administrator is reading.

mod admin_shell;
mod alert;
mod app_bar;
mod confirm;
mod file_drop;
mod form;
mod groups_picker;
mod helpers;
mod layout;
mod page_title;
mod prefs_editor;
mod qr_code;
mod role_badge;
mod secret_input;
mod secret_reveal;
mod split_button;
mod status_pill;
mod xml_view;

pub use admin_shell::{AdminShell, PageActions};
pub use alert::{Alert, AlertKind};
pub use app_bar::AppBar;
pub use confirm::ConfirmButton;
pub use file_drop::FileDrop;
#[allow(unused_imports)]
pub use form::{
    Button, ButtonGroup, ButtonKind, Field, NumberInput, Select, SelectOption, Switch, TextArea,
    TextInput,
};
pub use groups_picker::GroupsPicker;
#[allow(unused_imports)]
pub use helpers::{Card, Center, EmptyState, LoadingNote, RefreshButton, Stat};
pub use layout::Layout;
pub use page_title::PageTitle;
#[allow(unused_imports)]
pub use prefs_editor::{PrefsEditor, problem_with};
pub use qr_code::QrCodeView;
#[allow(unused_imports)]
pub use role_badge::{RoleBadge, role_description, role_label, role_options};
#[allow(unused_imports)]
pub use secret_input::SecretInput;
#[allow(unused_imports)]
pub use secret_reveal::{Copyable, SecretReveal};
pub use split_button::{MenuAction, MenuItem, SplitButton};
pub use status_pill::{StatusPill, StatusTone};
// Its only caller today is the control gallery, which a release build does not
// contain; the CoT browser that needs it lands with the next brief.
#[allow(unused_imports)]
pub use xml_view::XmlView;
