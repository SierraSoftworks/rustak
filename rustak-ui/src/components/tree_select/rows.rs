//! Which rows a [`TreeSelect`](super::TreeSelect) is showing.
//!
//! Two modes, decided by whether anything has been typed. With nothing typed
//! the list is one level of the hierarchy, and somebody walks down it. With
//! something typed it is every match from the whole tree, best first — which
//! is what makes 863 symbols usable by somebody who knows the word "mortar"
//! and not where 2525 files it.

use super::model::Tree;

/// How many matches are worth drawing. Past this, typing another letter is
/// quicker than scrolling.
pub const SEARCH_LIMIT: usize = 40;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    /// Go back to having nothing chosen.
    Clear,
    /// Use what was typed as it stands: the caller said it is well formed.
    Typed(String),
    /// Something in the tree.
    Item(usize),
}

/// The rows for `query` — or, with nothing typed, for the inside of `branch`.
pub fn rows(
    tree: &Tree,
    query: &str,
    branch: Option<usize>,
    clearable: bool,
    typed: Option<String>,
) -> Vec<Row> {
    let searching = !query.trim().is_empty();
    let found = match searching {
        true => tree.search(query, SEARCH_LIMIT),
        false => tree.level(branch).to_vec(),
    };

    // What was typed is only worth offering when the tree has no such value:
    // otherwise the same thing would be listed twice, once without its name.
    let typed = typed.filter(|value| tree.find(value).is_none());
    let clear = (clearable && !searching && branch.is_none()).then_some(Row::Clear);

    clear
        .into_iter()
        .chain(found.into_iter().map(Row::Item))
        .chain(typed.map(Row::Typed))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::model::Entry;
    use super::*;

    fn tree() -> Tree {
        let entry = |path: &[&str], value: &str| Entry {
            path: path.iter().map(|step| (*step).to_string()).collect(),
            value: Some(value.to_string()),
            ..Entry::default()
        };

        Tree::build([
            entry(&["Ground"], "a-u-G"),
            entry(&["Ground", "Unit", "Infantry"], "a-u-G-U-C-I"),
            entry(&["Air"], "a-u-A"),
        ])
    }

    #[test]
    fn nothing_typed_is_one_level_and_something_typed_is_the_whole_tree() {
        let tree = tree();
        let ground = tree.find("a-u-G");

        assert_eq!(rows(&tree, "", None, false, None).len(), 2);
        assert_eq!(rows(&tree, "  ", ground, false, None).len(), 1);
        assert_eq!(
            rows(&tree, "infantry", None, false, None),
            [Row::Item(tree.find("a-u-G-U-C-I").unwrap())]
        );
    }

    #[test]
    fn clearing_is_offered_at_the_top_and_what_was_typed_only_when_it_is_new() {
        let tree = tree();

        assert_eq!(rows(&tree, "", None, true, None)[0], Row::Clear);
        assert!(!rows(&tree, "", tree.find("a-u-G"), true, None).contains(&Row::Clear));
        assert!(!rows(&tree, "air", None, true, None).contains(&Row::Clear));

        let typed = |value: &str| rows(&tree, value, None, false, Some(value.to_string()));
        assert_eq!(
            typed("a-h-G-X").last(),
            Some(&Row::Typed("a-h-G-X".to_string()))
        );
        assert!(
            !typed("a-u-A")
                .iter()
                .any(|row| matches!(row, Row::Typed(_)))
        );
    }
}
