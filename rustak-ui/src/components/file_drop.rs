//! Choosing a file, by dropping it or by picking it.

use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct FileDropProps {
    /// Fired with the browser's own files, which are handed to `FormData` as
    /// they are: their bytes are never read into wasm.
    pub onfiles: Callback<Vec<web_sys::File>>,

    /// The id of the file input, for a label or a test to point at.
    pub id: AttrValue,

    #[prop_or(AttrValue::from("Drop a file here, or choose one"))]
    pub label: AttrValue,

    #[prop_or_default]
    pub help: Option<AttrValue>,

    #[prop_or_default]
    pub accept: Option<AttrValue>,

    #[prop_or_default]
    pub multiple: bool,

    #[prop_or_default]
    pub disabled: bool,

    /// An upload is in flight: nothing more can be dropped until it lands.
    #[prop_or_default]
    pub busy: bool,
}

#[function_component(FileDrop)]
pub fn file_drop(props: &FileDropProps) -> Html {
    let on_files = props
        .onfiles
        .reform(|files: Vec<yewi::SelectedFile>| files.iter().filter_map(browser_file).collect());

    html! {
        <yewi::FileDrop
            input_id={props.id.clone()}
            label={html! { props.label.clone() }}
            hint={props.help.clone().map(Html::from).unwrap_or_default()}
            accept={props.accept.clone()}
            multiple={props.multiple}
            disabled={props.disabled}
            busy={props.busy}
            busy_label="Uploading…"
            {on_files}
        />
    }
}

/// The browser's file behind a selection. Unit tests run off the browser,
/// where a selection has none.
#[cfg(target_arch = "wasm32")]
fn browser_file(file: &yewi::SelectedFile) -> Option<web_sys::File> {
    use wasm_bindgen::JsCast;

    file.js().clone().dyn_into().ok()
}

#[cfg(not(target_arch = "wasm32"))]
fn browser_file(_: &yewi::SelectedFile) -> Option<web_sys::File> {
    None
}
