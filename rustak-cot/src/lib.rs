//! CoT XML model and clean-room TAK Protocol v1 codecs.
//!
//! This crate has no I/O and no server dependencies: it is the shared,
//! wasm-safe vocabulary for talking about Cursor-on-Target events, used by
//! both `rustak-server` and `rustak-client`.
//!
//! M0 ships only the generated-protobuf pipeline (see [`proto`]) so later
//! milestones have a working `protox`/`prost-build` build script to extend.
//! The CoT XML event model and the codecs that convert between XML and the
//! wire protocol are added in M1.

pub mod proto;
