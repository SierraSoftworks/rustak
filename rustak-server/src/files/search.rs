//! Turning a client's query string into a listing it is allowed to see.
//!
//! Both search endpoints — the legacy `/Marti/sync/search` servlet and the
//! modern `/Marti/api/sync/search` — ask the same questions in different
//! spellings, so one parser reads both. The parameter names are matched
//! case-insensitively ([`CiQuery`]) because a real TAK Server does, and because
//! ATAK and the browser upload page do not agree on the capitalisation.
//!
//! # What is refused and what is ignored
//!
//! A parameter we do not serve is **ignored**, not refused. TAK answers `400`
//! for an unrecognised one, which turns a client that learned a newer server's
//! parameter into a client that cannot search at all; logging it and carrying
//! on is the deliberate deviation recorded in the design.
//!
//! `Circle` is the exception: a radius search we silently dropped would quietly
//! return the whole store, so it is a `400` naming the parameter.
//!
//! # Two passes, on purpose
//!
//! The cheap, indexed predicates are SQL ([`crate::db::repos::ResourceFilter`]);
//! the geographic ones and the visibility rule are applied in Rust over what
//! comes back. Group membership is a bit vector on one side and a JSON array on
//! the other, so the join SQLite would need does not exist, and the volumes
//! here are a browse screen rather than a feed.

use crate::db::Database;
use crate::db::repos::{ResourceFilter, ResourceRow};
use crate::marti::error::MartiError;
use crate::marti::extract::CiQuery;
use crate::marti::time;
use crate::prelude::*;

use super::metadata::Viewer;

/// A latitude/longitude window, normalised so the corners are in order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoundingBox {
    pub min_latitude: f64,
    pub min_longitude: f64,
    pub max_latitude: f64,
    pub max_longitude: f64,
}

impl BoundingBox {
    /// Reads `lat,lon,lat,lon`, in either corner order.
    ///
    /// # Errors
    ///
    /// [`MartiError::InvalidRequest`] naming the value.
    pub fn parse(raw: &str) -> Result<Self, MartiError> {
        let parts: Vec<f64> = raw
            .split(',')
            .map(str::trim)
            .map(str::parse::<f64>)
            .collect::<Result<_, _>>()
            .map_err(|_| MartiError::InvalidRequest(format!("BBox={raw}")))?;

        let [a, b, c, d] = parts[..] else {
            return Err(MartiError::InvalidRequest(format!("BBox={raw}")));
        };

        Ok(Self {
            min_latitude: a.min(c),
            min_longitude: b.min(d),
            max_latitude: a.max(c),
            max_longitude: b.max(d),
        })
    }

    /// Whether a point falls inside, treating a resource with no position as
    /// outside every window.
    pub fn contains(&self, latitude: Option<f64>, longitude: Option<f64>) -> bool {
        let (Some(latitude), Some(longitude)) = (latitude, longitude) else {
            return false;
        };

        latitude >= self.min_latitude
            && latitude <= self.max_latitude
            && longitude >= self.min_longitude
            && longitude <= self.max_longitude
    }
}

/// A parsed search: the part SQL answers and the part Rust does.
#[derive(Debug, Clone, Default)]
pub struct SearchQuery {
    /// The indexed predicates.
    pub filter: ResourceFilter,
    /// `BBox`, applied after the rows come back.
    pub bbox: Option<BoundingBox>,
    pub min_altitude: Option<f64>,
    pub max_altitude: Option<f64>,
    pub remarks: Option<String>,
    pub permissions: Option<String>,
}

impl SearchQuery {
    /// Reads a query string in either endpoint's spelling.
    ///
    /// # Errors
    ///
    /// [`MartiError::InvalidRequest`] for `Circle`, which we do not serve, and
    /// for a timestamp, number or bounding box that does not parse.
    pub fn parse(query: &CiQuery) -> Result<Self, MartiError> {
        if query.has("circle") {
            return Err(MartiError::InvalidRequest(
                "Circle is not supported; use BBox".to_string(),
            ));
        }

        let start = query
            .get("starttime")
            .or_else(|| query.get("submissiondatetime"));
        let end = query.get("stoptime").or_else(|| query.get("endtime"));
        // `keywords` on the legacy servlet, `keyword` on the modern API; both
        // accept the repeated and the comma-joined spellings.
        let mut keywords = query.strings("keywords");
        keywords.extend(query.strings("keyword"));
        keywords.sort();
        keywords.dedup();

        let filter = ResourceFilter {
            id: query.parsed::<i64>("primarykey")?,
            uid: query.get("uid").map(str::to_string),
            hash: query.get("hash").map(str::to_string),
            name: query.get("name").map(str::to_string),
            filename: query.get("filename").map(str::to_string),
            mime_type: query.get("mimetype").map(str::to_string),
            tool: query.get("tool").map(str::to_string),
            mission_name: query.get("mission").map(str::to_string),
            keywords,
            start: start.map(time::parse_date).transpose()?,
            end: end.map(time::parse_date).transpose()?,
            ..ResourceFilter::default()
        };

        let bbox = query
            .get("bbox")
            .or_else(|| query.get("box"))
            .map(BoundingBox::parse)
            .transpose()?;

        Ok(Self {
            filter,
            bbox,
            min_altitude: query.parsed("minaltitude")?,
            max_altitude: query.parsed("maxaltitude")?,
            remarks: query.get("remarks").map(str::to_string),
            permissions: query.get("permissions").map(str::to_string),
        })
    }

    /// Whether a row satisfies the predicates SQL did not answer.
    pub fn matches(&self, resource: &ResourceRow) -> bool {
        if let Some(bbox) = self.bbox
            && !bbox.contains(resource.latitude, resource.longitude)
        {
            return false;
        }

        for (bound, above) in [(self.min_altitude, true), (self.max_altitude, false)] {
            let Some(bound) = bound else { continue };
            let Some(altitude) = resource.altitude else {
                return false;
            };

            if (above && altitude < bound) || (!above && altitude > bound) {
                return false;
            }
        }

        if let Some(remarks) = &self.remarks
            && resource.remarks.as_deref() != Some(remarks.as_str())
        {
            return false;
        }

        if let Some(permissions) = &self.permissions
            && resource.permissions.as_deref() != Some(permissions.as_str())
        {
            return false;
        }

        true
    }
}

/// Runs a search and keeps only what the caller may see.
///
/// # Errors
///
/// A [`human_errors::Kind::System`] error if the listing cannot be read.
#[instrument("files.search", skip_all)]
pub async fn run(
    db: &Database,
    viewer: &Viewer,
    query: SearchQuery,
) -> Result<Vec<ResourceRow>, Error> {
    let rows = db.resources().list(query.filter.clone()).await?;

    Ok(rows
        .into_iter()
        .filter(|row| query.matches(row) && viewer.can_read(row))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(raw: &str) -> CiQuery {
        CiQuery::parse(raw)
    }

    fn row() -> ResourceRow {
        ResourceRow {
            id: 1,
            hash: "aa".to_string(),
            uid: "uid-1".to_string(),
            name: "package.zip".to_string(),
            filename: None,
            mime_type: "application/octet-stream".to_string(),
            size: 1,
            tool: "public".to_string(),
            creator_uid: None,
            submitter_id: None,
            submitter: None,
            submission_time: chrono::Utc::now(),
            expiration: None,
            is_mission_package: false,
            groups: Vec::new(),
            mission_name: None,
            latitude: None,
            longitude: None,
            altitude: None,
            remarks: None,
            permissions: None,
            contacts: None,
            download_path: None,
            plugin_class_name: None,
            install_on_enrollment: false,
            deleted_at: None,
            created_at: chrono::Utc::now(),
            keywords: Vec::new(),
        }
    }

    #[test]
    fn the_parameter_names_are_read_in_whatever_case_they_arrive() {
        let parsed = SearchQuery::parse(&query("KEYWORDS=missionpackage&Tool=public&uid=u1"))
            .expect("a well formed query");

        assert_eq!(parsed.filter.keywords, vec!["missionpackage"]);
        assert_eq!(parsed.filter.tool.as_deref(), Some("public"));
        assert_eq!(parsed.filter.uid.as_deref(), Some("u1"));
    }

    #[test]
    fn both_spellings_of_the_keyword_parameter_are_accepted() {
        let parsed =
            SearchQuery::parse(&query("keywords=a,b&keyword=c&keyword=a")).expect("a query");

        assert_eq!(parsed.filter.keywords, vec!["a", "b", "c"]);
    }

    #[test]
    fn a_parameter_we_do_not_serve_is_ignored_rather_than_refused() {
        // TAK answers 400 here; a client that learned a newer server's
        // parameter would then be unable to search at all.
        let parsed = SearchQuery::parse(&query("Name=x&somethingNew=1")).expect("a query");

        assert_eq!(parsed.filter.name.as_deref(), Some("x"));
    }

    #[test]
    fn a_radius_search_is_refused_rather_than_quietly_dropped() {
        let Err(err) = SearchQuery::parse(&query("Circle=1,2,3")) else {
            panic!("Circle should be refused");
        };

        assert!(err.message().contains("Circle"), "{err}");
    }

    #[test]
    fn a_time_window_is_parsed_in_the_spellings_clients_send() {
        let parsed = SearchQuery::parse(&query(
            "StartTime=2024-05-01T00:00:00.000Z&StopTime=2024-05-02T00:00:00Z",
        ))
        .expect("a query");

        assert!(parsed.filter.start.is_some());
        assert!(parsed.filter.end.is_some());
        assert!(SearchQuery::parse(&query("StartTime=yesterday")).is_err());
    }

    #[test]
    fn a_bounding_box_is_normalised_whichever_corner_came_first() {
        let first = BoundingBox::parse("10,20,-10,-20").unwrap();
        let second = BoundingBox::parse("-10,-20,10,20").unwrap();

        assert_eq!(first, second);
        assert!(first.contains(Some(0.0), Some(0.0)));
        assert!(!first.contains(Some(50.0), Some(0.0)));
        assert!(
            !first.contains(None, None),
            "a resource with no position is outside every window",
        );
        assert!(BoundingBox::parse("10,20,30").is_err());
    }

    #[test]
    fn the_altitude_bounds_exclude_a_resource_that_has_no_altitude() {
        let parsed = SearchQuery::parse(&query("MinAltitude=100")).expect("a query");
        let mut row = row();

        row.altitude = None;
        assert!(!parsed.matches(&row));

        row.altitude = Some(50.0);
        assert!(!parsed.matches(&row));

        row.altitude = Some(150.0);
        assert!(parsed.matches(&row));
    }
}
