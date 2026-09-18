//! The shared, presentational components.
//!
//! All text is rendered through Yew's `{value}` interpolation, which
//! HTML-escapes its content, and JSON is rendered as a plain text node inside
//! `<pre><code>` — so a value that came from a client, a certificate or an audit
//! record cannot inject markup into the page an administrator is reading.

mod admin_shell;
mod alert;
mod app_bar;
mod form;
mod helpers;
mod layout;
mod page_title;
mod secret_input;
mod status_pill;

pub use admin_shell::{AdminShell, PageActions};
pub use alert::{Alert, AlertKind};
pub use app_bar::AppBar;
#[allow(unused_imports)]
pub use form::{
    Button, ButtonGroup, ButtonKind, Field, NumberInput, Select, SelectOption, Switch, TextArea,
    TextInput,
};
#[allow(unused_imports)]
pub use helpers::{Card, Center, EmptyState, LoadingNote, RefreshButton, Stat};
pub use layout::Layout;
pub use page_title::PageTitle;
#[allow(unused_imports)]
pub use secret_input::SecretInput;
pub use status_pill::{StatusPill, StatusTone};
