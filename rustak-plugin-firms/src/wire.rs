//! The FIRMS area CSV, read into [`Detection`]s.
//!
//! FIRMS answers `/api/area/csv/...` with one row per thermal anomaly. The
//! columns differ by instrument, so rows are read **by header name** rather
//! than by position, and only the four that place a detection in space and time
//! are required:
//!
//! | Column | VIIRS | MODIS | Landsat | Read as |
//! |---|---|---|---|---|
//! | `latitude`, `longitude` | yes | yes | yes | the pixel centre, required |
//! | `acq_date`, `acq_time` | yes | yes | yes | UTC, `acq_time` is `HHMM` with or without its leading zeros, required |
//! | `confidence` | `l`/`n`/`h` | `0`-`100` | `L`/`M`/`H` | [`Confidence`] |
//! | `frp` | yes | yes | no | fire radiative power, MW |
//! | `bright_ti4` / `brightness` | I-4 | band 21/22 | no | brightness temperature, K |
//! | `scan`, `track` | yes | yes | yes | the pixel's size, km |
//!
//! # A refusal is not a CSV
//!
//! FIRMS reports a bad MAP_KEY, a bad area or a spent quota as a line of plain
//! text, not always with a failing status. A body whose first line is not a
//! header is therefore answered as `Err` carrying that line, which is the
//! upstream's own explanation and the most useful thing to show an operator.

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use rustak_core::prelude::*;

/// The longest excerpt of an unreadable reply that is kept.
const EXCERPT: usize = 160;

/// How sure the detection algorithm was that this is a fire.
///
/// MODIS reports a percentage, which is folded onto the three classes FIRMS'
/// own documentation uses for it: under 30 is low, 80 and over is high.
#[derive(
    Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Often sun glint or a warm surface rather than a fire.
    #[default]
    Low,
    /// The ordinary case.
    Nominal,
    /// A saturated or otherwise unambiguous fire pixel.
    High,
}

impl Confidence {
    /// Reads any of the three spellings FIRMS uses.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();

        // `NaN` and the infinities parse as numbers and fail every comparison
        // below, which would land them on `High` and past a confidence filter.
        if let Some(percent) = raw.parse::<f64>().ok().filter(|p| p.is_finite()) {
            return Some(match percent {
                p if p < 30.0 => Self::Low,
                p if p < 80.0 => Self::Nominal,
                _ => Self::High,
            });
        }

        match raw.to_ascii_lowercase().as_str() {
            "l" | "low" => Some(Self::Low),
            "n" | "nominal" | "m" | "medium" => Some(Self::Nominal),
            "h" | "high" => Some(Self::High),
            _ => None,
        }
    }

    /// The word an operator reads.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Nominal => "nominal",
            Self::High => "high",
        }
    }
}

/// One thermal anomaly: a satellite pixel the algorithm flagged as burning.
#[derive(Clone, Debug, PartialEq)]
pub struct Detection {
    /// Pixel centre latitude, decimal degrees.
    pub lat: f64,
    /// Pixel centre longitude, decimal degrees.
    pub lon: f64,
    /// When the satellite saw it.
    pub acquired_at: DateTime<Utc>,
    /// `N`, `N20`, `N21`, `Terra`, `Aqua`, `L8`… as FIRMS spells it.
    pub satellite: Option<String>,
    /// `VIIRS`, `MODIS` or `OLI`.
    pub instrument: Option<String>,
    /// How sure the algorithm was.
    pub confidence: Option<Confidence>,
    /// Fire radiative power, megawatts.
    pub frp_mw: Option<f64>,
    /// Brightness temperature of the fire channel, kelvin.
    pub brightness_k: Option<f64>,
    /// The pixel's along-scan size, kilometres.
    pub scan_km: Option<f64>,
    /// The pixel's along-track size, kilometres.
    pub track_km: Option<f64>,
    /// Whether the overpass was in daylight.
    pub daytime: Option<bool>,
    /// The processing version, e.g. `2.0NRT`.
    pub version: Option<String>,
}

impl Detection {
    /// A detection with nothing known but where and when.
    #[must_use]
    pub const fn at(lat: f64, lon: f64, acquired_at: DateTime<Utc>) -> Self {
        Self {
            lat,
            lon,
            acquired_at,
            satellite: None,
            instrument: None,
            confidence: None,
            frp_mw: None,
            brightness_k: None,
            scan_km: None,
            track_km: None,
            daytime: None,
            version: None,
        }
    }
}

/// What one reply held.
#[derive(Debug, Default, PartialEq)]
pub struct Parsed {
    /// The rows that read as detections.
    pub detections: Vec<Detection>,
    /// How many rows did not: a position off the globe, a time that is not one.
    pub skipped: usize,
}

/// Reads a FIRMS CSV document. Blank lines and `#` comments are ignored, so a
/// hand-written replay file may carry both.
///
/// # Errors
///
/// An excerpt of the first line, when that line is not a FIRMS header: see the
/// module documentation for why that is the upstream refusing.
pub fn parse(body: &str) -> Result<Parsed, String> {
    let mut lines = body
        .trim_start_matches('\u{feff}')
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'));

    // No header at all is an empty answer rather than a refusal.
    let Some(header) = lines.next() else {
        return Ok(Parsed::default());
    };

    let columns =
        Columns::read(header).ok_or_else(|| header.chars().take(EXCERPT).collect::<String>())?;
    let mut parsed = Parsed::default();

    for line in lines {
        match columns.detection(line) {
            Some(detection) => parsed.detections.push(detection),
            None => parsed.skipped += 1,
        }
    }

    Ok(parsed)
}

/// Where each column is in this document.
struct Columns {
    lat: usize,
    lon: usize,
    date: usize,
    time: usize,
    satellite: Option<usize>,
    instrument: Option<usize>,
    confidence: Option<usize>,
    frp: Option<usize>,
    brightness: Option<usize>,
    scan: Option<usize>,
    track: Option<usize>,
    daynight: Option<usize>,
    version: Option<usize>,
}

impl Columns {
    /// Reads a header, or [`None`] when this line is not one.
    fn read(header: &str) -> Option<Self> {
        let names: Vec<String> = fields(header)
            .into_iter()
            .map(str::to_ascii_lowercase)
            .collect();
        let find = |name: &str| names.iter().position(|column| column == name);

        Some(Self {
            lat: find("latitude")?,
            lon: find("longitude")?,
            date: find("acq_date")?,
            time: find("acq_time")?,
            satellite: find("satellite"),
            instrument: find("instrument"),
            confidence: find("confidence"),
            frp: find("frp"),
            // VIIRS names its fire channel; MODIS calls it `brightness`.
            brightness: find("bright_ti4").or_else(|| find("brightness")),
            scan: find("scan"),
            track: find("track"),
            daynight: find("daynight"),
            version: find("version"),
        })
    }

    /// One row, or [`None`] for one that cannot be placed in space and time.
    fn detection(&self, line: &str) -> Option<Detection> {
        let fields = fields(line);
        let text = |index: Option<usize>| {
            index
                .and_then(|index| fields.get(index).copied())
                .filter(|value| !value.is_empty())
        };
        let number = |index: Option<usize>| {
            text(index)
                .and_then(|value| value.parse::<f64>().ok())
                .filter(|value| value.is_finite())
        };

        let (lat, lon) = (number(Some(self.lat))?, number(Some(self.lon))?);

        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            return None;
        }

        Some(Detection {
            satellite: text(self.satellite).map(str::to_string),
            instrument: text(self.instrument).map(str::to_string),
            confidence: text(self.confidence).and_then(Confidence::parse),
            frp_mw: number(self.frp).filter(|frp| *frp >= 0.0),
            brightness_k: number(self.brightness),
            scan_km: number(self.scan).filter(|km| *km > 0.0),
            track_km: number(self.track).filter(|km| *km > 0.0),
            daytime: text(self.daynight).and_then(daytime),
            version: text(self.version).map(str::to_string),
            ..Detection::at(
                lat,
                lon,
                acquired(text(Some(self.date))?, text(Some(self.time))?)?,
            )
        })
    }
}

/// A line's fields. FIRMS quotes nothing and escapes nothing, so a split is the
/// whole of the grammar; stray quotes from a spreadsheet round trip are shed.
fn fields(line: &str) -> Vec<&str> {
    line.split(',')
        .map(|field| field.trim().trim_matches('"').trim())
        .collect()
}

/// `2026-09-22` and `142`, `0142` or `01:42`, as an instant in UTC.
fn acquired(date: &str, time: &str) -> Option<DateTime<Utc>> {
    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    let hhmm: u32 = time.replace(':', "").parse().ok()?;
    let time = NaiveTime::from_hms_opt(hhmm / 100, hhmm % 100, 0)?;

    Some(date.and_time(time).and_utc())
}

/// `D` or `N`.
fn daytime(flag: &str) -> Option<bool> {
    match flag.to_ascii_uppercase().as_str() {
        "D" => Some(true),
        "N" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VIIRS: &str = "latitude,longitude,bright_ti4,scan,track,acq_date,acq_time,satellite,instrument,confidence,version,bright_ti5,frp,daynight";
    const MODIS: &str = "latitude,longitude,brightness,scan,track,acq_date,acq_time,satellite,instrument,confidence,version,bright_t31,frp,daynight";

    fn only(body: &str) -> Detection {
        let mut parsed = parse(body).expect("a FIRMS document");

        assert_eq!(parsed.detections.len(), 1, "{parsed:?}");

        parsed.detections.remove(0)
    }

    #[test]
    fn a_viirs_row_reads_as_a_detection() {
        let detection = only(&format!(
            "{VIIRS}\n40.10234,-7.91456,345.2,0.39,0.36,2026-09-22,1342,N20,VIIRS,h,2.0NRT,301.4,47.3,D\n",
        ));

        assert!((detection.lat - 40.10234).abs() < 1e-9);
        assert!((detection.lon + 7.91456).abs() < 1e-9);
        assert_eq!(
            detection.acquired_at,
            "2026-09-22T13:42:00Z".parse::<DateTime<Utc>>().unwrap(),
        );
        assert_eq!(detection.satellite.as_deref(), Some("N20"));
        assert_eq!(detection.confidence, Some(Confidence::High));
        assert_eq!(detection.frp_mw, Some(47.3));
        assert_eq!(detection.brightness_k, Some(345.2));
        assert_eq!(detection.scan_km, Some(0.39));
        assert_eq!(detection.daytime, Some(true));
    }

    #[test]
    fn a_modis_row_reads_its_own_column_names_and_a_time_without_its_zeros() {
        let detection = only(&format!(
            "{MODIS}\n-33.5,150.25,322.1,1.1,1.0,2026-09-22,42,Aqua,MODIS,85,6.1NRT,290.0,18.0,N\n",
        ));

        assert_eq!(
            detection.acquired_at,
            "2026-09-22T00:42:00Z".parse::<DateTime<Utc>>().unwrap(),
        );
        assert_eq!(detection.brightness_k, Some(322.1));
        assert_eq!(detection.confidence, Some(Confidence::High));
        assert_eq!(detection.daytime, Some(false));
    }

    #[test]
    fn every_spelling_of_confidence_lands_on_one_of_three_classes() {
        for (written, expected) in [
            ("l", Some(Confidence::Low)),
            ("n", Some(Confidence::Nominal)),
            ("H", Some(Confidence::High)),
            ("M", Some(Confidence::Nominal)),
            ("nominal", Some(Confidence::Nominal)),
            ("0", Some(Confidence::Low)),
            ("29", Some(Confidence::Low)),
            ("30", Some(Confidence::Nominal)),
            ("80", Some(Confidence::High)),
            ("?", None),
            ("NaN", None),
            ("inf", None),
            ("-inf", None),
        ] {
            assert_eq!(Confidence::parse(written), expected, "{written}");
        }
    }

    #[test]
    fn a_reply_that_is_not_a_csv_is_the_upstream_refusing() {
        let refused = parse("Invalid MAP_KEY.\n").expect_err("not a header");

        assert_eq!(refused, "Invalid MAP_KEY.");
    }

    #[test]
    fn rows_that_cannot_be_placed_are_counted_rather_than_fatal() {
        let parsed = parse(&format!(
            "# a comment\n{VIIRS}\n\
             95.0,10.0,330.0,0.4,0.4,2026-09-22,1200,N,VIIRS,n,2.0NRT,290.0,5.0,D\n\
             40.0,-8.0,330.0,0.4,0.4,2026-09-22,2561,N,VIIRS,n,2.0NRT,290.0,5.0,D\n\
             40.0,-8.0,330.0,0.4,0.4,2026-09-22,1200,N,VIIRS,n,2.0NRT,290.0,5.0,D\n",
        ))
        .expect("a document");

        assert_eq!(parsed.detections.len(), 1);
        assert_eq!(parsed.skipped, 2);
    }

    #[test]
    fn a_quiet_area_is_a_header_or_nothing_and_neither_is_an_error() {
        assert_eq!(parse(VIIRS), Ok(Parsed::default()));
        assert_eq!(parse(""), Ok(Parsed::default()));
    }
}
