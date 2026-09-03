//! Getting text onto the clipboard, including over SSH (R3.5, PLAN M1.12).
//!
//! This uses the **OSC 52** terminal escape sequence rather than a native
//! clipboard API, and the reason is the product's whole premise: `redis-pane`
//! is meant to be used inside an SSH session on a bastion host. A native
//! clipboard call there would copy to the clipboard of the *remote* machine,
//! which is nobody's clipboard. OSC 52 hands the text to the terminal emulator,
//! so it lands on the clipboard of the laptop the human is actually sitting at.
//!
//! It also needs no dependency and no X11 or Wayland connection.
//!
//! The cost is that support is not universal: it works in iTerm2, kitty,
//! WezTerm, Alacritty, foot, Windows Terminal and tmux (with `set -g
//! set-clipboard on`), and is off by default in some builds of others. There is
//! no reply to read, so a terminal that ignores the sequence does so silently —
//! which is why the notice says "copied" rather than "copied to clipboard": we
//! know what we sent, not what was received.

use std::io::Write;

/// Terminals commonly cap OSC 52 payloads; oversized ones are dropped whole.
/// Truncating loudly beats sending something that silently vanishes.
const MAX_BYTES: usize = 74_000;

/// Base64, written out rather than pulled in: it is twenty lines and this is
/// the only place the workspace needs it.
fn base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// The escape sequence that carries `text` to the terminal's clipboard.
pub fn osc52(text: &str) -> String {
    let bytes = text.as_bytes();
    let payload = if bytes.len() > MAX_BYTES {
        &bytes[..MAX_BYTES]
    } else {
        bytes
    };
    format!("\x1b]52;c;{}\x07", base64(payload))
}

/// Whether the text had to be shortened to fit.
pub fn was_truncated(text: &str) -> bool {
    text.len() > MAX_BYTES
}

/// Write the sequence to the terminal.
pub fn copy(text: &str) -> std::io::Result<()> {
    let mut out = std::io::stdout();
    out.write_all(osc52(text).as_bytes())?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_rfc_examples() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_handles_bytes_that_are_not_text() {
        assert_eq!(base64(&[0xff, 0xfe, 0xfd]), "//79");
    }

    #[test]
    fn the_sequence_is_shaped_the_way_terminals_expect() {
        let s = osc52("hi");
        assert!(s.starts_with("\x1b]52;c;"), "{s:?}");
        assert!(s.ends_with('\x07'), "{s:?}");
        assert!(s.contains("aGk="), "{s:?}");
    }

    #[test]
    fn an_oversized_payload_is_truncated_rather_than_dropped_whole() {
        // Terminals discard sequences past their limit, so sending the lot
        // would copy nothing at all — and silently.
        let huge = "x".repeat(MAX_BYTES * 2);
        assert!(was_truncated(&huge));
        let s = osc52(&huge);
        assert!(s.len() < MAX_BYTES * 2, "still sent the whole thing");
        assert!(s.ends_with('\x07'));
    }

    #[test]
    fn an_ordinary_payload_is_not_truncated() {
        assert!(!was_truncated("user:8812:session"));
    }
}
