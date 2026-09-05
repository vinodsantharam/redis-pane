//! Building what `y` puts on the clipboard (R3.5, PLAN M1.12).
//!
//! Pure: these functions turn state into text. Getting that text to a clipboard
//! is the shell's problem, and over SSH it is a more interesting one than it
//! looks — see `crates/app/src/clipboard.rs`.

use super::value::Value;

/// What `y` was asked to copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyWhat {
    /// The key's name.
    Key,
    /// The value, as the Viewer renders it.
    Value,
    /// A `redis-cli` invocation that reads this key from this server.
    Command,
}

impl CopyWhat {
    /// The word shown in the confirmation notice.
    pub fn label(&self) -> &'static str {
        match self {
            CopyWhat::Key => "key",
            CopyWhat::Value => "value",
            CopyWhat::Command => "redis-cli command",
        }
    }
}

/// The value as text: one row per line, cells separated by tabs.
///
/// Tabs rather than aligned columns, because this is going into a terminal or
/// an editor, not back into this pane. The whole value is included even where
/// the Viewer is scrolled — a partial copy is a trap.
///
/// `now_ms` fixes what a Stream's AGE column reads at the moment of copying —
/// callers should pass the time the value was actually read
/// ([`super::open::OpenKey::read_at_ms`]), not an ambient clock, since a copy is a snapshot
/// of what was read, not a live view (and `update()`, where every copy is
/// built, is pure and has no clock of its own — ADR-0011).
pub fn value_text(value: &Value, now_ms: u64) -> String {
    let viewer = value.viewer();
    (0..viewer.row_count())
        .map(|i| viewer.row(i, now_ms).join("\t"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A paste-ready `redis-cli` invocation that reads this key from this server.
///
/// The command matches the key's type, because `GET` on a hash is an error and
/// a copied command that does not work is worse than no command at all. Takes
/// the value directly rather than an `OpenKey` and reaching for `.value`
/// itself: there is no type to match when a key was confirmed gone before
/// ever loading, and a function that cannot be called for that case is safer
/// than one that has to be taught not to unwrap a `None`.
pub fn redis_cli_command(target: &str, name: &str, value: &Value) -> String {
    let connection = if target.contains("://") {
        format!("-u {}", shell_quote(target))
    } else {
        // `host:port/db` — the shape the title bar shows.
        let (address, db) = target.split_once('/').unwrap_or((target, "0"));
        let (host, port) = address.rsplit_once(':').unwrap_or((address, "6379"));
        format!("-h {host} -p {port} -n {db}")
    };
    let key = shell_quote(name);
    let verb = match value {
        Value::Str(_) | Value::Json(_) | Value::Binary(_) => format!("GET {key}"),
        Value::Hash(_) => format!("HGETALL {key}"),
        Value::List(_) => format!("LRANGE {key} 0 -1"),
        Value::Set(_) => format!("SMEMBERS {key}"),
        Value::ZSet(_) => format!("ZRANGE {key} 0 -1 WITHSCORES"),
        Value::Stream(_) => format!("XRANGE {key} - +"),
    };
    format!("redis-cli {connection} {verb}")
}

/// Single-quote for a POSIX shell.
///
/// Redis keys are arbitrary bytes and routinely contain `:` and `*`; some
/// contain spaces or quotes. A command that needs hand-editing before it runs
/// is not "ready to paste".
fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:@+".contains(c))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::value::{PairValue, ScoredValue, StringValue};

    fn hash() -> Value {
        Value::Hash(PairValue {
            pairs: vec![("id".into(), "8812".into()), ("plan".into(), "pro".into())],
            total: 2,
        })
    }

    #[test]
    fn the_value_copies_as_tab_separated_rows() {
        assert_eq!(value_text(&hash(), 0), "id\t8812\nplan\tpro");
    }

    #[test]
    fn a_string_copies_as_its_text() {
        let v = Value::Str(StringValue::new("hello", 40));
        assert_eq!(value_text(&v, 0), "hello");
    }

    #[test]
    fn the_whole_value_is_copied_even_when_the_viewer_is_scrolled() {
        // A partial copy is a trap: it looks complete in the paste buffer.
        let v = Value::ZSet(ScoredValue {
            entries: (0..100).map(|i| (format!("m{i}"), i as f64)).collect(),
            total: 100,
        });
        assert_eq!(value_text(&v, 0).lines().count(), 100);
    }

    #[test]
    fn the_command_matches_the_type_so_it_actually_runs() {
        let target = "cache-01:6379/0";
        assert!(
            redis_cli_command(target, "k", &hash()).ends_with("HGETALL k"),
            "GET on a hash is an error"
        );
        assert!(
            redis_cli_command(target, "k", &Value::Str(StringValue::new("v", 8)))
                .ends_with("GET k")
        );
        assert!(
            redis_cli_command(target, "k", &Value::ZSet(ScoredValue::default()))
                .ends_with("ZRANGE k 0 -1 WITHSCORES")
        );
    }

    #[test]
    fn the_command_carries_the_host_port_and_database() {
        let cmd = redis_cli_command("cache-01:6380/3", "k", &hash());
        assert_eq!(cmd, "redis-cli -h cache-01 -p 6380 -n 3 HGETALL k");
    }

    #[test]
    fn a_url_target_is_passed_through_as_a_url() {
        let cmd = redis_cli_command("redis://cache-01.eu-w1:6379", "k", &hash());
        assert!(
            cmd.starts_with("redis-cli -u redis://cache-01.eu-w1:6379"),
            "{cmd}"
        );
    }

    #[test]
    fn keys_that_need_quoting_get_it() {
        let cmd = redis_cli_command("h:6379/0", "key with space", &hash());
        assert!(cmd.ends_with("HGETALL 'key with space'"), "{cmd}");

        let cmd = redis_cli_command("h:6379/0", "it's", &hash());
        assert!(cmd.ends_with(r"HGETALL 'it'\''s'"), "{cmd}");
    }

    #[test]
    fn ordinary_keys_are_left_unquoted_because_quoting_them_is_noise() {
        let cmd = redis_cli_command("h:6379/0", "user:8812:session", &hash());
        assert!(cmd.ends_with("HGETALL user:8812:session"), "{cmd}");
    }

    #[test]
    fn a_glob_in_a_key_name_is_quoted_so_the_shell_does_not_eat_it() {
        let cmd = redis_cli_command("h:6379/0", "cache:*:tmp", &hash());
        assert!(cmd.ends_with("HGETALL 'cache:*:tmp'"), "{cmd}");
    }
}
