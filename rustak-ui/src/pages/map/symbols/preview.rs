//! The picture beside a row: the symbol a code is drawn as.
//!
//! Drawn by the same milsymbol the map draws with, so what somebody picks is
//! what they get. It is an `<img>` with the symbol's SVG as its address rather
//! than the SVG itself in the page, which keeps a drawing library's markup out
//! of the document and lets the browser cache a picture it has drawn once.

use yew::prelude::*;

use super::super::glue;

/// How tall a preview is drawn, in the units milsymbol sizes a symbol in.
const SIZE: u32 = 18;

#[derive(Properties, PartialEq)]
pub struct SymbolPreviewProps {
    /// A symbol identification code, in any edition milsymbol reads.
    pub code: AttrValue,
}

#[function_component(SymbolPreview)]
pub fn symbol_preview(props: &SymbolPreviewProps) -> Html {
    let address = glue::symbol_url(&props.code, SIZE);

    // The box is there either way, so a list does not shuffle sideways between
    // the rows that have a picture and the ones that do not.
    html! {
        <span class="tree-select__icon">
            if !address.is_empty() {
                <img src={address} alt="" loading="lazy" />
            }
        </span>
    }
}
