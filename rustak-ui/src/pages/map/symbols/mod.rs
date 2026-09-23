//! Choosing what a marker is, and what it is drawn as, without knowing either
//! by heart.
//!
//! A CoT type and a MIL-STD-2525 symbol code are both positions in a published
//! hierarchy, written as a code. The form used to ask for the code. This asks
//! three questions somebody can answer by looking: whose it is, what it is,
//! and — only when the type's own symbol is not the one wanted — which symbol.
//!
//! * [`codes`] is the arithmetic between the codes.
//! * [`load`] fetches a catalogue the first time it is wanted.
//! * [`trees`] turns a catalogue into what a picker shows, for one affiliation.
//! * [`preview`] is the picture beside a row.
//! * [`fields`] is the three form fields, and what each does to the others.

mod codes;
mod fields;
mod load;
mod preview;
mod trees;

pub use fields::Identity;
