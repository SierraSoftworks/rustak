//! Choosing a file, by dropping it or by saying so.
//!
//! Both, always: a drop zone on its own is unreachable from a keyboard and
//! invisible to a screen reader, and a file input on its own ignores the
//! gesture most people reach for first. So the zone *is* a `<label>` wrapping
//! a real `<input type="file">` — the browser then gives the keyboard, the
//! focus ring and the accessible name for nothing, and the drop handler is an
//! addition rather than a replacement.
//!
//! # The bytes are never read here
//!
//! What comes out is the browser's own `File`, which
//! [`crate::api::download::upload`] hands straight to `FormData`. Nothing in
//! this component copies a file into the wasm heap, so dropping a large one
//! costs nothing until it is sent.

use wasm_bindgen::JsCast;
use web_sys::{DragEvent, HtmlInputElement};
use yew::prelude::*;

#[derive(Properties, PartialEq)]
pub struct FileDropProps {
    /// Called with each chosen file, in the order the browser listed them.
    pub onfiles: Callback<Vec<web_sys::File>>,

    /// Distinguishes this input from any other on the page.
    pub id: AttrValue,

    /// What to drop, in the imperative.
    #[prop_or(AttrValue::from("Drop a file here, or choose one"))]
    pub label: AttrValue,

    /// What this zone accepts and what happens to it afterwards.
    #[prop_or_default]
    pub help: Option<AttrValue>,

    /// The `accept` attribute, when there is something worth narrowing to.
    #[prop_or_default]
    pub accept: Option<AttrValue>,

    /// Whether more than one file may be chosen at a time.
    #[prop_or_default]
    pub multiple: bool,

    #[prop_or_default]
    pub disabled: bool,

    /// Dims the zone and refuses new files while an upload is in flight.
    #[prop_or_default]
    pub busy: bool,
}

/// Reads a `FileList` into a vector, because a `FileList` is not iterable from
/// Rust and every caller wants the same thing from it.
fn files_of(list: Option<web_sys::FileList>) -> Vec<web_sys::File> {
    let Some(list) = list else {
        return Vec::new();
    };

    (0..list.length())
        .filter_map(|index| list.get(index))
        .collect()
}

#[function_component(FileDrop)]
pub fn file_drop(props: &FileDropProps) -> Html {
    let over = use_state(|| false);
    let blocked = props.disabled || props.busy;

    let onchange = {
        let onfiles = props.onfiles.clone();
        Callback::from(move |event: Event| {
            let Some(input) = event
                .target()
                .and_then(|target| target.dyn_into::<HtmlInputElement>().ok())
            else {
                return;
            };

            let files = files_of(input.files());
            // The same file chosen twice in a row is the same value, and a
            // value that has not changed raises no event — so the input is
            // emptied after every choice and a repeat re-upload works.
            input.set_value("");

            if !files.is_empty() {
                onfiles.emit(files);
            }
        })
    };

    let ondrop = {
        let (onfiles, over) = (props.onfiles.clone(), over.clone());
        Callback::from(move |event: DragEvent| {
            // Without this the browser navigates away from the console to
            // display the file, which loses whatever was being edited.
            event.prevent_default();
            over.set(false);

            if blocked {
                return;
            }

            let files = files_of(event.data_transfer().and_then(|data| data.files()));
            if !files.is_empty() {
                onfiles.emit(files);
            }
        })
    };

    let ondragover = {
        let over = over.clone();
        Callback::from(move |event: DragEvent| {
            event.prevent_default();
            if !blocked {
                over.set(true);
            }
        })
    };

    let ondragleave = {
        let over = over.clone();
        Callback::from(move |_: DragEvent| over.set(false))
    };

    html! {
        <label
            class={classes!(
                "file-drop",
                (*over).then_some("file-drop--over"),
                blocked.then_some("file-drop--disabled"),
            )}
            for={props.id.clone()}
            {ondrop}
            {ondragover}
            {ondragleave}
        >
            <input
                class="file-drop__input"
                type="file"
                id={props.id.clone()}
                accept={props.accept.clone()}
                multiple={props.multiple}
                disabled={blocked}
                {onchange}
            />

            <span class="file-drop__label">
                { if props.busy { "Uploading…" } else { props.label.as_str() } }
            </span>

            if let Some(help) = &props.help {
                <span class="file-drop__help">{ help.clone() }</span>
            }
        </label>
    }
}
