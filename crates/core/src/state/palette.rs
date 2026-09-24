//! The command palette's own state (`Ctrl-K`, PLAN M3 task 1).
//!
//! The Palette is the third reader of [`crate::keymap::Keymap`], not a new
//! source of truth — it searches [`crate::keymap::ALL_ACTIONS`], the same
//! list `label`/`description` are defined against, and its `Enter` handler
//! dispatches through the same step a direct keypress uses
//! (`update::dispatch_action`). See `docs/plans/m3-palette.md`.

use crate::keymap::{ALL_ACTIONS, Action};

/// Open, mid-query, or with a selection moved — an `Option<PaletteState>` on
/// [`crate::State`], matching the shape every other overlay already has
/// (`state.confirm`, `state.help_open`): "is the Palette open" is one field,
/// not a derived condition.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PaletteState {
    /// What the reader has typed so far.
    pub query: String,
    /// `ALL_ACTIONS`, filtered and ranked against `query` — recomputed
    /// wherever `query` changes, never patched incrementally, since the
    /// whole list is small enough that re-scoring it is not a cost worth
    /// tracking (PLAN M3 row 1's "under 40 actions").
    pub matches: Vec<Action>,
    /// Index into `matches` of the highlighted row. Clamped back to the last
    /// row whenever a keystroke shortens `matches` out from under it.
    pub selected: usize,
}

impl PaletteState {
    /// A freshly opened Palette: every action, in `ALL_ACTIONS` order, with
    /// nothing typed yet and the first row highlighted.
    pub fn new() -> Self {
        let mut state = Self::default();
        state.refresh();
        state
    }

    /// Re-run the fuzzy filter against the current `query` and reset the
    /// selection to the top match.
    ///
    /// Reset rather than preserved: `matches` is re-ranked from scratch on
    /// every keystroke, so "the third row" means something different before
    /// and after — keeping `selected` pinned to a position, not to the
    /// `Action` that used to be there, would silently follow whatever
    /// scrolled into that slot.
    pub fn refresh(&mut self) {
        self.matches = fuzzy_match(&self.query, ALL_ACTIONS);
        self.selected = 0;
    }

    /// The action a `Enter` right now would dispatch, if any.
    pub fn highlighted(&self) -> Option<Action> {
        self.matches.get(self.selected).copied()
    }

    /// Move the selection by `delta` rows, clamped to the match list — no
    /// wraparound, so repeatedly holding `↓` on a short list stops at the
    /// bottom rather than cycling past it unannounced.
    pub fn move_selection(&mut self, delta: isize) {
        if self.matches.is_empty() {
            self.selected = 0;
            return;
        }
        let max = self.matches.len() - 1;
        let next = (self.selected as isize + delta).clamp(0, max as isize);
        self.selected = next as usize;
    }
}

/// Every action whose label or description is a fuzzy subsequence match for
/// `query`, ranked best match first, ties broken by [`ALL_ACTIONS`] order so
/// the list does not reorder itself for no reason as the reader types.
///
/// A hand-rolled subsequence scorer rather than a `fuzzy-matcher`-style
/// dependency: this crate must stay `crossterm`/`tokio`/`fred`-free (the
/// `boundary` CI job, ADR-0011), and a pure matching crate would not
/// jeopardize that, but under 40 actions is not a case that needs one either
/// (PLAN M3 row 1's "Out of scope").
pub fn fuzzy_match(query: &str, actions: &[Action]) -> Vec<Action> {
    if query.is_empty() {
        return actions.to_vec();
    }
    let mut scored: Vec<(i32, usize, Action)> = actions
        .iter()
        .enumerate()
        .filter_map(|(order, &action)| {
            let haystack = format!("{} {}", action.label(), action.description());
            subsequence_score(query, &haystack).map(|score| (score, order, action))
        })
        .collect();
    // Highest score first; equal scores keep `ALL_ACTIONS` order, which is
    // what the stable sort below plus the `order` key gives for free.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, _, action)| action).collect()
}

/// Score one case-insensitive subsequence match of `query` inside
/// `haystack`, or `None` if `query` is not a subsequence at all.
///
/// Denser, more front-loaded, more word-boundary-aligned matches score
/// higher — the usual fuzzy-finder shape (fzf, VS Code's Quick Open): typing
/// `tro` should put "Toggle **r**ead-**o**nly" ahead of a hit that only
/// works by scattering three letters across a long description.
fn subsequence_score(query: &str, haystack: &str) -> Option<i32> {
    let q: Vec<char> = query.to_lowercase().chars().collect();
    let h: Vec<char> = haystack.to_lowercase().chars().collect();
    if q.is_empty() {
        return Some(0);
    }
    let mut qi = 0;
    let mut score = 0i32;
    let mut last_hit: Option<usize> = None;
    for (hi, &ch) in h.iter().enumerate() {
        if qi >= q.len() {
            break;
        }
        if ch != q[qi] {
            continue;
        }
        score += 10;
        if last_hit == Some(hi.wrapping_sub(1)) {
            // A run of consecutive characters is a much stronger signal
            // than the same letters found scattered across the row.
            score += 15;
        }
        if hi == 0 || h[hi - 1] == ' ' || h[hi - 1] == '-' || h[hi - 1] == '/' {
            score += 8;
        }
        last_hit = Some(hi);
        qi += 1;
    }
    if qi == q.len() {
        // Matching earlier in the haystack (a lower `last_hit`, for the same
        // query length) is rewarded relative to a match found only near the
        // end — a flat bonus against how far into `haystack` the match
        // finished, so "Filter" for `f` beats a description that happens to
        // contain an `f` only in its last word.
        score -= last_hit.unwrap_or(0) as i32;
        Some(score)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_returns_every_action_in_order() {
        let matches = fuzzy_match("", ALL_ACTIONS);
        assert_eq!(matches, ALL_ACTIONS.to_vec());
    }

    #[test]
    fn a_query_with_no_subsequence_match_returns_nothing() {
        assert!(fuzzy_match("zzzzqqqq", ALL_ACTIONS).is_empty());
    }

    #[test]
    fn matching_is_case_insensitive() {
        let lower = fuzzy_match("quit", ALL_ACTIONS);
        let upper = fuzzy_match("QUIT", ALL_ACTIONS);
        assert_eq!(lower, upper);
        assert_eq!(lower.first(), Some(&Action::Quit));
    }

    /// The scoring point this task exists to prove: a tighter, front-loaded
    /// match for the exact action outranks the same letters scattered across
    /// a longer, unrelated one.
    #[test]
    fn a_tighter_match_ranks_first() {
        let matches = fuzzy_match("readonly", ALL_ACTIONS);
        assert_eq!(matches.first(), Some(&Action::ToggleReadOnly));
    }

    #[test]
    fn ties_keep_all_actions_order() {
        // `Copy` and `CopyCommand` both start "copy …", so `"co"` scores them
        // identically — the tie-break is `ALL_ACTIONS`'s own order (`Copy`
        // before `CopyCommand`), proving the sort is stable rather than
        // incidental.
        let matches = fuzzy_match("co", ALL_ACTIONS);
        let copy_pos = matches.iter().position(|a| *a == Action::Copy);
        let copy_command_pos = matches.iter().position(|a| *a == Action::CopyCommand);
        assert!(copy_pos.is_some() && copy_command_pos.is_some());
        assert!(
            copy_pos < copy_command_pos,
            "ALL_ACTIONS order should break the tie: {matches:?}"
        );
    }

    #[test]
    fn new_starts_open_with_every_action_and_the_top_row_selected() {
        let palette = PaletteState::new();
        assert_eq!(palette.matches, ALL_ACTIONS.to_vec());
        assert_eq!(palette.selected, 0);
        assert_eq!(palette.highlighted(), ALL_ACTIONS.first().copied());
    }

    #[test]
    fn refresh_reruns_the_filter_and_resets_the_selection() {
        let mut palette = PaletteState::new();
        palette.selected = 3;
        palette.query = "quit".into();
        palette.refresh();
        assert_eq!(palette.matches, vec![Action::Quit]);
        assert_eq!(palette.selected, 0);
    }

    #[test]
    fn move_selection_clamps_at_both_ends() {
        let mut palette = PaletteState::new();
        palette.move_selection(-5);
        assert_eq!(palette.selected, 0, "no wraparound past the top");

        let last = palette.matches.len() - 1;
        palette.move_selection(1_000);
        assert_eq!(palette.selected, last, "no wraparound past the bottom");
    }

    #[test]
    fn move_selection_on_an_empty_match_list_stays_at_zero() {
        let mut palette = PaletteState::new();
        palette.query = "zzzzqqqq".into();
        palette.refresh();
        assert!(palette.matches.is_empty());
        palette.move_selection(1);
        assert_eq!(palette.selected, 0);
    }
}
