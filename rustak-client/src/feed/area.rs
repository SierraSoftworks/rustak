//! The area of interest a feed subscribes with and the publisher filters on.
//!
//! Every open feed is bigger than any operator wants: AIS carries the world's
//! shipping and a public ADS-B aggregator carries every aircraft in the air. An
//! area is therefore two things at once — what the *source* asks upstream for
//! (a bounding box on a WebSocket subscription, a centre and a radius on an
//! HTTP endpoint), and what the *publisher* re-checks before it puts anything
//! on the map, because an upstream that widens its box is not a reason for a
//! channel to fill up with the Atlantic.

use serde::{Deserialize, Serialize};

/// Metres per degree of latitude, which is constant enough at this scale: the
/// WGS-84 meridian varies by about half a percent pole to equator, and an area
/// of interest is a filter rather than a survey.
const METRES_PER_DEGREE: f64 = 111_320.0;

/// Metres in a nautical mile, exactly.
const METRES_PER_NAUTICAL_MILE: f64 = 1_852.0;

/// The mean Earth radius the haversine formula uses, in metres.
const EARTH_RADIUS_M: f64 = 6_371_008.8;

/// Where a feed is looking.
///
/// Written in a plugin's `[settings]` table as one of:
///
/// ```toml
/// [settings.area]
/// kind = "bbox"
/// south = 50.0
/// west = -1.5
/// north = 51.8
/// east = 1.5
/// ```
///
/// ```toml
/// [settings.area]
/// kind = "circle"
/// lat = 51.4775
/// lon = -0.4614
/// radius_km = 120.0
/// ```
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Area {
    /// A latitude/longitude box, which may cross the anti-meridian (`west`
    /// greater than `east`, e.g. Fiji: west 175, east -178).
    Bbox {
        /// Southern edge, decimal degrees.
        south: f64,
        /// Western edge, decimal degrees.
        west: f64,
        /// Northern edge, decimal degrees.
        north: f64,
        /// Eastern edge, decimal degrees.
        east: f64,
    },
    /// A circle around a point, which is what most HTTP feeds take.
    Circle {
        /// Centre latitude, decimal degrees.
        lat: f64,
        /// Centre longitude, decimal degrees.
        lon: f64,
        /// Radius in kilometres.
        radius_km: f64,
    },
}

impl Default for Area {
    /// The whole world, so that a plugin whose operator has not said where it
    /// is looking publishes everything it is given rather than nothing at all.
    fn default() -> Self {
        Self::Bbox {
            south: -90.0,
            west: -180.0,
            north: 90.0,
            east: 180.0,
        }
    }
}

impl Area {
    /// Whether a position is inside this area.
    ///
    /// ```
    /// use rustak_client::feed::Area;
    ///
    /// // Fiji, across the anti-meridian.
    /// let area = Area::Bbox { south: -19.0, west: 176.0, north: -16.0, east: -178.0 };
    ///
    /// assert!(area.contains(-18.1, 178.4));
    /// assert!(area.contains(-17.5, -179.0));
    /// assert!(!area.contains(-17.5, 100.0));
    /// ```
    #[must_use]
    pub fn contains(&self, lat: f64, lon: f64) -> bool {
        match *self {
            Self::Bbox {
                south,
                west,
                north,
                east,
            } => {
                let within_latitude = lat >= south && lat <= north;
                let within_longitude = match west <= east {
                    true => lon >= west && lon <= east,
                    // The box wraps through 180°, so "inside" is the union of
                    // the two halves rather than the intersection.
                    false => lon >= west || lon <= east,
                };

                within_latitude && within_longitude
            }
            Self::Circle {
                lat: centre_lat,
                lon: centre_lon,
                radius_km,
            } => distance_m((centre_lat, centre_lon), (lat, lon)) <= radius_km * 1_000.0,
        }
    }

    /// The smallest box enclosing this area, for a source whose subscription
    /// only takes one.
    ///
    /// A circle's box is the one its radius fits inside, so the source asks for
    /// slightly more than the operator wanted and [`contains`](Self::contains)
    /// trims the corners before anything is published.
    #[must_use]
    pub fn bbox(&self) -> Self {
        match *self {
            Self::Bbox { .. } => *self,
            Self::Circle {
                lat,
                lon,
                radius_km,
            } => {
                let radius_m = radius_km * 1_000.0;
                let latitude_span = radius_m / METRES_PER_DEGREE;
                // A degree of longitude shortens towards the poles. `cos` of a
                // latitude at the pole is zero, so the span is floored at a
                // whole hemisphere rather than dividing by it.
                let shrink = lat.to_radians().cos().abs().max(1e-6);
                let longitude_span = (radius_m / (METRES_PER_DEGREE * shrink)).min(180.0);

                Self::Bbox {
                    south: (lat - latitude_span).max(-90.0),
                    west: wrap_longitude(lon - longitude_span),
                    north: (lat + latitude_span).min(90.0),
                    east: wrap_longitude(lon + longitude_span),
                }
            }
        }
    }

    /// The centre of this area, as a source that takes a point wants it.
    #[must_use]
    pub fn centre(&self) -> (f64, f64) {
        match *self {
            Self::Bbox {
                south,
                west,
                north,
                east,
            } => {
                let span = match west <= east {
                    true => east - west,
                    false => east + 360.0 - west,
                };

                ((south + north) / 2.0, wrap_longitude(west + span / 2.0))
            }
            Self::Circle { lat, lon, .. } => (lat, lon),
        }
    }

    /// The radius in nautical miles that covers this area from its
    /// [`centre`](Self::centre), which is the unit every ADS-B aggregator's
    /// `point` endpoint takes.
    ///
    /// A box's radius reaches its furthest corner, so the request covers the
    /// whole box rather than the circle inscribed in it.
    #[must_use]
    pub fn radius_nm(&self) -> f64 {
        match *self {
            Self::Circle { radius_km, .. } => radius_km * 1_000.0 / METRES_PER_NAUTICAL_MILE,
            Self::Bbox {
                south,
                west,
                north,
                east,
            } => {
                let centre = self.centre();
                // The haversine is periodic in longitude, so a corner on the
                // far side of the anti-meridian measures as the three degrees
                // it is rather than the three hundred and fifty-seven it looks.
                let furthest = [(south, west), (south, east), (north, west), (north, east)]
                    .into_iter()
                    .map(|corner| distance_m(centre, corner))
                    .fold(0.0_f64, f64::max);

                furthest / METRES_PER_NAUTICAL_MILE
            }
        }
    }
}

/// Great-circle distance between two positions, in metres.
///
/// The haversine formula on a sphere: a few lines of our own rather than a
/// geodesy dependency, because what it is used for is "did this ship move more
/// than twenty-five metres" and "is this aircraft inside the circle", where the
/// third of a percent a sphere costs against WGS-84 changes no decision.
#[must_use]
pub fn distance_m(from: (f64, f64), to: (f64, f64)) -> f64 {
    let (from_lat, from_lon) = (from.0.to_radians(), from.1.to_radians());
    let (to_lat, to_lon) = (to.0.to_radians(), to.1.to_radians());

    let delta_lat = to_lat - from_lat;
    let delta_lon = to_lon - from_lon;

    let a = (delta_lat / 2.0).sin().powi(2)
        + from_lat.cos() * to_lat.cos() * (delta_lon / 2.0).sin().powi(2);

    2.0 * EARTH_RADIUS_M * a.sqrt().clamp(0.0, 1.0).asin()
}

/// Brings a longitude back into `[-180, 180]` after arithmetic took it out.
fn wrap_longitude(lon: f64) -> f64 {
    let wrapped = (lon + 180.0).rem_euclid(360.0) - 180.0;

    // `rem_euclid` answers 0 for exactly 360, which would turn the eastern edge
    // of a box at the anti-meridian into its western one.
    match wrapped == -180.0 && lon > 0.0 {
        true => 180.0,
        false => wrapped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_box_contains_what_is_inside_it() {
        let area = Area::Bbox {
            south: 50.0,
            west: -1.5,
            north: 51.8,
            east: 1.5,
        };

        assert!(area.contains(51.5074, -0.1278));
        assert!(area.contains(50.0, -1.5), "the edges are inside");
        assert!(!area.contains(52.0, 0.0));
        assert!(!area.contains(51.0, 2.0));
    }

    #[test]
    fn a_box_across_the_anti_meridian_is_the_union_of_its_halves() {
        // The naive `west <= lon <= east` reading of this box is empty, which
        // is how a feed watching Fiji publishes nothing at all.
        let area = Area::Bbox {
            south: -19.0,
            west: 176.0,
            north: -16.0,
            east: -178.0,
        };

        assert!(area.contains(-18.1, 178.4));
        assert!(area.contains(-17.5, -179.5));
        assert!(!area.contains(-17.5, 170.0));
        assert!(!area.contains(-20.0, 178.0), "latitude still bounds it");
    }

    #[test]
    fn a_circle_contains_what_is_within_its_radius() {
        // Heathrow, 30 km.
        let area = Area::Circle {
            lat: 51.4775,
            lon: -0.4614,
            radius_km: 30.0,
        };

        assert!(area.contains(51.4775, -0.4614));
        assert!(
            area.contains(51.5074, -0.1278),
            "central London is 23 km east",
        );
        assert!(
            !area.contains(51.8860, 0.2389),
            "Stansted, at 60 km, is not"
        );
    }

    #[test]
    fn a_circles_box_encloses_it() {
        let circle = Area::Circle {
            lat: 51.4775,
            lon: -0.4614,
            radius_km: 20.0,
        };

        let Area::Bbox {
            south,
            west,
            north,
            east,
        } = circle.bbox()
        else {
            panic!("a circle's bbox is a box");
        };

        assert!(south < 51.4775 && north > 51.4775);
        assert!(west < -0.4614 && east > -0.4614);
        // Everything the circle holds is in the box, and the box is not much
        // bigger: its corners are the radius times root two away.
        assert!(circle.bbox().contains(51.4775, -0.4614));
        assert!(circle.bbox().contains(51.6, -0.4614));
        assert!(!circle.bbox().contains(52.5, -0.4614));
    }

    #[test]
    fn a_box_is_its_own_box_and_a_circle_keeps_its_radius() {
        let box_area = Area::Bbox {
            south: 50.0,
            west: -1.0,
            north: 52.0,
            east: 1.0,
        };

        assert_eq!(box_area.bbox(), box_area);
        assert_eq!(box_area.centre(), (51.0, 0.0));

        let circle = Area::Circle {
            lat: 0.0,
            lon: 0.0,
            radius_km: 185.2,
        };

        assert!((circle.radius_nm() - 100.0).abs() < 0.001);
        assert_eq!(circle.centre(), (0.0, 0.0));
    }

    #[test]
    fn a_boxs_radius_reaches_its_corner() {
        // A source that only takes a centre and a radius must be asked for at
        // least the whole box, or the corners never arrive.
        let area = Area::Bbox {
            south: 50.0,
            west: -1.0,
            north: 52.0,
            east: 1.0,
        };

        let radius_m = area.radius_nm() * METRES_PER_NAUTICAL_MILE;

        assert!(distance_m(area.centre(), (50.0, -1.0)) <= radius_m);
        assert!(distance_m(area.centre(), (52.0, 1.0)) <= radius_m);
    }

    #[test]
    fn a_box_across_the_anti_meridian_has_its_centre_on_the_line() {
        let area = Area::Bbox {
            south: -19.0,
            west: 176.0,
            north: -16.0,
            east: -178.0,
        };

        let (lat, lon) = area.centre();

        assert!((lat - -17.5).abs() < 1e-9);
        assert!((lon - 179.0).abs() < 1e-9, "the centre is at 179E, not 1W");
        assert!(area.contains(lat, lon));
    }

    #[test]
    fn distances_are_the_ones_a_chart_gives() {
        // London to Paris, about 343 km.
        let london = (51.5074, -0.1278);
        let paris = (48.8566, 2.3522);

        let metres = distance_m(london, paris);

        assert!(
            (metres - 343_500.0).abs() < 2_000.0,
            "{metres} is not the distance to Paris"
        );
        assert_eq!(distance_m(london, london), 0.0);
    }

    #[test]
    fn an_area_is_read_from_a_settings_table_and_refuses_what_it_does_not_know() {
        let area: Area = toml::from_str(
            r#"
            kind = "circle"
            lat = 51.4775
            lon = -0.4614
            radius_km = 120.0
            "#,
        )
        .expect("a circle");

        assert_eq!(
            area,
            Area::Circle {
                lat: 51.4775,
                lon: -0.4614,
                radius_km: 120.0
            }
        );

        let refused = toml::from_str::<Area>(
            r#"
            kind = "circle"
            lat = 51.4775
            lon = -0.4614
            radius_km = 120.0
            radius_nm = 65.0
            "#,
        );

        assert!(refused.is_err(), "a misspelled key is a start-up failure");
    }

    #[test]
    fn the_default_area_is_the_whole_world() {
        assert!(Area::default().contains(0.0, 0.0));
        assert!(Area::default().contains(-89.0, 179.0));
    }
}
