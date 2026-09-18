//! Enterprise Sync: the stored files and everything said about them.
//!
//! A TAK client calls this surface a *data package*, an *attachment* or a
//! *resource* depending on which screen it is on; they are all one thing here —
//! some bytes in the content-addressed store ([`store`]) and a row saying what
//! they are ([`crate::db::repos::resources`]).
//!
//! # The three views of one row
//!
//! The same resource is rendered three different ways on the wire, and a client
//! that gets the wrong one fails rather than adapts:
//!
//! * the Title-case **`Metadata`** object with string-valued `Size` and
//!   `PrimaryKey` that the legacy `/Marti/sync/*` servlets emit ([`legacy`]),
//! * the lowerCamelCase **`Resource`** object with a numeric `size` that
//!   `/Marti/api/sync/search` emits ([`metadata`]), and
//! * the flat map of display strings, with a humanised `Size` and a Java
//!   `Date.toString()` `Time`, that `/Marti/api/files/metadata` emits
//!   ([`legacy::files_entry`]).
//!
//! They are three functions over one row rather than three types, so a field
//! added to the row is one edit and a missing field is a compile error.
//!
//! # Nothing here loads a file into memory
//!
//! A data package is routinely tens of megabytes and may be four hundred.
//! Uploads stream through [`store::ingest`] into a temporary file while being
//! hashed, and downloads stream back out of one — see that module for why the
//! size limit is enforced inside the reader rather than around it.

pub mod legacy;
/// The upload ceiling and where it comes from (M3-03).
pub mod limits;
pub mod metadata;
/// The Mission Package manifest reader and writer (M3-02).
pub mod package;
/// Changing a stored file's metadata from the admin API (M3-03).
pub mod patch;
pub mod search;
pub mod store;
pub mod upload;

pub use metadata::{ResourceJson, Viewer, resource_json, viewer_for};
pub use store::{IngestError, Ingested, ingest};
pub use upload::Upload;
