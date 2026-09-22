//! What this sidecar is looking at: where, and which kinds of outage.

use rustak_client::feed::Area;

use crate::outage::{Outage, OutageKind};

/// The area of interest and the kinds of outage worth a marker.
///
/// Applied twice: the source checks it before spending a detail request on an
/// outage, and the publisher checks it before anything goes out.
#[derive(Clone, Debug)]
pub struct Scope {
    area: Area,
    include: Vec<OutageKind>,
}

impl Scope {
    /// A scope over an area and the kinds to show.
    #[must_use]
    pub const fn new(area: Area, include: Vec<OutageKind>) -> Self {
        Self { area, include }
    }

    /// Whether an outage is one this sidecar shows.
    #[must_use]
    pub fn admits(&self, outage: &Outage) -> bool {
        self.include.contains(&outage.kind)
            && self.area.contains(outage.position.0, outage.position.1)
    }
}

impl Default for Scope {
    /// Everything, everywhere.
    fn default() -> Self {
        Self::new(Area::default(), OutageKind::ALL.to_vec())
    }
}
