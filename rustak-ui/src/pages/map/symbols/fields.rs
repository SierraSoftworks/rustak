//! The three fields that say what a marker is: whose, what, and drawn as what.
//!
//! They are one component because they are one decision. Whose something is
//! lives inside both of the other two codes — the `f` of `a-f-G-U-C`, the `F`
//! of `SFGPUC---------` — so changing it rewrites both, and every preview in
//! both lists is redrawn in the new frame and colour.
//!
//! The symbol is optional, and most markers leave it alone: a CoT type implies
//! a symbol. It is offered in the edition of MIL-STD-2525 the account chose
//! under Preferences, unless the marker already carries a code in the other
//! one, which is then the edition its list is in.

use std::rc::Rc;

use rustak_api::Symbology;
use yew::prelude::*;

use crate::app::AuthHandle;
use crate::components::{Select, SelectOption, TreeSelect};

use super::super::draft::well_formed_type;
use super::codes::{self, AFFILIATIONS};
use super::load::use_catalogue;
use super::preview::SymbolPreview;
use super::trees;

#[derive(Properties, PartialEq)]
pub struct IdentityProps {
    /// The CoT type, as the draft holds it.
    pub kind: AttrValue,
    /// The symbol code, or empty to leave the symbol to the type.
    pub sidc: AttrValue,
    /// The type and the symbol code, whenever either changes.
    pub onchange: Callback<(String, String)>,
}

#[function_component(Identity)]
pub fn identity(props: &IdentityProps) -> Html {
    let preferred = use_context::<AuthHandle>()
        .and_then(|auth| auth.user)
        .map(|user| user.preferences.symbology)
        .unwrap_or_default();
    let edition = codes::edition_of(&props.sidc).unwrap_or(preferred);
    let whose = codes::affiliation_of(&props.kind);

    // The type tree is always 2525C's, because that is what CoT was laid out on.
    let listed = use_catalogue(Symbology::Milstd2525C);
    let drawn = use_catalogue(edition);

    let types = use_memo((listed, whose.cot), |(rows, _)| {
        trees::types(rows.as_deref().map_or(&[], Vec::as_slice), whose)
    });
    let symbols = use_memo((drawn, edition, whose.cot), |(rows, edition, _)| {
        trees::symbols(rows.as_deref().map_or(&[], Vec::as_slice), *edition, whose)
    });

    let (kind, sidc) = (props.kind.to_string(), props.sidc.to_string());
    let on_whose = {
        let (onchange, kind, sidc) = (props.onchange.clone(), kind.clone(), sidc.clone());
        Callback::from(move |picked: Option<String>| {
            let picked = AFFILIATIONS
                .into_iter()
                .find(|known| Some(known.cot) == picked.as_deref());
            if let Some(whose) = picked {
                onchange.emit((codes::retyped(&kind, whose), codes::resided(&sidc, whose)));
            }
        })
    };
    let on_kind = {
        let (onchange, sidc) = (props.onchange.clone(), sidc.clone());
        Callback::from(move |picked: Option<String>| {
            if let Some(kind) = picked {
                // A symbol chosen for a friend is not the symbol for a hostile.
                let whose = codes::affiliation_of(&kind);
                onchange.emit((kind, codes::resided(&sidc, whose)));
            }
        })
    };
    let on_sidc = {
        let (onchange, kind) = (props.onchange.clone(), kind.clone());
        Callback::from(move |picked: Option<String>| {
            onchange.emit((kind.clone(), picked.unwrap_or_default()));
        })
    };

    let render_icon = Callback::from(|code: String| html! { <SymbolPreview {code} /> });
    let options: Vec<SelectOption> = AFFILIATIONS
        .into_iter()
        .map(|known| SelectOption::new(known.cot.to_string(), known.label.to_string()))
        .collect();

    html! {
        <>
            <div class="map-editor__field">
                <label for="marker-affiliation">{ "Affiliation" }</label>
                <Select
                    id="marker-affiliation"
                    value={codes::is_atom(&kind).then(|| AttrValue::from(whose.cot))}
                    {options}
                    onchange={on_whose}
                    placeholder="Not something that has one"
                    disabled={!codes::is_atom(&kind)}
                />
            </div>
            <div class="map-editor__field">
                <label for="marker-type">{ "Type" }</label>
                <TreeSelect
                    id="marker-type"
                    tree={Rc::clone(&types)}
                    value={Some(props.kind.clone()).filter(|kind| !kind.is_empty())}
                    onchange={on_kind}
                    placeholder="Choose a type"
                    render_icon={render_icon.clone()}
                    typed={Callback::from(|text: String| well_formed_type(&text).then_some(text))}
                />
            </div>
            <div class="map-editor__field">
                <label for="marker-sidc">{ "Symbol" }</label>
                <TreeSelect
                    id="marker-sidc"
                    tree={Rc::clone(&symbols)}
                    value={Some(props.sidc.clone()).filter(|sidc| !sidc.is_empty())}
                    onchange={on_sidc}
                    placeholder="The type's own symbol"
                    clear_label="The type's own symbol"
                    {render_icon}
                    typed={Callback::from(|text: String| rustak_api::map::sidc(&text))}
                />
            </div>
        </>
    }
}
