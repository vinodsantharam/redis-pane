//! What the key list currently shows: filtered, ordered, and pointing into the
//! arena (R2.2, R2.5, ADR-0010).
//!
//! Filtering and sorting never touch the [`LoadedSet`](super::LoadedSet). They
//! rebuild an index vector — `Vec<u32>` of positions into it — which is the
//! whole reason the store is columnar. A million-key sort permutes 4MB of
//! indices, not 40MB of names.

use super::loaded::LoadedSet;

/// How a filter pattern is interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FilterMode {
    /// Redis-style `*` and `?`, which is what users of `redis-cli` already know.
    #[default]
    Glob,
    /// Characters in order but not adjacent, for when you half-remember a name.
    Fuzzy,
}

/// Which column orders the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortBy {
    /// Scan order — the order `SCAN` happened to return keys in. Not stable
    /// across scans, and honest about that: it is not called "natural".
    #[default]
    Scan,
    Name,
    Ttl,
    Size,
    Kind,
}

impl SortBy {
    pub fn label(&self) -> &'static str {
        match self {
            SortBy::Scan => "scan order",
            SortBy::Name => "name",
            SortBy::Ttl => "ttl",
            SortBy::Size => "size",
            SortBy::Kind => "type",
        }
    }

    /// Whether this column is fetched lazily, and so may be partly unknown.
    pub fn is_lazy(&self) -> bool {
        matches!(self, SortBy::Ttl | SortBy::Size | SortBy::Kind)
    }

    /// The cycle `s` walks through.
    pub fn next(&self) -> SortBy {
        match self {
            SortBy::Scan => SortBy::Name,
            SortBy::Name => SortBy::Ttl,
            SortBy::Ttl => SortBy::Size,
            SortBy::Size => SortBy::Kind,
            SortBy::Kind => SortBy::Scan,
        }
    }
}

/// The ordered, filtered list of rows on offer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KeyView {
    order: Vec<u32>,
    pub filter: String,
    pub mode: FilterMode,
    pub sort: SortBy,
    /// How many of the shown rows have the sort column fetched (R2.5).
    known: usize,
}

impl KeyView {
    /// A view with a given filter and sort. The order is built by
    /// [`KeyView::rebuild`], never set directly.
    pub fn new(filter: impl Into<String>, mode: FilterMode, sort: SortBy) -> Self {
        Self {
            order: Vec::new(),
            filter: filter.into(),
            mode,
            sort,
            known: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// The Loaded set index shown at this row.
    pub fn index_at(&self, row: usize) -> Option<usize> {
        self.order.get(row).map(|i| *i as usize)
    }

    /// How many rows carry a value for the sort column.
    ///
    /// Sorting by a lazily-fetched column orders what has arrived and states
    /// this count; it never triggers a mass fetch (R2.5).
    pub fn known(&self) -> usize {
        self.known
    }

    /// What the filter line reads, e.g. `3,410 match`.
    pub fn match_readout(&self, total: usize) -> String {
        if self.filter.is_empty() {
            String::new()
        } else {
            format!("{} of {}", self.len(), total)
        }
    }

    /// What the sort readout says when the column is only partly known.
    pub fn sort_readout(&self) -> Option<String> {
        if self.sort == SortBy::Scan {
            return None;
        }
        if self.sort.is_lazy() && self.known < self.len() {
            Some(format!(
                "sorted by {} · {} of {} known",
                self.sort.label(),
                self.known,
                self.len()
            ))
        } else {
            Some(format!("sorted by {}", self.sort.label()))
        }
    }

    /// Recompute the order from the store. Cheap enough to run on every change.
    pub fn rebuild(&mut self, keys: &LoadedSet) {
        self.order.clear();
        self.order.reserve(keys.len());
        for i in 0..keys.len() {
            if self.filter.is_empty() {
                self.order.push(i as u32);
            } else if let Some(name) = keys.name(i)
                && matches(name, &self.filter, self.mode)
            {
                self.order.push(i as u32);
            }
        }
        self.apply_sort(keys);
    }

    fn apply_sort(&mut self, keys: &LoadedSet) {
        self.known = match self.sort {
            SortBy::Scan | SortBy::Name => self.order.len(),
            SortBy::Ttl => self
                .order
                .iter()
                .filter(|i| keys.ttl(**i as usize).is_some())
                .count(),
            SortBy::Size => self
                .order
                .iter()
                .filter(|i| keys.size(**i as usize).is_some())
                .count(),
            SortBy::Kind => self
                .order
                .iter()
                .filter(|i| keys.kind(**i as usize).is_some())
                .count(),
        };

        match self.sort {
            SortBy::Scan => {}
            SortBy::Name => {
                self.order
                    .sort_by(|a, b| keys.name(*a as usize).cmp(&keys.name(*b as usize)));
            }
            // For every lazily-fetched column the rule is the same: order what
            // arrived, park the unknowns at the end in scan order, and say how
            // many were sorted. The alternative — firing a million commands
            // because somebody pressed a key — is the behaviour this project
            // exists to replace.
            SortBy::Ttl => self.sort_by_lazy(|i| keys.ttl(i).map(ttl_rank)),
            SortBy::Size => self.sort_by_lazy(|i| keys.size(i)),
            SortBy::Kind => self.sort_by_lazy(|i| keys.kind(i).map(|k| k as u8)),
        }
    }

    fn sort_by_lazy<T: Ord, F: Fn(usize) -> Option<T>>(&mut self, value: F) {
        self.order.sort_by(|a, b| {
            match (value(*a as usize), value(*b as usize)) {
                (Some(x), Some(y)) => x.cmp(&y).then(a.cmp(b)),
                // Unknown sorts last, and keeps scan order among itself so the
                // tail does not shuffle every time one more value arrives.
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.cmp(b),
            }
        });
    }
}

/// A key with no expiry sorts after every key that has one: `∞` is the largest
/// TTL there is, not a missing value.
fn ttl_rank(seconds: i32) -> i64 {
    if seconds == super::loaded::TTL_NONE {
        i64::MAX
    } else {
        seconds as i64
    }
}

/// Whether a key name matches a pattern.
pub fn matches(name: &[u8], pattern: &str, mode: FilterMode) -> bool {
    match mode {
        FilterMode::Glob => glob(name, pattern.as_bytes()),
        FilterMode::Fuzzy => fuzzy(name, pattern.as_bytes()),
    }
}

/// Redis-style glob: `*` any run, `?` any single byte.
///
/// A pattern with no wildcards is treated as a substring search, because that
/// is what people mean when they type `session` into a filter box.
fn glob(name: &[u8], pattern: &[u8]) -> bool {
    if !pattern.contains(&b'*') && !pattern.contains(&b'?') {
        return name
            .windows(pattern.len().max(1))
            .any(|w| w.eq_ignore_ascii_case(pattern))
            || pattern.is_empty();
    }
    glob_at(name, pattern)
}

fn glob_at(name: &[u8], pattern: &[u8]) -> bool {
    let (mut n, mut p) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while n < name.len() {
        if p < pattern.len() && (pattern[p] == b'?' || eq_ci(pattern[p], name[n])) {
            n += 1;
            p += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = p;
            mark = n;
            p += 1;
        } else if star != usize::MAX {
            p = star + 1;
            mark += 1;
            n = mark;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

/// Characters in order, not necessarily adjacent.
fn fuzzy(name: &[u8], pattern: &[u8]) -> bool {
    let mut it = name.iter();
    pattern.iter().all(|c| it.any(|n| eq_ci(*c, *n)))
}

fn eq_ci(a: u8, b: u8) -> bool {
    a.eq_ignore_ascii_case(&b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(names: &[&str]) -> LoadedSet {
        let mut s = LoadedSet::default();
        for n in names {
            s.push(n.as_bytes());
        }
        s
    }

    fn viewed(keys: &LoadedSet, view: &KeyView) -> Vec<String> {
        (0..view.len())
            .map(|r| {
                keys.name_str(view.index_at(r).unwrap())
                    .unwrap()
                    .to_string()
            })
            .collect()
    }

    // ── filtering (M1.5) ────────────────────────────────────────────────────

    #[test]
    fn a_glob_filters_the_way_redis_cli_users_expect() {
        let keys = store(&[
            "user:1:session",
            "user:2:session",
            "user:1:cart",
            "feed:hot",
        ]);
        let mut v = KeyView {
            filter: "user:*:session".into(),
            ..KeyView::default()
        };
        v.rebuild(&keys);
        assert_eq!(viewed(&keys, &v), ["user:1:session", "user:2:session"]);
    }

    #[test]
    fn a_bare_word_is_a_substring_search_because_that_is_what_people_mean() {
        let keys = store(&["user:1:session", "user:1:cart", "feed:hot"]);
        let mut v = KeyView {
            filter: "cart".into(),
            ..KeyView::default()
        };
        v.rebuild(&keys);
        assert_eq!(viewed(&keys, &v), ["user:1:cart"]);
    }

    #[test]
    fn question_mark_matches_exactly_one() {
        let keys = store(&["k:1", "k:12", "k:2"]);
        let mut v = KeyView {
            filter: "k:?".into(),
            ..KeyView::default()
        };
        v.rebuild(&keys);
        assert_eq!(viewed(&keys, &v), ["k:1", "k:2"]);
    }

    #[test]
    fn fuzzy_finds_a_half_remembered_name() {
        let keys = store(&["user:8812:session", "feed:global:hot", "cart:91af"]);
        let mut v = KeyView {
            filter: "usession".into(),
            mode: FilterMode::Fuzzy,
            ..KeyView::default()
        };
        v.rebuild(&keys);
        assert_eq!(viewed(&keys, &v), ["user:8812:session"]);
    }

    #[test]
    fn an_empty_filter_shows_everything() {
        let keys = store(&["a", "b", "c"]);
        let mut v = KeyView::default();
        v.rebuild(&keys);
        assert_eq!(v.len(), 3);
        assert_eq!(v.match_readout(3), "", "no filter, so nothing to report");
    }

    #[test]
    fn the_match_count_is_stated_against_the_whole_loaded_set() {
        let keys = store(&["user:1", "user:2", "feed:1"]);
        let mut v = KeyView {
            filter: "user:*".into(),
            ..KeyView::default()
        };
        v.rebuild(&keys);
        assert_eq!(v.match_readout(keys.len()), "2 of 3");
    }

    #[test]
    fn filtering_leaves_the_store_untouched() {
        // ADR-0010: filtering permutes an index vector, never the arena.
        let keys = store(&["a", "b", "c"]);
        let before = keys.clone();
        let mut v = KeyView {
            filter: "a".into(),
            ..KeyView::default()
        };
        v.rebuild(&keys);
        assert_eq!(keys, before);
    }

    // ── sorting (M1.7) ──────────────────────────────────────────────────────

    #[test]
    fn sorting_by_name_is_never_partial_because_names_are_never_lazy() {
        let keys = store(&["c", "a", "b"]);
        let mut v = KeyView {
            sort: SortBy::Name,
            ..KeyView::default()
        };
        v.rebuild(&keys);
        assert_eq!(viewed(&keys, &v), ["a", "b", "c"]);
        assert_eq!(v.sort_readout().as_deref(), Some("sorted by name"));
    }

    #[test]
    fn sorting_by_size_orders_what_arrived_and_parks_the_rest() {
        let mut keys = store(&["big", "small", "unknown-a", "unknown-b"]);
        keys.set_size(0, 5_000);
        keys.set_size(1, 100);
        let mut v = KeyView {
            sort: SortBy::Size,
            ..KeyView::default()
        };
        v.rebuild(&keys);

        assert_eq!(
            viewed(&keys, &v),
            ["small", "big", "unknown-a", "unknown-b"],
            "known values sort; unknowns go to the end in scan order"
        );
        assert_eq!(v.known(), 2);
        assert_eq!(
            v.sort_readout().as_deref(),
            Some("sorted by size · 2 of 4 known"),
            "the reader must be told the sort is partial"
        );
    }

    #[test]
    fn a_key_with_no_expiry_sorts_after_every_key_that_has_one() {
        // ∞ is the largest TTL there is, not a missing value.
        let mut keys = store(&["forever", "soon", "later"]);
        keys.set_ttl(0, super::super::loaded::TTL_NONE, 0);
        keys.set_ttl(1, 30, 0);
        keys.set_ttl(2, 3_600, 0);
        let mut v = KeyView {
            sort: SortBy::Ttl,
            ..KeyView::default()
        };
        v.rebuild(&keys);
        assert_eq!(viewed(&keys, &v), ["soon", "later", "forever"]);
        assert_eq!(v.known(), 3, "no expiry is a known fact, not a gap");
    }

    /// Sort-by-TTL must not reshuffle rows once a second as their countdowns
    /// cross each other — that would look broken, not live. `sort_by_lazy`
    /// reads the raw, unmoving `ttl()`, never `ttl_now()`; this pins that as a
    /// property of the sort rather than an accident of which function nobody
    /// happened to call.
    #[test]
    fn sort_by_ttl_does_not_reshuffle_as_the_clock_advances() {
        let mut keys = store(&["soon", "later"]);
        keys.set_ttl(0, 10, 0);
        keys.set_ttl(1, 20, 0);
        let mut v = KeyView {
            sort: SortBy::Ttl,
            ..KeyView::default()
        };
        v.rebuild(&keys);
        let order = viewed(&keys, &v);
        assert_eq!(order, ["soon", "later"]);

        // 15 seconds on: "soon"'s displayed countdown would now read behind
        // "later"'s original number, but nothing here has been re-sorted —
        // rebuild wasn't even called again.
        assert_eq!(keys.ttl_now(0, 15), Some(0));
        assert_eq!(keys.ttl_now(1, 15), Some(5));
        assert_eq!(
            viewed(&keys, &v),
            order,
            "the clock alone must not change sort order"
        );
    }

    #[test]
    fn the_unknown_tail_does_not_shuffle_as_values_trickle_in() {
        // Otherwise the list would churn under the reader while metadata loads.
        let mut keys = store(&["a", "b", "c", "d"]);
        keys.set_size(0, 10);
        let mut v = KeyView {
            sort: SortBy::Size,
            ..KeyView::default()
        };
        v.rebuild(&keys);
        let tail_before = viewed(&keys, &v)[1..].to_vec();

        keys.set_size(3, 5);
        v.rebuild(&keys);
        let after = viewed(&keys, &v);
        assert_eq!(&after[..2], ["d", "a"], "the two known sizes sort");
        assert_eq!(
            &after[2..],
            &tail_before[..2],
            "the unknown tail kept its order"
        );
    }

    #[test]
    fn sorting_never_asks_for_anything() {
        // The proof for R2.5 is a negative: this function has no way to fetch.
        // It takes an immutable store and returns an ordering, so a mass fetch
        // is not merely avoided, it is unrepresentable.
        let keys = store(&["a", "b"]);
        let before = keys.clone();
        let mut v = KeyView {
            sort: SortBy::Size,
            ..KeyView::default()
        };
        v.rebuild(&keys);
        assert_eq!(keys, before);
        assert_eq!(v.known(), 0);
    }

    #[test]
    fn the_sort_cycle_returns_to_where_it_started() {
        let mut s = SortBy::Scan;
        for _ in 0..5 {
            s = s.next();
        }
        assert_eq!(s, SortBy::Scan);
    }

    #[test]
    fn scan_order_is_named_honestly() {
        // SCAN guarantees no ordering, so calling this "natural" would be a lie.
        assert_eq!(SortBy::Scan.label(), "scan order");
        assert_eq!(KeyView::default().sort_readout(), None);
    }

    #[test]
    fn filter_and_sort_compose() {
        let mut keys = store(&["user:b", "user:a", "feed:z"]);
        keys.set_size(0, 200);
        keys.set_size(1, 100);
        let mut v = KeyView {
            filter: "user:*".into(),
            sort: SortBy::Size,
            ..KeyView::default()
        };
        v.rebuild(&keys);
        assert_eq!(viewed(&keys, &v), ["user:a", "user:b"]);
    }
}
