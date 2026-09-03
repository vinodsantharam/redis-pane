//! Semantic colour tokens and capability degradation (DESIGN §5, PLAN M0.3).
//!
//! Widgets ask for [`Token::BorderFocus`] or [`Token::TypeHash`], never a hex
//! value. Themes remap tokens; the terminal's capability decides how far a
//! token can be honoured. Truecolor degrades to 256 and then to monochrome.
//!
//! The rule that makes monochrome usable: **colour never carries meaning
//! alone**. An Environment is a coloured dot *and* the word `prod`; a type is a
//! coloured cell *and* the word `hash`. Losing colour must lose emphasis, never
//! information.

use ratatui::style::{Color, Modifier, Style};

/// What the terminal can actually display. Detected by the shell, injected like
/// the clock so that golden frames can pin it (ADR-0011).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorDepth {
    TrueColor,
    Ansi256,
    Monochrome,
}

/// A semantic colour token. Never a literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Token {
    /// Ordinary foreground text.
    Text,
    /// De-emphasised text: units, labels, secondary facts.
    Muted,
    /// Pane borders and separators.
    Border,
    /// The border of the focused pane.
    BorderFocus,
    /// The current selection: a full-row highlight, not a foreground tint.
    /// Used for exactly one row at a time, in the keys pane.
    Selected,
    EnvLocal,
    EnvStaging,
    EnvProd,
    EnvUnknown,
    /// One hue per Redis type — DESIGN §5's `type.*`, consistent everywhere a
    /// type appears: the keys pane's marker and TYPE column, and the value
    /// pane's header. `Other`/binary deliberately has no token of its own; an
    /// unclassified type is neutral, not a ninth colour to keep track of.
    TypeString,
    TypeHash,
    TypeList,
    TypeSet,
    TypeZSet,
    TypeStream,
    TypeJson,
    /// Liveness is healthy.
    Ok,
    /// Something needs attention but nothing is broken.
    Warn,
    /// Destructive, or refused.
    Danger,
}

/// Resolves tokens to styles at a given colour depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub depth: ColorDepth,
}

impl Theme {
    pub fn new(depth: ColorDepth) -> Self {
        Self { depth }
    }

    /// The style for a token, degraded to what the terminal can show.
    ///
    /// In monochrome, hue is gone entirely and the only tools left are bold,
    /// dim and reverse video — which is why every token that carries meaning
    /// is also spelled out in words, or in the case of the selection, in
    /// position, somewhere on screen.
    pub fn style(&self, token: Token) -> Style {
        match self.depth {
            ColorDepth::TrueColor => self.true_color(token),
            ColorDepth::Ansi256 => self.ansi_256(token),
            ColorDepth::Monochrome => self.monochrome(token),
        }
    }

    fn true_color(&self, token: Token) -> Style {
        let fg = |c| Style::default().fg(c);
        match token {
            Token::Text => fg(Color::Rgb(0xE6, 0xE6, 0xE6)),
            Token::Muted => fg(Color::Rgb(0x8A, 0x8A, 0x8A)),
            Token::Border => fg(Color::Rgb(0x44, 0x44, 0x44)),
            Token::BorderFocus => fg(Color::Rgb(0x7A, 0xA2, 0xF7)),
            // A full bar, not a tint: a fixed dark-on-amber pair that reads
            // clearly regardless of which type or metadata colour it covers.
            Token::Selected => Style::default()
                .fg(Color::Rgb(0x11, 0x13, 0x1A))
                .bg(Color::Rgb(0xF5, 0xA7, 0x42)),
            Token::EnvLocal => fg(Color::Rgb(0x7A, 0xC7, 0x8E)),
            Token::EnvStaging => fg(Color::Rgb(0xE0, 0xAF, 0x68)),
            Token::EnvProd => fg(Color::Rgb(0xF7, 0x76, 0x8E)),
            Token::EnvUnknown => fg(Color::Rgb(0x9A, 0x9A, 0x9A)),
            Token::TypeString => fg(Color::Rgb(0x7D, 0xCF, 0xFF)),
            Token::TypeHash => fg(Color::Rgb(0xE0, 0xAF, 0x68)),
            Token::TypeList => fg(Color::Rgb(0x9E, 0xCE, 0x6A)),
            Token::TypeSet => fg(Color::Rgb(0xBB, 0x9A, 0xF7)),
            Token::TypeZSet => fg(Color::Rgb(0xFF, 0x9E, 0x64)),
            Token::TypeStream => fg(Color::Rgb(0xF7, 0x76, 0x8E)),
            Token::TypeJson => fg(Color::Rgb(0x73, 0xDA, 0xCA)),
            Token::Ok => fg(Color::Rgb(0x7A, 0xC7, 0x8E)),
            Token::Warn => fg(Color::Rgb(0xE0, 0xAF, 0x68)),
            Token::Danger => fg(Color::Rgb(0xF7, 0x76, 0x8E)),
        }
    }

    fn ansi_256(&self, token: Token) -> Style {
        let fg = |i| Style::default().fg(Color::Indexed(i));
        match token {
            Token::Text => fg(253),
            Token::Muted => fg(245),
            Token::Border => fg(238),
            Token::BorderFocus => fg(111),
            Token::Selected => Style::default()
                .fg(Color::Indexed(0))
                .bg(Color::Indexed(208)),
            Token::EnvLocal => fg(114),
            Token::EnvStaging => fg(179),
            Token::EnvProd => fg(210),
            Token::EnvUnknown => fg(246),
            Token::TypeString => fg(117),
            Token::TypeHash => fg(179),
            Token::TypeList => fg(150),
            Token::TypeSet => fg(141),
            Token::TypeZSet => fg(215),
            Token::TypeStream => fg(210),
            Token::TypeJson => fg(116),
            Token::Ok => fg(114),
            Token::Warn => fg(179),
            Token::Danger => fg(210),
        }
    }

    fn monochrome(&self, token: Token) -> Style {
        let s = Style::default();
        match token {
            // Reverse video is the one signal that survives with no hue at
            // all — the standard way a terminal marks "this is the row you
            // are on" when it cannot colour it.
            Token::Selected => s.add_modifier(Modifier::REVERSED),
            Token::Muted | Token::Border | Token::EnvUnknown => s.add_modifier(Modifier::DIM),
            Token::BorderFocus | Token::EnvProd | Token::Danger => s.add_modifier(Modifier::BOLD),
            // Every type token: no modifier. The TYPE column's word is what
            // carries the information here, per this module's own rule.
            _ => s,
        }
    }
}

/// The token that stands for an Environment.
pub fn env_token(env: crate::state::Environment) -> Token {
    use crate::state::Environment as E;
    match env {
        E::Local => Token::EnvLocal,
        E::Staging => Token::EnvStaging,
        E::Prod => Token::EnvProd,
        E::Unknown => Token::EnvUnknown,
    }
}

/// The token for a Redis type, wherever one is shown (DESIGN §5's `type.*`).
/// `None` — metadata not yet fetched — is [`Token::Muted`], matching every
/// other pending cell.
pub fn type_token(kind: Option<crate::state::KeyKind>) -> Token {
    use crate::state::KeyKind as K;
    match kind {
        None => Token::Muted,
        Some(K::String) => Token::TypeString,
        Some(K::Hash) => Token::TypeHash,
        Some(K::List) => Token::TypeList,
        Some(K::Set) => Token::TypeSet,
        Some(K::ZSet) => Token::TypeZSet,
        Some(K::Stream) => Token::TypeStream,
        Some(K::Json) => Token::TypeJson,
        // Unclassified is neutral, not a ninth colour to keep track of.
        Some(K::Other) => Token::Muted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::KeyKind;

    #[test]
    fn truecolor_and_256_differ_but_both_carry_hue() {
        let t = Theme::new(ColorDepth::TrueColor).style(Token::EnvProd);
        let a = Theme::new(ColorDepth::Ansi256).style(Token::EnvProd);
        assert_ne!(t, a);
        assert!(matches!(t.fg, Some(Color::Rgb(..))));
        assert!(matches!(a.fg, Some(Color::Indexed(_))));
    }

    #[test]
    fn monochrome_sets_no_foreground_colour_at_all() {
        for token in [Token::EnvProd, Token::EnvLocal, Token::Ok, Token::Danger] {
            assert_eq!(Theme::new(ColorDepth::Monochrome).style(token).fg, None);
        }
    }

    #[test]
    fn prod_stays_emphasised_when_hue_is_gone() {
        let s = Theme::new(ColorDepth::Monochrome).style(Token::EnvProd);
        assert!(s.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn every_type_gets_a_distinct_hue_in_truecolor() {
        let theme = Theme::new(ColorDepth::TrueColor);
        let kinds = [
            KeyKind::String,
            KeyKind::Hash,
            KeyKind::List,
            KeyKind::Set,
            KeyKind::ZSet,
            KeyKind::Stream,
            KeyKind::Json,
        ];
        let colors: Vec<_> = kinds
            .iter()
            .map(|k| theme.style(type_token(Some(*k))).fg)
            .collect();
        for i in 0..colors.len() {
            for j in (i + 1)..colors.len() {
                assert_ne!(
                    colors[i], colors[j],
                    "{:?} and {:?} share a hue",
                    kinds[i], kinds[j]
                );
            }
        }
    }

    #[test]
    fn a_type_not_yet_fetched_is_muted_like_every_other_pending_cell() {
        assert_eq!(type_token(None), Token::Muted);
    }

    #[test]
    fn an_unclassified_type_is_neutral_not_a_ninth_colour() {
        assert_eq!(type_token(Some(KeyKind::Other)), Token::Muted);
    }

    #[test]
    fn selection_is_a_full_bar_with_both_a_background_and_a_foreground() {
        for depth in [ColorDepth::TrueColor, ColorDepth::Ansi256] {
            let s = Theme::new(depth).style(Token::Selected);
            assert!(s.bg.is_some(), "{depth:?} selection has no background");
            assert!(s.fg.is_some(), "{depth:?} selection has no foreground");
        }
    }

    #[test]
    fn selection_in_monochrome_is_reverse_video_with_no_colour_at_all() {
        let s = Theme::new(ColorDepth::Monochrome).style(Token::Selected);
        assert!(s.add_modifier.contains(Modifier::REVERSED));
        assert!(s.fg.is_none());
        assert!(s.bg.is_none());
    }
}
