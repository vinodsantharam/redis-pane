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
    EnvLocal,
    EnvStaging,
    EnvProd,
    EnvUnknown,
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
    /// In monochrome, hue is gone entirely and the only tools left are bold and
    /// dim — which is why every token that carries meaning is also spelled out
    /// in words somewhere on screen.
    pub fn style(&self, token: Token) -> Style {
        match self.depth {
            ColorDepth::TrueColor => Style::default().fg(self.true_color(token)),
            ColorDepth::Ansi256 => Style::default().fg(self.ansi_256(token)),
            ColorDepth::Monochrome => self.monochrome(token),
        }
    }

    fn true_color(&self, token: Token) -> Color {
        match token {
            Token::Text => Color::Rgb(0xE6, 0xE6, 0xE6),
            Token::Muted => Color::Rgb(0x8A, 0x8A, 0x8A),
            Token::Border => Color::Rgb(0x44, 0x44, 0x44),
            Token::BorderFocus => Color::Rgb(0x7A, 0xA2, 0xF7),
            Token::EnvLocal => Color::Rgb(0x7A, 0xC7, 0x8E),
            Token::EnvStaging => Color::Rgb(0xE0, 0xAF, 0x68),
            Token::EnvProd => Color::Rgb(0xF7, 0x76, 0x8E),
            Token::EnvUnknown => Color::Rgb(0x9A, 0x9A, 0x9A),
            Token::Ok => Color::Rgb(0x7A, 0xC7, 0x8E),
            Token::Warn => Color::Rgb(0xE0, 0xAF, 0x68),
            Token::Danger => Color::Rgb(0xF7, 0x76, 0x8E),
        }
    }

    fn ansi_256(&self, token: Token) -> Color {
        match token {
            Token::Text => Color::Indexed(253),
            Token::Muted => Color::Indexed(245),
            Token::Border => Color::Indexed(238),
            Token::BorderFocus => Color::Indexed(111),
            Token::EnvLocal => Color::Indexed(114),
            Token::EnvStaging => Color::Indexed(179),
            Token::EnvProd => Color::Indexed(210),
            Token::EnvUnknown => Color::Indexed(246),
            Token::Ok => Color::Indexed(114),
            Token::Warn => Color::Indexed(179),
            Token::Danger => Color::Indexed(210),
        }
    }

    fn monochrome(&self, token: Token) -> Style {
        let s = Style::default();
        match token {
            Token::Muted | Token::Border | Token::EnvUnknown => s.add_modifier(Modifier::DIM),
            Token::BorderFocus | Token::EnvProd | Token::Danger => s.add_modifier(Modifier::BOLD),
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
