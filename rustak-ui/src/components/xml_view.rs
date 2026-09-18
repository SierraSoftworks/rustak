//! An XML document, shown the way an operator needs to read one.
//!
//! CoT events are the thing an administrator most often has to look at
//! character by character — a stale time, a callsign with a stray space, a
//! `<detail>` a plugin wrote — so this renders the document verbatim and
//! colours it, rather than pretty-printing it into something that is no longer
//! what came off the wire.
//!
//! # Nothing is ever injected as markup
//!
//! The document being shown came from a client, which makes it exactly the
//! kind of value that must not reach the DOM as HTML. [`tokenise`] therefore
//! splits the text into spans and every one of them is rendered through Yew's
//! `{value}` interpolation, which escapes it. There is no
//! `dangerously_set_inner_html` on this path, and the unit tests pin the
//! invariant that matters: the tokens concatenate back to exactly the input,
//! so nothing is dropped, reordered or invented on the way to the screen.

// The CoT browser this was written for is the next brief's; until then the only
// caller is the control gallery, which a release build does not contain. The
// allow is here rather than on the re-export so that a release build stays as
// quiet as a debug one.
#![allow(dead_code)]

use yew::prelude::*;

/// What a run of characters is, which is also the class it is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    /// `<`, `</`, `>`, `/>` and the punctuation inside a tag.
    Punctuation,
    /// An element's name.
    Name,
    /// An attribute's name.
    Attribute,
    /// An attribute's value, quotes included.
    Value,
    /// A comment, a processing instruction or a CDATA section, whole.
    Aside,
    /// Character data between elements.
    Text,
}

impl Token {
    fn class(self) -> &'static str {
        match self {
            Token::Punctuation => "xml__punct",
            Token::Name => "xml__name",
            Token::Attribute => "xml__attr",
            Token::Value => "xml__value",
            Token::Aside => "xml__aside",
            Token::Text => "xml__text",
        }
    }
}

/// Splits a document into coloured runs.
///
/// Deliberately not a parser: it never rejects anything, because a malformed
/// document is precisely the one somebody is trying to read. Anything it
/// cannot make sense of is emitted as text, unchanged.
pub fn tokenise(xml: &str) -> Vec<(Token, String)> {
    let mut out: Vec<(Token, String)> = Vec::new();
    let mut rest = xml;

    while !rest.is_empty() {
        let Some(start) = rest.find('<') else {
            push(&mut out, Token::Text, rest);
            break;
        };

        push(&mut out, Token::Text, &rest[..start]);
        rest = &rest[start..];

        // A comment, declaration or CDATA section runs to its own terminator
        // and has no attributes worth colouring inside it.
        if let Some((prefix, terminator)) = aside_of(rest) {
            let end = rest[prefix.len()..]
                .find(terminator)
                .map(|at| at + prefix.len() + terminator.len())
                .unwrap_or(rest.len());

            push(&mut out, Token::Aside, &rest[..end]);
            rest = &rest[end..];
            continue;
        }

        let Some(end) = rest.find('>') else {
            // An unterminated tag is the rest of the document, and showing it
            // as text is more honest than pretending it closed.
            push(&mut out, Token::Text, rest);
            break;
        };

        tag(&mut out, &rest[..=end]);
        rest = &rest[end + 1..];
    }

    out
}

/// The prefix and terminator of a run that has no internal structure.
fn aside_of(rest: &str) -> Option<(&'static str, &'static str)> {
    for (prefix, terminator) in [
        ("<!--", "-->"),
        ("<![CDATA[", "]]>"),
        ("<?", "?>"),
        ("<!", ">"),
    ] {
        if rest.starts_with(prefix) {
            return Some((prefix, terminator));
        }
    }

    None
}

/// Colours one `<…>`, which is the only place attributes live.
fn tag(out: &mut Vec<(Token, String)>, tag: &str) {
    let opener = if tag.starts_with("</") { "</" } else { "<" };
    push(out, Token::Punctuation, opener);

    let body = &tag[opener.len()..tag.len() - 1];
    let closer = match body.ends_with('/') {
        true => "/>",
        false => ">",
    };
    let body = &body[..body.len() - (closer.len() - 1)];

    let name_end = body.find(|c: char| c.is_whitespace()).unwrap_or(body.len());
    push(out, Token::Name, &body[..name_end]);

    attributes(out, &body[name_end..]);
    push(out, Token::Punctuation, closer);
}

/// Colours the attribute list of a tag, whitespace and all.
fn attributes(out: &mut Vec<(Token, String)>, mut rest: &str) {
    while !rest.is_empty() {
        let Some(equals) = rest.find('=') else {
            push(out, Token::Attribute, rest);
            return;
        };

        // The whitespace before a name belongs to nobody, so it goes out as
        // punctuation rather than being trimmed — the document is shown
        // verbatim, spacing included. Walked by `char_indices` rather than by
        // byte, because an attribute name may be any XML name and slicing one
        // mid-character would panic rather than mis-colour.
        let name_start = rest[..equals]
            .char_indices()
            .rev()
            .find(|(_, c)| c.is_whitespace())
            .map(|(at, c)| at + c.len_utf8())
            .unwrap_or(0);

        push(out, Token::Punctuation, &rest[..name_start]);
        push(out, Token::Attribute, &rest[name_start..equals]);
        push(out, Token::Punctuation, "=");

        rest = &rest[equals + 1..];

        let quote = rest.chars().next();
        match quote {
            Some(quote @ ('"' | '\'')) => {
                let end = rest[1..].find(quote).map(|at| at + 2).unwrap_or(rest.len());
                push(out, Token::Value, &rest[..end]);
                rest = &rest[end..];
            }
            _ => {
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                push(out, Token::Value, &rest[..end]);
                rest = &rest[end..];
            }
        }
    }
}

/// Appends a run, dropping the empty ones so the DOM stays small.
fn push(out: &mut Vec<(Token, String)>, token: Token, text: &str) {
    if !text.is_empty() {
        out.push((token, text.to_string()));
    }
}

#[derive(Properties, PartialEq)]
pub struct XmlViewProps {
    /// The document, exactly as it arrived.
    pub xml: AttrValue,

    /// What this document is, for a screen reader.
    #[prop_or(AttrValue::from("XML document"))]
    pub label: AttrValue,
}

/// A read-only, coloured XML document.
#[function_component(XmlView)]
pub fn xml_view(props: &XmlViewProps) -> Html {
    let tokens = tokenise(&props.xml);

    html! {
        <pre class="xml" tabindex="0" role="group" aria-label={props.label.clone()}>
            <code>
                { for tokens.into_iter().enumerate().map(|(index, (token, text))| html! {
                    <span key={index} class={token.class()}>{ text }</span>
                }) }
            </code>
        </pre>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rejoined(xml: &str) -> String {
        tokenise(xml)
            .into_iter()
            .map(|(_, text)| text)
            .collect::<String>()
    }

    #[test]
    fn the_tokens_concatenate_back_to_exactly_the_document() {
        // The invariant that matters: a viewer that dropped or reordered a
        // character would be showing an operator something the client did not
        // send, which is worse than not showing it at all.
        for document in [
            r#"<event version="2.0" uid="ANDROID-1"><point lat="51.5" lon="-0.1"/></event>"#,
            "<?xml version='1.0'?><!-- a note --><a b='c'>text &amp; more</a>",
            "<event  spaced = \"yes\" bare=1 ><detail/></event>",
            "<unterminated attr=\"open",
            "no tags at all",
            "",
        ] {
            assert_eq!(rejoined(document), document, "round trip of {document:?}");
        }
    }

    #[test]
    fn an_element_its_attributes_and_its_text_are_told_apart() {
        let tokens = tokenise(r#"<point lat="51.5"/>"#);

        assert!(tokens.contains(&(Token::Name, "point".to_string())));
        assert!(tokens.contains(&(Token::Attribute, "lat".to_string())));
        assert!(tokens.contains(&(Token::Value, "\"51.5\"".to_string())));
        assert!(tokens.contains(&(Token::Punctuation, "/>".to_string())));
    }

    #[test]
    fn a_comment_is_one_run_even_when_it_contains_angle_brackets() {
        let tokens = tokenise("<!-- <event/> was here --><a/>");

        assert_eq!(
            tokens.first(),
            Some(&(Token::Aside, "<!-- <event/> was here -->".to_string())),
            "{tokens:?}",
        );
    }

    #[test]
    fn a_closing_tag_keeps_its_slash_with_the_punctuation() {
        let tokens = tokenise("</event>");

        assert_eq!(tokens[0], (Token::Punctuation, "</".to_string()));
        assert_eq!(tokens[1], (Token::Name, "event".to_string()));
    }

    #[test]
    fn markup_inside_character_data_stays_character_data() {
        // An `&lt;` in the text is text; if it ever came out as punctuation and
        // a name, the colouring would be telling the reader there is an element
        // where the client sent none.
        let tokens = tokenise("<a>1 &lt; 2</a>");

        assert!(
            tokens.contains(&(Token::Text, "1 &lt; 2".to_string())),
            "{tokens:?}"
        );
    }
}
