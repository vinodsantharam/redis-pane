//! Getting text onto the clipboard, including over SSH (R3.5, PLAN M1.12).
//!
//! Two delivery methods, chosen by [`detect_method`] (ADR-0013):
//!
//! - **OSC 52**, over a detected SSH session. `redis-pane` is meant to be used
//!   inside an SSH session on a bastion host, where a native clipboard call
//!   would copy to the clipboard of the *remote* machine — nobody's clipboard.
//!   OSC 52 hands the text to the terminal emulator instead, so it lands on
//!   the clipboard of the laptop the human is actually sitting at. It needs no
//!   dependency and no X11 or Wayland connection, but support is not
//!   universal: it works in iTerm2, kitty, WezTerm, Alacritty, foot, Windows
//!   Terminal and tmux (with `set -g set-clipboard on`), and is off by default
//!   — or entirely unimplemented — in some others. There is no reply to read,
//!   so a terminal that ignores the sequence does so silently.
//! - **Native (`pbcopy`)**, everywhere else, on macOS. Locally there is no SSH
//!   hop to get the target wrong, and OSC 52's inconsistent local support is
//!   exactly the failure mode this avoids: silently doing nothing.
//!
//! Outside SSH and macOS there is no native path implemented yet, so OSC 52 is
//! still what runs — the same "may silently do nothing" caveat applies there.

use std::io::Write;
use std::process::{Command, Stdio};

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

/// Whether the text had to be shortened to fit. Only meaningful for
/// [`Method::Osc52`] — [`Method::Native`] has no comparable cap.
pub fn was_truncated(text: &str) -> bool {
    text.len() > MAX_BYTES
}

/// Which way [`copy`] should reach the clipboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// The OSC 52 escape sequence, written to our own stdout.
    Osc52,
    /// A native clipboard call (`pbcopy`) that never touches our stdout.
    Native,
}

/// Decides [`Method`] for the process we're actually running in (ADR-0013).
pub fn detect_method() -> Method {
    resolve_method(
        std::env::var("SSH_TTY").ok(),
        std::env::var("SSH_CONNECTION").ok(),
        std::env::var("SSH_CLIENT").ok(),
    )
}

/// The pure decision behind [`detect_method`], split out the same way
/// `terminal::resolve_color_depth` is: reading `std::env::var` needs `unsafe`
/// to fake in a test (this workspace forbids `unsafe`), so the read and the
/// decision are two functions and only the read touches the environment.
///
/// Any of the three SSH variables present means an SSH client set them for
/// this session — the standard signal every `ssh`-aware tool (including
/// Neovim's own OSC 52 provider) keys off. Native only outranks OSC 52
/// locally: over SSH, `pbcopy` would write to the *server's* clipboard, which
/// is nobody's, so OSC 52 stays the only correct answer there regardless of
/// OS.
fn resolve_method(
    ssh_tty: Option<String>,
    ssh_connection: Option<String>,
    ssh_client: Option<String>,
) -> Method {
    let over_ssh = ssh_tty.is_some() || ssh_connection.is_some() || ssh_client.is_some();
    if over_ssh {
        Method::Osc52
    } else if cfg!(target_os = "macos") {
        Method::Native
    } else {
        Method::Osc52
    }
}

/// Get `text` onto the clipboard the way `method` says to.
pub fn copy(text: &str, method: Method) -> std::io::Result<()> {
    match method {
        Method::Osc52 => copy_osc52(text),
        Method::Native => copy_native(text),
    }
}

/// Write the OSC 52 sequence to the terminal.
fn copy_osc52(text: &str) -> std::io::Result<()> {
    let mut out = std::io::stdout();
    out.write_all(osc52(text).as_bytes())?;
    out.flush()
}

/// Hand the whole payload to `pbcopy` over its stdin. Never touches our own
/// stdout — nothing here needs the terminal redrawn afterward.
fn copy_native(text: &str) -> std::io::Result<()> {
    let mut child = Command::new("pbcopy").stdin(Stdio::piped()).spawn()?;
    child
        .stdin
        .take()
        .expect("just configured with a piped stdin")
        .write_all(text.as_bytes())?;
    let status = child.wait()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "pbcopy exited with {status}"
        )));
    }
    Ok(())
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

    #[test]
    fn any_ssh_variable_forces_osc52_regardless_of_os() {
        // pbcopy over SSH would write to the *server's* clipboard, which is
        // nobody's — OSC 52 is the only correct answer there (ADR-0013).
        assert_eq!(
            resolve_method(Some("/dev/ttys003".into()), None, None),
            Method::Osc52
        );
        assert_eq!(
            resolve_method(None, Some("10.0.0.1 51000 10.0.0.2 22".into()), None),
            Method::Osc52
        );
        assert_eq!(
            resolve_method(None, None, Some("10.0.0.1 51000 22".into())),
            Method::Osc52
        );
    }

    #[test]
    fn no_ssh_variables_prefers_native_on_macos() {
        let method = resolve_method(None, None, None);
        if cfg!(target_os = "macos") {
            assert_eq!(method, Method::Native);
        } else {
            assert_eq!(method, Method::Osc52, "no native path outside macOS yet");
        }
    }
}
