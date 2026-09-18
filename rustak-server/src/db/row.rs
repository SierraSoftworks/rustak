//! Column helpers shared by every repository.
//!
//! There is no ORM here: each repository writes its own SQL and maps its own
//! rows. What lives here is the handful of conversions that would otherwise be
//! rewritten — and eventually written differently — in every one of them.
//!
//! # Timestamps
//!
//! [`Timestamp`] is the only way a time reaches or leaves the database. It
//! writes RFC 3339 with exactly three fractional digits and a `Z` suffix
//! (`2026-09-18T10:00:00.123Z`), which sorts lexicographically in the same
//! order it sorts chronologically — the property every `ORDER BY`, `BETWEEN`
//! and retention sweep in the schema depends on.
//!
//! `rusqlite`'s `chrono` feature would happily bind a [`DateTime<Utc>`]
//! directly, and must not be used to: it writes `%F %T%.f%:z`, which is a space
//! instead of the `T`, a `+00:00` instead of the `Z`, and nanoseconds instead of
//! milliseconds. Two rows written through the two paths would not compare.

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};

/// A UTC instant in the one representation the schema stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Timestamp(DateTime<Utc>);

impl Timestamp {
    /// The current instant, truncated to the millisecond the column holds.
    ///
    /// Truncating here rather than on the way out means a value read back
    /// equals the value written, which is what a test comparing the two
    /// expects.
    pub fn now() -> Self {
        Self::from(Utc::now())
    }

    /// The instant this timestamp holds.
    pub fn get(self) -> DateTime<Utc> {
        self.0
    }

    /// The text this timestamp is stored as.
    pub fn to_text(self) -> String {
        self.0.to_rfc3339_opts(SecondsFormat::Millis, true)
    }

    /// Parses a stored timestamp.
    ///
    /// Accepts any RFC 3339 spelling, not only the one we write: a value that
    /// reached the file from a `sqlite3` prompt or an older release is better
    /// read than refused.
    pub fn parse(text: &str) -> Result<Self, chrono::ParseError> {
        Ok(Self::from(
            DateTime::parse_from_rfc3339(text)?.with_timezone(&Utc),
        ))
    }
}

impl From<DateTime<Utc>> for Timestamp {
    fn from(value: DateTime<Utc>) -> Self {
        // Round-trip through the stored text so that the in-memory value and
        // the stored one cannot disagree about the sub-second part.
        let text = value.to_rfc3339_opts(SecondsFormat::Millis, true);

        Self(
            DateTime::parse_from_rfc3339(&text)
                .map(|parsed| parsed.with_timezone(&Utc))
                // Infallible: the string was produced by the formatter three
                // lines above, which only emits values this parser accepts.
                .unwrap_or(value),
        )
    }
}

impl From<Timestamp> for DateTime<Utc> {
    fn from(value: Timestamp) -> Self {
        value.0
    }
}

impl ToSql for Timestamp {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.to_text()))
    }
}

impl FromSql for Timestamp {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        let text = value.as_str()?;

        Timestamp::parse(text).map_err(|err| FromSqlError::Other(Box::new(err)))
    }
}

/// Reads a required timestamp column.
pub fn ts(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<DateTime<Utc>> {
    Ok(row.get::<_, Timestamp>(index)?.get())
}

/// Reads a nullable timestamp column.
pub fn opt_ts(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<DateTime<Utc>>> {
    Ok(row.get::<_, Option<Timestamp>>(index)?.map(Timestamp::get))
}

/// Reads an `INTEGER` column holding a boolean.
///
/// Every such column carries a `CHECK (… IN (0,1))`, so anything else is a row
/// written around the schema rather than through it.
pub fn bool_col(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<bool> {
    Ok(row.get::<_, i64>(index)? != 0)
}

/// Reads an `INTEGER PRIMARY KEY` column into its typed identifier.
pub fn id_col<T: From<i64>>(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<T> {
    Ok(T::from(row.get::<_, i64>(index)?))
}

/// Reads a nullable foreign-key column into its typed identifier.
pub fn opt_id_col<T: From<i64>>(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<Option<T>> {
    Ok(row.get::<_, Option<i64>>(index)?.map(T::from))
}

/// Reads a `TEXT` column holding JSON.
pub fn json_col<T: serde::de::DeserializeOwned>(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<T> {
    let text: String = row.get(index)?;

    serde_json::from_str(&text).map_err(|err| conversion_failed(index, err))
}

/// Reads a nullable `TEXT` column holding JSON.
pub fn opt_json_col<T: serde::de::DeserializeOwned>(
    row: &rusqlite::Row<'_>,
    index: usize,
) -> rusqlite::Result<Option<T>> {
    match row.get::<_, Option<String>>(index)? {
        None => Ok(None),
        Some(text) => serde_json::from_str(&text)
            .map(Some)
            .map_err(|err| conversion_failed(index, err)),
    }
}

/// Reads a `TEXT` column holding one of an enum's stored spellings.
///
/// The parser is the DTO's own `parse`, so the set of values a column may hold
/// is defined once, in `rustak-api`, rather than again here.
pub fn enum_col<T>(
    row: &rusqlite::Row<'_>,
    index: usize,
    parse: impl Fn(&str) -> Option<T>,
) -> rusqlite::Result<T> {
    let text: String = row.get(index)?;

    parse(&text).ok_or_else(|| conversion_failed(index, UnknownValue(text)))
}

/// Serialises a value for a JSON column.
pub fn to_json<T: serde::Serialize>(value: &T) -> rusqlite::Result<String> {
    serde_json::to_string(value)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))
}

/// A stored value no current variant of the column's enum spells.
///
/// Carried as an error rather than mapped onto a fallback, because a row we
/// cannot interpret must not be silently treated as some other kind of row.
#[derive(Debug)]
struct UnknownValue(String);

impl std::fmt::Display for UnknownValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "stored value '{}' is not one this column may hold",
            self.0
        )
    }
}

impl std::error::Error for UnknownValue {}

fn conversion_failed(
    index: usize,
    err: impl std::error::Error + Send + Sync + 'static,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(err))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustak_api::UserKind;

    fn row_with<T>(sql: &str, read: impl FnOnce(&rusqlite::Row<'_>) -> rusqlite::Result<T>) -> T {
        let connection = rusqlite::Connection::open_in_memory().unwrap();

        connection.query_one(sql, [], |row| read(row)).unwrap()
    }

    #[test]
    fn timestamps_are_rfc3339_with_exactly_three_fractional_digits() {
        let at = Timestamp::parse("2026-09-18T10:00:00.123456Z").unwrap();

        assert_eq!(at.to_text(), "2026-09-18T10:00:00.123Z");
    }

    #[test]
    fn timestamps_offered_with_an_offset_are_normalised_to_utc() {
        let at = Timestamp::parse("2026-09-18T12:00:00.000+02:00").unwrap();

        assert_eq!(at.to_text(), "2026-09-18T10:00:00.000Z");
    }

    #[test]
    fn a_timestamp_reads_back_exactly_as_it_was_written() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute("CREATE TABLE t (at TEXT NOT NULL) STRICT", [])
            .unwrap();

        let written = Timestamp::now();
        connection
            .execute("INSERT INTO t (at) VALUES (?1)", [written])
            .unwrap();

        let read: Timestamp = connection
            .query_one("SELECT at FROM t", [], |row| row.get(0))
            .unwrap();

        assert_eq!(read, written);
        assert_eq!(read.get(), written.get());
    }

    #[test]
    fn stored_timestamps_sort_in_chronological_order_as_text() {
        let earlier = Timestamp::parse("2026-09-18T09:59:59.999Z").unwrap();
        let later = Timestamp::parse("2026-09-18T10:00:00.000Z").unwrap();

        assert!(earlier.to_text() < later.to_text());
    }

    #[test]
    fn a_timestamp_we_cannot_parse_is_an_error_rather_than_a_default() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();

        let read: rusqlite::Result<Timestamp> =
            connection.query_one("SELECT 'not a time'", [], |row| row.get(0));

        assert!(read.is_err());
    }

    #[test]
    fn booleans_read_back_from_their_integer_column() {
        assert!(row_with("SELECT 1", |row| bool_col(row, 0)));
        assert!(!row_with("SELECT 0", |row| bool_col(row, 0)));
    }

    #[test]
    fn identifiers_read_back_into_their_own_type() {
        let id: rustak_api::UserId = row_with("SELECT 7", |row| id_col(row, 0));
        assert_eq!(id, rustak_api::UserId::new(7));

        let absent: Option<rustak_api::DeviceId> =
            row_with("SELECT NULL", |row| opt_id_col(row, 0));
        assert_eq!(absent, None);
    }

    #[test]
    fn json_columns_round_trip() {
        let encoded = to_json(&vec!["a", "b"]).unwrap();
        let decoded: Vec<String> = row_with(&format!("SELECT '{encoded}'"), |row| json_col(row, 0));

        assert_eq!(decoded, vec!["a".to_string(), "b".to_string()]);

        let absent: Option<Vec<String>> = row_with("SELECT NULL", |row| opt_json_col(row, 0));
        assert_eq!(absent, None);
    }

    #[test]
    fn an_unknown_enum_value_fails_the_read_rather_than_picking_a_variant() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();

        let read: rusqlite::Result<UserKind> = connection.query_one("SELECT 'robot'", [], |row| {
            enum_col(row, 0, UserKind::parse)
        });

        let message = read.unwrap_err().to_string();
        assert!(message.contains("robot"), "{message}");
    }
}
