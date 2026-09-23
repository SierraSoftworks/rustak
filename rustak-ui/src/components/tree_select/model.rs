//! A hierarchy somebody picks one thing out of, and the search across it.
//!
//! Nothing here touches the browser, so all of it is tested natively. The
//! component in [`super`] only decides which of these rows is on screen.
//!
//! # Built from paths
//!
//! A catalogue is published as a list of things, each with the names that lead
//! to it: `["Ground track", "Unit", "Combat", "Infantry"]`. [`Tree::build`]
//! turns that into parents and children, and makes up a branch for any step
//! on the way that the catalogue never listed by itself. Such a branch can be
//! opened but not chosen: it has no value, because it is not a thing.

use std::collections::HashMap;

/// One thing as a catalogue lists it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    /// The names that lead here, outermost first, ending in this one's own.
    pub path: Vec<String>,
    /// What choosing it means. [`None`] for something that only holds others.
    pub value: Option<String>,
    /// Shown small beside the name: the code, for somebody who knows it.
    pub detail: Option<String>,
    /// What a caller's `render_icon` is handed to draw a preview from.
    pub icon: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeItem {
    pub label: String,
    pub value: Option<String>,
    pub detail: Option<String>,
    pub icon: Option<String>,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    /// Everything a search looks through, lowercased once: the names that lead
    /// here, this one's own, and its detail.
    haystack: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tree {
    items: Vec<TreeItem>,
    roots: Vec<usize>,
}

impl Tree {
    /// A tree of `entries`, in the order they were listed.
    pub fn build(entries: impl IntoIterator<Item = Entry>) -> Self {
        let mut tree = Self::default();
        let mut known: HashMap<Vec<String>, usize> = HashMap::new();

        for entry in entries {
            let mut parent = None;

            for depth in 1..=entry.path.len() {
                let steps = &entry.path[..depth];
                let at = match known.get(steps) {
                    Some(at) => *at,
                    None => {
                        let at = tree.branch(steps, parent);
                        known.insert(steps.to_vec(), at);
                        at
                    }
                };
                parent = Some(at);
            }

            if let Some(at) = parent {
                let item = &mut tree.items[at];
                item.haystack = haystack(&entry.path, entry.detail.as_deref());
                item.value = entry.value;
                item.detail = entry.detail;
                item.icon = entry.icon;
            }
        }

        tree
    }

    /// Adds a step nothing has been said about yet, under `parent`.
    fn branch(&mut self, steps: &[String], parent: Option<usize>) -> usize {
        let at = self.items.len();

        self.items.push(TreeItem {
            label: steps.last().cloned().unwrap_or_default(),
            value: None,
            detail: None,
            icon: None,
            parent,
            children: Vec::new(),
            haystack: haystack(steps, None),
        });
        match parent {
            Some(parent) => self.items[parent].children.push(at),
            None => self.roots.push(at),
        }

        at
    }

    pub fn item(&self, at: usize) -> Option<&TreeItem> {
        self.items.get(at)
    }

    /// What is directly inside `branch`, or at the top for [`None`].
    pub fn level(&self, branch: Option<usize>) -> &[usize] {
        match branch.and_then(|at| self.items.get(at)) {
            Some(item) => &item.children,
            None => &self.roots,
        }
    }

    /// The thing whose value is `value`.
    pub fn find(&self, value: &str) -> Option<usize> {
        self.items
            .iter()
            .position(|item| item.value.as_deref() == Some(value))
    }

    /// The names that lead to `at`, outermost first, without its own.
    pub fn trail(&self, at: usize) -> Vec<&str> {
        let mut trail = Vec::new();
        let mut next = self.items.get(at).and_then(|item| item.parent);

        while let Some(parent) = next {
            trail.push(self.items[parent].label.as_str());
            next = self.items[parent].parent;
        }

        trail.reverse();
        trail
    }

    /// The things that can be chosen and that every word of `query` is found
    /// in, best first: a name that *starts* with what was typed, then one that
    /// contains it, then one reached only through the names above it or its
    /// code — and, within each, the broader thing before the narrower one.
    pub fn search(&self, query: &str, limit: usize) -> Vec<usize> {
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        if words.is_empty() {
            return Vec::new();
        }

        let mut found: Vec<(u8, usize, usize)> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.value.is_some())
            .filter(|(_, item)| words.iter().all(|word| item.haystack.contains(word)))
            .map(|(at, item)| {
                let label = item.label.to_lowercase();
                let rank = match () {
                    () if label.starts_with(&words[0]) => 0,
                    () if words.iter().all(|word| label.contains(word)) => 1,
                    () => 2,
                };

                (rank, self.trail(at).len(), at)
            })
            .collect();

        found.sort_unstable();
        found.into_iter().take(limit).map(|(_, _, at)| at).collect()
    }
}

fn haystack(path: &[String], detail: Option<&str>) -> String {
    let mut all = path.join(" ");
    if let Some(detail) = detail {
        all.push(' ');
        all.push_str(detail);
    }

    all.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &[&str], value: &str) -> Entry {
        Entry {
            path: path.iter().map(|step| (*step).to_string()).collect(),
            value: Some(value.to_string()),
            detail: Some(value.to_string()),
            icon: None,
        }
    }

    /// A slice of 2525C: `Unit` is never listed by itself, as in the standard.
    fn tree() -> Tree {
        Tree::build([
            entry(&["Ground track"], "G"),
            entry(&["Ground track", "Unit", "Combat"], "G-U-C"),
            entry(&["Ground track", "Unit", "Combat", "Infantry"], "G-U-C-I"),
            entry(
                &[
                    "Ground track",
                    "Unit",
                    "Combat",
                    "Infantry",
                    "Infantry mortar",
                ],
                "G-U-C-I-O",
            ),
            entry(&["Air track"], "A"),
            entry(&["Air track", "Military", "Rotary wing"], "A-M-H"),
        ])
    }

    fn labels<'a>(tree: &'a Tree, found: &[usize]) -> Vec<&'a str> {
        found
            .iter()
            .filter_map(|at| tree.item(*at))
            .map(|item| item.label.as_str())
            .collect()
    }

    #[test]
    fn a_step_nobody_listed_can_be_opened_but_not_chosen() {
        let tree = tree();

        assert_eq!(
            labels(&tree, tree.level(None)),
            ["Ground track", "Air track"]
        );

        let ground = tree.find("G").expect("the ground track");
        let unit = tree.level(Some(ground))[0];
        let unit = tree.item(unit).expect("the branch");

        assert_eq!((unit.label.as_str(), unit.value.as_deref()), ("Unit", None));
        assert_eq!(unit.children.len(), 1);
    }

    #[test]
    fn something_is_found_with_the_names_that_lead_to_it() {
        let tree = tree();
        let infantry = tree.find("G-U-C-I").expect("infantry");

        assert_eq!(tree.trail(infantry), ["Ground track", "Unit", "Combat"]);
        assert_eq!(tree.find("G-X"), None);
    }

    #[test]
    fn a_search_prefers_the_name_typed_and_the_broader_of_two() {
        let tree = tree();

        for (query, expected) in [
            // Starts with it, broader first; then only reached through a name above.
            ("inf", vec!["Infantry", "Infantry mortar"]),
            // Every word, anywhere on the way down.
            ("air rotary", vec!["Rotary wing"]),
            // By its code, in either case.
            ("g-u-c-i-o", vec!["Infantry mortar"]),
            // A branch that was never a thing is not an answer.
            ("unit", vec!["Combat", "Infantry", "Infantry mortar"]),
            ("   ", vec![]),
            ("submarine", vec![]),
        ] {
            assert_eq!(
                labels(&tree, &tree.search(query, 10)),
                expected,
                "{query:?}"
            );
        }

        assert_eq!(tree.search("i", 2).len(), 2);
    }
}
