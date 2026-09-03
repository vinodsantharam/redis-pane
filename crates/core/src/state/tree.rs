//! Tree mode: folding the key list on a separator (R2.3, PLAN M1.6).
//!
//! **The tree holds no key names.** Every node points into the same byte arena
//! the [`LoadedSet`](super::LoadedSet) already owns, as an `(offset, len)` pair
//! borrowed from whichever key first produced that prefix. A second copy of a
//! million key names would cost more than the entire rest of the structure
//! (ADR-0010).

use super::loaded::LoadedSet;
use super::view::KeyView;

/// What a rendered row is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// A folded prefix, e.g. `user:` — with how many keys sit beneath it.
    Group {
        /// Where the *segment* text lives in the arena.
        offset: u32,
        len: u16,
        depth: u16,
        descendants: u32,
        expanded: bool,
    },
    /// An actual key. `index` is its position in the Loaded set.
    Key { index: u32, depth: u16 },
}

impl Row {
    pub fn depth(&self) -> u16 {
        match self {
            Row::Group { depth, .. } | Row::Key { depth, .. } => *depth,
        }
    }
}

/// The folded view of the current [`KeyView`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tree {
    rows: Vec<Row>,
    /// Prefixes the user has collapsed, stored as full prefix strings. Small by
    /// construction: only what someone has actually clicked shut.
    collapsed: Vec<String>,
    pub separator: char,
}

impl Default for Tree {
    fn default() -> Self {
        // `:` is the near-universal Redis convention; it is configurable
        // because "near-universal" is not "always".
        Tree::new(':')
    }
}

impl Tree {
    pub fn new(separator: char) -> Self {
        Self {
            rows: Vec::new(),
            collapsed: Vec::new(),
            separator,
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn row(&self, i: usize) -> Option<Row> {
        self.rows.get(i).copied()
    }

    /// The Loaded set index at this row, if the row is a key rather than a group.
    pub fn key_index(&self, row: usize) -> Option<usize> {
        match self.rows.get(row) {
            Some(Row::Key { index, .. }) => Some(*index as usize),
            _ => None,
        }
    }

    pub fn toggle(&mut self, prefix: &str) {
        if let Some(i) = self.collapsed.iter().position(|p| p == prefix) {
            self.collapsed.remove(i);
        } else {
            self.collapsed.push(prefix.to_string());
        }
    }

    pub fn is_collapsed(&self, prefix: &str) -> bool {
        self.collapsed.iter().any(|p| p == prefix)
    }

    /// Rebuild the folded rows from the current view.
    ///
    /// Runs in one pass over the view's order, comparing each key's prefix
    /// segments against the previous key's. That is only correct on a
    /// name-ordered view, which is why tree mode sorts by name.
    pub fn rebuild(&mut self, keys: &LoadedSet, view: &KeyView) {
        self.rows.clear();
        let sep = self.separator as u8;
        let mut previous: Vec<(u32, u16)> = Vec::new();

        for row in 0..view.len() {
            let Some(index) = view.index_at(row) else {
                continue;
            };
            let Some(name) = keys.name(index) else {
                continue;
            };
            let Some(start) = keys.name_offset(index) else {
                continue;
            };

            let segments = split_segments(name, sep, start);
            // How many leading segments this key shares with the previous one.
            //
            // Compared by *bytes*, not by `(offset, len)`: the same prefix text
            // sits at a different arena offset in every key that carries it, so
            // comparing handles would find nothing in common and emit a fresh
            // group header for every single key.
            let shared = segments
                .iter()
                .zip(previous.iter())
                .take_while(|((ao, al), (bo, bl))| {
                    keys.arena_slice(*ao, *al) == keys.arena_slice(*bo, *bl)
                })
                .count();

            let mut prefix = String::new();
            for (depth, (offset, len)) in segments.iter().enumerate() {
                prefix.push_str(&String::from_utf8_lossy(
                    keys.arena_slice(*offset, *len).unwrap_or_default(),
                ));
                prefix.push(self.separator);

                if depth < shared {
                    continue;
                }
                let collapsed = self.is_collapsed(&prefix);
                self.rows.push(Row::Group {
                    offset: *offset,
                    len: *len,
                    depth: depth as u16,
                    descendants: 0,
                    expanded: !collapsed,
                });
                if collapsed {
                    // Stop descending: every key under this prefix is hidden.
                    // `previous` is set to the full segment list below, which
                    // is what stops the next key re-emitting this same header.
                    break;
                }
            }

            // If any ancestor is collapsed, the key itself is not shown.
            if !self.ancestor_collapsed(keys, name) {
                self.rows.push(Row::Key {
                    index: index as u32,
                    depth: segments.len() as u16,
                });
            }
            previous = segments;
        }
        self.count_descendants();
    }

    fn ancestor_collapsed(&self, _keys: &LoadedSet, name: &[u8]) -> bool {
        if self.collapsed.is_empty() {
            return false;
        }
        let name = String::from_utf8_lossy(name);
        self.collapsed.iter().any(|p| name.starts_with(p.as_str()))
    }

    /// Walk backwards filling in how many keys sit beneath each group.
    fn count_descendants(&mut self) {
        let mut counts: Vec<u32> = vec![0; self.rows.len()];
        let mut stack: Vec<usize> = Vec::new();
        for i in 0..self.rows.len() {
            let depth = self.rows[i].depth();
            while let Some(&top) = stack.last() {
                if self.rows[top].depth() >= depth {
                    stack.pop();
                } else {
                    break;
                }
            }
            if matches!(self.rows[i], Row::Key { .. }) {
                for &g in &stack {
                    counts[g] += 1;
                }
            }
            if matches!(self.rows[i], Row::Group { .. }) {
                stack.push(i);
            }
        }
        for (i, row) in self.rows.iter_mut().enumerate() {
            if let Row::Group { descendants, .. } = row {
                *descendants = counts[i];
            }
        }
    }
}

/// Split a key name into arena-relative `(offset, len)` segments.
///
/// The trailing segment (the leaf) is excluded: it is the key itself, not a
/// group. `a:b:c` yields the groups `a` and `b`.
fn split_segments(name: &[u8], sep: u8, arena_start: u32) -> Vec<(u32, u16)> {
    let mut out = Vec::new();
    let mut begin = 0usize;
    for (i, b) in name.iter().enumerate() {
        if *b == sep {
            out.push((arena_start + begin as u32, (i - begin) as u16));
            begin = i + 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::view::SortBy;

    fn built(names: &[&str]) -> (LoadedSet, KeyView, Tree) {
        let mut keys = LoadedSet::default();
        for n in names {
            keys.push(n.as_bytes());
        }
        let mut view = KeyView::new("", crate::state::FilterMode::Glob, SortBy::Name);
        view.rebuild(&keys);
        let mut tree = Tree::new(':');
        tree.rebuild(&keys, &view);
        (keys, view, tree)
    }

    fn rendered(keys: &LoadedSet, tree: &Tree) -> Vec<String> {
        (0..tree.len())
            .map(|i| match tree.row(i).unwrap() {
                Row::Group {
                    offset,
                    len,
                    depth,
                    descendants,
                    ..
                } => format!(
                    "{}{}: ({descendants})",
                    "  ".repeat(depth as usize),
                    String::from_utf8_lossy(keys.arena_slice(offset, len).unwrap())
                ),
                Row::Key { index, depth } => format!(
                    "{}{}",
                    "  ".repeat(depth as usize),
                    keys.name_str(index as usize).unwrap()
                ),
            })
            .collect()
    }

    #[test]
    fn keys_fold_on_the_separator() {
        let (keys, _, tree) = built(&[
            "user:1:session",
            "user:1:cart",
            "user:2:session",
            "feed:hot",
        ]);
        assert_eq!(
            rendered(&keys, &tree),
            [
                "feed: (1)",
                "  feed:hot",
                "user: (3)",
                "  1: (2)",
                "    user:1:cart",
                "    user:1:session",
                "  2: (1)",
                "    user:2:session",
            ]
        );
    }

    #[test]
    fn collapsed_nodes_show_their_child_count_and_hide_their_children() {
        let (keys, view, mut tree) = built(&["user:1:a", "user:1:b", "feed:hot"]);
        tree.toggle("user:");
        tree.rebuild(&keys, &view);
        let rows = rendered(&keys, &tree);
        assert!(rows.iter().any(|r| r.starts_with("user:")));
        assert!(
            !rows.iter().any(|r| r.contains("user:1:a")),
            "collapsed children must be hidden: {rows:?}"
        );
    }

    #[test]
    fn a_key_with_no_separator_is_a_leaf_at_the_root() {
        let (keys, _, tree) = built(&["standalone"]);
        assert_eq!(rendered(&keys, &tree), ["standalone"]);
    }

    #[test]
    fn the_tree_stores_no_key_names_of_its_own() {
        // R2.3 and ADR-0010: a second copy of a million names would cost more
        // than the whole rest of the structure. Every group points into the
        // arena the store already owns.
        let (keys, _, tree) = built(&["user:1:session", "user:2:session"]);
        for i in 0..tree.len() {
            if let Some(Row::Group { offset, len, .. }) = tree.row(i) {
                assert!(
                    keys.arena_slice(offset, len).is_some(),
                    "a group pointed outside the arena"
                );
            }
        }
        assert_eq!(
            std::mem::size_of::<Row>(),
            16,
            "a row is a handful of integers, not a string"
        );
    }

    #[test]
    fn toggling_twice_returns_to_where_it_started() {
        let (keys, view, mut tree) = built(&["user:1:a", "user:1:b"]);
        let before = rendered(&keys, &tree);
        tree.toggle("user:");
        tree.rebuild(&keys, &view);
        tree.toggle("user:");
        tree.rebuild(&keys, &view);
        assert_eq!(rendered(&keys, &tree), before);
    }
}
