//! A QR code, drawn as SVG.
//!
//! ATAK's "quick connect" reads
//! `tak://com.atakmap.app/enroll?host=&username=&token=` out of a camera frame,
//! so the enrolment flow is only finished when there is something on screen to
//! point a phone at. The encoding is done here, in the browser, by a pure-Rust
//! encoder compiled to wasm: the URL carries a one-time secret, and handing it
//! to an image service — or to any code we did not compile ourselves — would be
//! handing away the credential.
//!
//! # Why the modules are drawn rather than rendered
//!
//! `qrcode` can emit an SVG document as a string, which would then have to be
//! injected with `dangerously_set_inner_html`. Walking the module matrix and
//! emitting one `<path>` instead keeps the whole component inside Yew's own
//! escaping, and the path data is an attribute value rather than markup — so
//! there is no route from the encoded string to the DOM as HTML.

use qrcode::{Color, QrCode};
use yew::prelude::*;

/// The blank border a scanner needs to find the code at all. Four modules is
/// the quiet zone the specification asks for.
const QUIET_ZONE: u32 = 4;

#[derive(Properties, PartialEq)]
pub struct QrCodeProps {
    /// What the code encodes.
    pub data: AttrValue,

    /// The rendered edge, in CSS pixels.
    #[prop_or(208)]
    pub size: u32,

    /// What the code is, for anybody who cannot see it. A QR code with no text
    /// alternative is an unlabelled image of a secret.
    #[prop_or(AttrValue::from("QR code"))]
    pub alt: AttrValue,
}

/// Renders `data` as a scannable QR code, or says why it could not.
#[function_component(QrCodeView)]
pub fn qr_code_view(props: &QrCodeProps) -> Html {
    let code = use_memo(props.data.clone(), |data| QrCode::new(data.as_bytes()));

    let Ok(code) = code.as_ref() else {
        // The only way this fails is data too long for the largest version,
        // which an enrolment URL cannot be — but saying so beats an empty box.
        return html! {
            <p class="qr-code__error" role="alert">
                { "This value is too long to put in a QR code. Use the link instead." }
            </p>
        };
    };

    let width = code.width() as u32;
    let span = width + QUIET_ZONE * 2;
    let path = modules_path(&code.to_colors(), width);
    let view_box = format!("0 0 {span} {span}");
    let size = props.size.to_string();

    html! {
        <svg
            class="qr-code"
            viewBox={view_box}
            width={size.clone()}
            height={size}
            role="img"
            aria-label={props.alt.clone()}
            shape-rendering="crispEdges"
        >
            <rect x="0" y="0" width="100%" height="100%" fill="#ffffff" />
            <path d={path} fill="#000000" />
        </svg>
    }
}

/// One `<path>` covering every dark module, as a run of unit squares.
///
/// A rectangle per module would be several hundred DOM nodes for a URL of this
/// length; a single path is one, and a scanner cannot tell the difference.
/// Horizontal runs are merged so the data stays short.
fn modules_path(colors: &[Color], width: u32) -> String {
    let mut path = String::new();

    for y in 0..width {
        let mut x = 0;
        while x < width {
            if colors[(y * width + x) as usize] == Color::Light {
                x += 1;
                continue;
            }

            let start = x;
            while x < width && colors[(y * width + x) as usize] == Color::Dark {
                x += 1;
            }

            let run = x - start;
            path.push_str(&format!(
                "M{} {}h{run}v1h-{run}z",
                start + QUIET_ZONE,
                y + QUIET_ZONE
            ));
        }
    }

    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjacent_modules_become_one_run() {
        // A two-by-two grid: a dark pair on the top row and one dark module on
        // the bottom right. The pair is one run rather than two squares, and
        // every coordinate is offset by the quiet zone.
        let colors = [Color::Dark, Color::Dark, Color::Light, Color::Dark];

        assert_eq!(modules_path(&colors, 2), "M4 4h2v1h-2zM5 5h1v1h-1z");
    }

    #[test]
    fn an_enrolment_url_encodes() {
        let url = "tak://com.atakmap.app/enroll?host=tak.example.com&username=avery&token=abc";
        let code = QrCode::new(url.as_bytes()).expect("an enrolment URL fits in a QR code");
        assert!(code.width() >= 21);
        assert!(!modules_path(&code.to_colors(), code.width() as u32).is_empty());
    }
}
