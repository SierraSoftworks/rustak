//! Latitude and longitude to a Military Grid Reference System string.
//!
//! An operator reading a position off this console is reading it to say it
//! to somebody with a map, and the map speaks MGRS. The conversion is the
//! standard one: WGS 84 to Universal Transverse Mercator (Snyder's series),
//! then the grid zone, the 100 km square letters and the metre residuals.
//!
//! Polar positions (beyond 84° north or 80° south) belong to the Universal
//! Polar Stereographic grid, which this does not implement; a caller falls
//! back to degrees there. The Norway and Svalbard zone exceptions are applied.

/// WGS 84.
const A: f64 = 6_378_137.0;
const F: f64 = 1.0 / 298.257_223_563;
/// The scale on the central meridian.
const K0: f64 = 0.9996;

/// The latitude band letters, 8° each from 80° south, with X stretched to 84°.
const BANDS: &[u8] = b"CDEFGHJKLMNPQRSTUVWX";
/// The 100 km column letters, in the three sets zones cycle through.
const COLUMNS: [&[u8]; 3] = [b"ABCDEFGH", b"JKLMNPQR", b"STUVWXYZ"];
/// The 100 km row letters, cycled every 2 000 km.
const ROWS: &[u8] = b"ABCDEFGHJKLMNPQRSTUV";

/// A position in metres on the UTM grid.
struct Utm {
    zone: u8,
    band: u8,
    easting: f64,
    northing: f64,
}

/// The UTM zone for a position, with the exceptions around Norway and
/// Svalbard where the standard grid is cut to keep a country in one zone.
fn zone_for(lat: f64, lon: f64) -> u8 {
    let zone = ((lon + 180.0) / 6.0).floor() as i32 + 1;
    let zone = zone.clamp(1, 60) as u8;

    if (56.0..64.0).contains(&lat) && (3.0..12.0).contains(&lon) {
        return 32;
    }
    if (72.0..84.0).contains(&lat) {
        return match lon {
            lon if (0.0..9.0).contains(&lon) => 31,
            lon if (9.0..21.0).contains(&lon) => 33,
            lon if (21.0..33.0).contains(&lon) => 35,
            lon if (33.0..42.0).contains(&lon) => 37,
            _ => zone,
        };
    }
    zone
}

fn to_utm(lat: f64, lon: f64) -> Option<Utm> {
    if !(-80.0..84.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
        return None;
    }

    let zone = zone_for(lat, lon);
    let band = BANDS[(((lat + 80.0) / 8.0).floor() as usize).min(BANDS.len() - 1)];

    let e2 = F * (2.0 - F);
    let ep2 = e2 / (1.0 - e2);
    let phi = lat.to_radians();
    let lon0 = (f64::from(zone) - 1.0) * 6.0 - 180.0 + 3.0;
    let dlam = (lon - lon0).to_radians();

    let (sin, cos) = phi.sin_cos();
    let n = A / (1.0 - e2 * sin * sin).sqrt();
    let t = (sin / cos).powi(2);
    let c = ep2 * cos * cos;
    let a1 = dlam * cos;

    let (e4, e6) = (e2 * e2, e2 * e2 * e2);
    let m = A
        * ((1.0 - e2 / 4.0 - 3.0 * e4 / 64.0 - 5.0 * e6 / 256.0) * phi
            - (3.0 * e2 / 8.0 + 3.0 * e4 / 32.0 + 45.0 * e6 / 1024.0) * (2.0 * phi).sin()
            + (15.0 * e4 / 256.0 + 45.0 * e6 / 1024.0) * (4.0 * phi).sin()
            - (35.0 * e6 / 3072.0) * (6.0 * phi).sin());

    let easting = K0
        * n
        * (a1
            + (1.0 - t + c) * a1.powi(3) / 6.0
            + (5.0 - 18.0 * t + t * t + 72.0 * c - 58.0 * ep2) * a1.powi(5) / 120.0)
        + 500_000.0;
    let mut northing = K0
        * (m + n
            * (sin / cos)
            * (a1 * a1 / 2.0
                + (5.0 - t + 9.0 * c + 4.0 * c * c) * a1.powi(4) / 24.0
                + (61.0 - 58.0 * t + t * t + 600.0 * c - 330.0 * ep2) * a1.powi(6) / 720.0));
    if lat < 0.0 {
        northing += 10_000_000.0;
    }

    Some(Utm {
        zone,
        band,
        easting,
        northing,
    })
}

/// The position as an MGRS reference to one metre, `31U DQ 48251 11932`, or
/// `None` where the grid does not reach.
pub fn format(lat: f64, lon: f64) -> Option<String> {
    let utm = to_utm(lat, lon)?;

    let set = usize::from((utm.zone - 1) % 3);
    let column = (utm.easting / 100_000.0).floor() as usize;
    let column = COLUMNS[set].get(column.checked_sub(1)?)?;

    // Even zones start their row lettering five letters along, so that
    // neighbouring zones never share a square name at the same latitude.
    let row = ((utm.northing / 100_000.0).floor() as usize + if utm.zone % 2 == 0 { 5 } else { 0 })
        % ROWS.len();
    let row = ROWS[row];

    let easting = (utm.easting % 100_000.0).floor() as u32;
    let northing = (utm.northing % 100_000.0).floor() as u32;

    Some(format!(
        "{}{} {}{} {easting:05} {northing:05}",
        utm.zone,
        char::from(utm.band),
        char::from(*column),
        char::from(row),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Splits a reference into its parts and checks the metres to a tolerance,
    /// because two correct implementations differ in the last metre.
    fn assert_close(actual: &str, expected: &str, metres: i64) {
        let parts = |s: &str| -> (String, String, i64, i64) {
            let mut it = s.split(' ');
            let zone = it.next().unwrap().to_string();
            let square = it.next().unwrap().to_string();
            let e = it.next().unwrap().parse().unwrap();
            let n = it.next().unwrap().parse().unwrap();
            (zone, square, e, n)
        };
        let (zone, square, e, n) = parts(actual);
        let (ezone, esquare, ee, en) = parts(expected);
        assert_eq!((zone, square), (ezone, esquare), "{actual} vs {expected}");
        assert!((e - ee).abs() <= metres, "easting {actual} vs {expected}");
        assert!((n - en).abs() <= metres, "northing {actual} vs {expected}");
    }

    #[test]
    fn known_positions_land_on_their_references() {
        // The equator on the Greenwich meridian: the textbook zone edge.
        assert_close(&format(0.0, 0.0).unwrap(), "31N AA 66021 00000", 2);
        // The Eiffel Tower, the Sydney Opera House (southern hemisphere, so
        // the 10 000 km false northing) and the Washington Monument, each
        // cross-checked against an independent Krüger-series UTM conversion.
        assert_close(&format(48.8584, 2.2945).unwrap(), "31U DQ 48252 11954", 3);
        assert_close(
            &format(-33.8568, 151.2153).unwrap(),
            "56H LH 34900 52288",
            3,
        );
        assert_close(&format(38.8895, -77.0353).unwrap(), "18S UJ 23478 06483", 3);
    }

    #[test]
    fn the_norway_and_svalbard_exceptions_apply() {
        assert_eq!(zone_for(60.0, 5.0), 32, "Bergen sits in the widened 32V");
        assert_eq!(zone_for(60.0, 2.0), 31);
        assert_eq!(zone_for(78.0, 15.0), 33, "Svalbard is in 33X");
        assert_eq!(zone_for(78.0, 25.0), 35);
        assert_eq!(zone_for(78.0, 10.0), 33);
        assert_eq!(
            zone_for(50.0, 15.0),
            33,
            "the plain grid below the exceptions"
        );
    }

    #[test]
    fn the_poles_are_outside_the_grid() {
        assert!(format(85.0, 10.0).is_none());
        assert!(format(-81.0, 10.0).is_none());
        assert!(
            format(84.0, 10.0).is_none(),
            "84° itself is the first UPS latitude"
        );
        assert!(format(83.9, 10.0).is_some());
        assert!(format(-80.0, 10.0).is_some());
    }

    #[test]
    fn every_reference_has_the_same_shape() {
        for (lat, lon) in [(51.5, -0.1), (-45.0, 170.0), (10.0, -100.0), (70.0, 179.99)] {
            let reference = format(lat, lon).unwrap();
            let parts: Vec<&str> = reference.split(' ').collect();
            assert_eq!(parts.len(), 4, "{reference}");
            assert_eq!(parts[1].len(), 2, "{reference}");
            assert_eq!(parts[2].len(), 5, "{reference}");
            assert_eq!(parts[3].len(), 5, "{reference}");
        }
    }
}
