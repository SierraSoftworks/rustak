//! The catalogue of ATAK preferences the profile editor draws, as served.
//!
//! The entries themselves live in [`rustak_api::pref_catalog`], because the
//! console's offline demo needs the same list the server serves and a second
//! copy would drift. This module is the server's view of it: the document
//! `GET /api/v1/profiles/pref-catalog` answers with.
//!
//! Nothing here touches what a device receives. A profile's `.pref` document is
//! rendered from what is stored, in the order it is stored, whatever the
//! catalogue says about a key — a stored class the catalogue disagrees with is
//! delivered as stored, because it was an operator's choice and a server that
//! quietly "corrected" it would be delivering something nobody saved.

use serde_json::Value;

/// The catalogue, as a JSON Schema whose properties are preference keys.
pub fn schema() -> Value {
    rustak_api::pref_catalog::schema()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::prefs::enrollment_defaults;
    use rustak_api::PrefClass;
    use rustak_api::pref_catalog::{class_of, classes_of, reads_of};

    #[test]
    fn the_preference_everything_depends_on_is_in_it_as_atak_reads_it_and_as_we_send_it() {
        let schema = schema();
        let key = "deviceProfileEnableOnConnect";

        assert_eq!(reads_of(&schema, key), Some(PrefClass::Boolean));
        // A new entry gets the class this server's own enrolment profile
        // sends it as, and a Boolean is accepted as well.
        assert_eq!(class_of(&schema, key), Some(PrefClass::String));
        assert_eq!(
            classes_of(&schema, key),
            [PrefClass::String, PrefClass::Boolean]
        );
        assert_eq!(class_of(&schema, "somethingWeHaveNotCatalogued"), None);
    }

    #[test]
    fn every_key_the_enrolment_defaults_send_is_sent_as_the_class_new_entries_get() {
        let schema = schema();

        for entry in enrollment_defaults("tak.example.com", None).entries {
            if let Some(preferred) = class_of(&schema, &entry.key) {
                assert_eq!(preferred, entry.class, "{}", entry.key);
            }
        }
    }
}
