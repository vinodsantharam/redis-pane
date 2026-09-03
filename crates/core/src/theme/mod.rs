//! Semantic colour tokens and capability degradation (DESIGN §5).
//!
//! Widgets ask for `border-focus` or `type.hash`, never a hex value. Themes
//! remap tokens; truecolor degrades to 256 and then to monochrome.
