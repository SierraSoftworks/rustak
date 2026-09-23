//! What the pop-over is open on.
//!
//! A click on the map lands on nothing, on one thing, or on several — and the
//! last is common: a person standing on a marker, the two ends of a route, a
//! dozen aircraft over one airport at the zoom somebody happens to be at. The
//! topmost is not the answer, because which is topmost is an accident of draw
//! order. So, as ATAK does, several is a question put to whoever clicked: the
//! pop-over opens where they clicked and lists what was there.

use serde::Deserialize;

use super::sketch::Near;

/// One click, as `js/map.js` reports it: everything under it, topmost first,
/// and where it was as `[lon, lat]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Pick {
    pub uids: Vec<String>,
    pub at: [f64; 2],
    /// While something is being drawn: the end of it the click landed on.
    #[serde(default)]
    pub near: Option<Near>,
}

/// What the pop-over is showing.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Focus {
    /// It is closed.
    #[default]
    Nothing,

    /// One feature's details, following the feature as it moves.
    Feature(String),

    /// The chooser: the features that were under a click, at the place it was.
    Choosing { uids: Vec<String>, at: [f64; 2] },
}

impl From<Pick> for Focus {
    fn from(pick: Pick) -> Self {
        let Pick { mut uids, at, .. } = pick;

        match uids.len() {
            0 => Self::Nothing,
            1 => Self::Feature(uids.remove(0)),
            _ => Self::Choosing { uids, at },
        }
    }
}

impl Focus {
    /// The feature whose details are open, if that is what is open.
    pub fn feature(&self) -> Option<&str> {
        match self {
            Self::Feature(uid) => Some(uid),
            _ => None,
        }
    }

    /// What is left of this focus once `gone` have left the map, or [`None`]
    /// when it is untouched.
    ///
    /// Details of something that is gone close. A chooser keeps whatever is
    /// still there — even one thing, because swapping a list somebody is
    /// reading for a details pane they did not ask for is a worse surprise
    /// than a list of one.
    pub fn without(&self, gone: &[String]) -> Option<Self> {
        match self {
            Self::Feature(uid) if gone.contains(uid) => Some(Self::Nothing),
            Self::Choosing { uids, at } if uids.iter().any(|uid| gone.contains(uid)) => {
                let left: Vec<String> = uids
                    .iter()
                    .filter(|uid| !gone.contains(uid))
                    .cloned()
                    .collect();

                Some(match left.is_empty() {
                    true => Self::Nothing,
                    false => Self::Choosing {
                        uids: left,
                        at: *at,
                    },
                })
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pick(uids: &[&str]) -> Pick {
        Pick {
            uids: uids.iter().map(|uid| (*uid).to_string()).collect(),
            at: [-0.12, 51.5],
            near: None,
        }
    }

    #[test]
    fn a_click_is_read_as_the_glue_spells_it() {
        let read: Pick =
            serde_json::from_str(r#"{ "uids": ["A", "B"], "at": [-0.12, 51.5] }"#).unwrap();

        assert_eq!(read, pick(&["A", "B"]));
    }

    #[test]
    fn nothing_closes_one_selects_and_several_is_a_question() {
        assert_eq!(Focus::from(pick(&[])), Focus::Nothing);
        assert_eq!(Focus::from(pick(&["A"])), Focus::Feature("A".to_string()));
        assert_eq!(
            Focus::from(pick(&["A", "B"])),
            Focus::Choosing {
                uids: vec!["A".to_string(), "B".to_string()],
                at: [-0.12, 51.5],
            }
        );
    }

    #[test]
    fn what_leaves_the_map_leaves_the_pop_over() {
        let gone = ["A".to_string()];

        assert_eq!(
            Focus::Feature("A".to_string()).without(&gone),
            Some(Focus::Nothing)
        );
        assert_eq!(Focus::Feature("B".to_string()).without(&gone), None);

        assert_eq!(
            Focus::from(pick(&["A", "B"])).without(&gone),
            Some(Focus::Choosing {
                uids: vec!["B".to_string()],
                at: [-0.12, 51.5],
            })
        );
        assert_eq!(Focus::from(pick(&["B", "C"])).without(&gone), None);
        assert_eq!(
            Focus::from(pick(&["A", "A2"])).without(&["A".to_string(), "A2".to_string()]),
            Some(Focus::Nothing)
        );
    }
}
