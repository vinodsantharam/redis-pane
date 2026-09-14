//! A Redis key, exactly as the server knows it.
//!
//! Keys are arbitrary bytes. Text is for the screen: [`KeyName::display`] is
//! the one lossy step, and nothing turns displayed text back into a key.
//!
//! This type exists because the Open key's name used to be a `String` built
//! with `from_utf8_lossy`, while every `Command` carried the key as bytes. The
//! shell rebuilt a Refetch's bytes from that string, so a key that was not
//! valid UTF-8 opened correctly once and then refetched a *different* key: the
//! Viewer badged a live key `✕ deleted`, and tracking was armed on the wrong
//! key. One type on both sides of the seam makes that unwritable
//! (docs/reviews/2026-09-13-codebase-design-review.md, C1).

use std::borrow::Cow;
use std::fmt;

/// The exact bytes of a key. Compared, hashed and ordered by those bytes.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct KeyName(Vec<u8>);

impl KeyName {
    /// The bytes the server knows this key by. What every command sends.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.0
    }

    /// The key as text, for the screen and for messages a person reads.
    ///
    /// Lossy for a key that is not valid UTF-8, which is exactly why this
    /// must never be turned back into a key.
    pub fn display(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.0)
    }

    /// The key as text, only when that text is exact.
    pub fn as_str(&self) -> Option<&str> {
        std::str::from_utf8(&self.0).ok()
    }
}

/// Lossy, like [`KeyName::display`]: for UI strings and error messages only.
impl fmt::Display for KeyName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display())
    }
}

/// Escaped rather than lossy, so a failing test shows which bytes differed.
impl fmt::Debug for KeyName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyName(\"{}\")", self.0.escape_ascii())
    }
}

impl From<Vec<u8>> for KeyName {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

impl From<&[u8]> for KeyName {
    fn from(bytes: &[u8]) -> Self {
        Self(bytes.to_vec())
    }
}

impl From<String> for KeyName {
    fn from(text: String) -> Self {
        Self(text.into_bytes())
    }
}

impl From<&str> for KeyName {
    fn from(text: &str) -> Self {
        Self(text.as_bytes().to_vec())
    }
}

impl PartialEq<str> for KeyName {
    fn eq(&self, other: &str) -> bool {
        self.0 == other.as_bytes()
    }
}

impl PartialEq<&str> for KeyName {
    fn eq(&self, other: &&str) -> bool {
        self.0 == other.as_bytes()
    }
}

impl<const N: usize> PartialEq<[u8; N]> for KeyName {
    fn eq(&self, other: &[u8; N]) -> bool {
        self.0 == other
    }
}

impl<const N: usize> PartialEq<&[u8; N]> for KeyName {
    fn eq(&self, other: &&[u8; N]) -> bool {
        self.0 == *other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOT_UTF8: &[u8] = b"\xff\xfe:session";

    #[test]
    fn bytes_that_are_not_utf8_survive_untouched() {
        let key = KeyName::from(NOT_UTF8);
        assert_eq!(key.as_bytes(), NOT_UTF8);
        assert_eq!(key.clone().into_bytes(), NOT_UTF8);
        assert_eq!(key.as_str(), None, "no exact text exists for these bytes");
    }

    #[test]
    fn display_is_lossy_and_therefore_not_the_key() {
        let key = KeyName::from(NOT_UTF8);
        assert_eq!(key.display(), "\u{FFFD}\u{FFFD}:session");
        assert_ne!(
            KeyName::from(key.display().into_owned()),
            key,
            "the displayed text names a different key"
        );
    }

    #[test]
    fn text_keys_compare_with_str_for_convenience() {
        let key = KeyName::from("user:1");
        assert_eq!(key, "user:1");
        assert_eq!(key.as_str(), Some("user:1"));
        assert_eq!(format!("{key}"), "user:1");
    }

    #[test]
    fn debug_escapes_rather_than_replacing() {
        assert_eq!(
            format!("{:?}", KeyName::from(NOT_UTF8)),
            r#"KeyName("\xff\xfe:session")"#
        );
    }
}
