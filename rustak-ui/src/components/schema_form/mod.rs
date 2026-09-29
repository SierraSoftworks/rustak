//! A form drawn from a JSON Schema.
//!
//! One set of controls for every document the console edits from a schema: a
//! service's configuration (the schema its plugin registered), an account's
//! own preferences ([`rustak_api::preferences::schema`]) and a device
//! profile's ATAK preferences ([`rustak_api::pref_catalog::schema`]). Each
//! caller hands [`SchemaNode`] a schema and a value and gets the inputs that
//! schema calls for — a picker for a `oneOf` of constants, a switch for a
//! boolean, a number input with its bounds, text otherwise — each laid out as
//! a labelled [`Field`](super::Field) with the schema's description as help.
//!
//! [`schema`] is the reading of the schema, with no browser in it, so it is
//! tested natively; [`SchemaNode`] and its groups are the drawing.

mod form;
mod groups;
mod inputs;
pub mod schema;

pub use form::SchemaNode;
