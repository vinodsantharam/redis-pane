//! A confirmed write, and what the server made of it (R4.4, review H1).
//!
//! [`crate::state::PendingMutation`] is the staged form: the write plus what
//! the confirm dialog shows about it. Confirming turns it into a [`Mutation`],
//! the one shape a write has between the core and the server. The shell
//! executes it and reports a [`MutationOutcome`] back in
//! [`crate::Msg::MutationSettled`], carrying the same `Mutation`, so the core
//! decides what every outcome means in one place.
//!
//! This replaced a `Command` variant, a shell arm and one or more `Msg`
//! variants per write. Adding a mutation now touches this enum, the staged
//! form that previews it, and the shell's one `execute`.

use crate::key::KeyName;

/// A write, exactly as the shell executes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mutation {
    /// `DEL key` (R4.3).
    DeleteKey { key: KeyName },
    /// `SET key value KEEPTTL XX`: overwrite a String, keeping its TTL and
    /// never recreating a key that is gone (ADR-0014).
    SetString { key: KeyName, value: Vec<u8> },
    /// Guarded `HSET key field value`, keeping the field's own TTL
    /// (ADR-0015).
    SetHashField {
        key: KeyName,
        field: Vec<u8>,
        value: Vec<u8>,
    },
    /// Guarded `HSETNX key field value`, never recreating the key
    /// (ADR-0015).
    AddHashField {
        key: KeyName,
        field: Vec<u8>,
        value: Vec<u8>,
    },
    /// `HDEL key field`. Removing the last field removes the key: Redis's own
    /// behaviour, not something this arranges.
    DeleteHashField { key: KeyName, field: Vec<u8> },
    /// Guarded `SADD key member`, never recreating the key (ADR-0016 D2). A
    /// Set member has no name/value split the way a Hash field does — it is
    /// only bytes — so unlike `AddHashField` there is nothing here but the
    /// member itself (PLAN M2 task 7, D3).
    AddSetMember { key: KeyName, member: Vec<u8> },
    /// `SREM key member`. Removing the last member removes the key: Redis's
    /// own behaviour, not something this arranges (ADR-0016).
    DeleteSetMember { key: KeyName, member: Vec<u8> },
}

impl Mutation {
    /// The key this writes to.
    pub fn key(&self) -> &KeyName {
        match self {
            Mutation::DeleteKey { key }
            | Mutation::SetString { key, .. }
            | Mutation::SetHashField { key, .. }
            | Mutation::AddHashField { key, .. }
            | Mutation::DeleteHashField { key, .. }
            | Mutation::AddSetMember { key, .. }
            | Mutation::DeleteSetMember { key, .. } => key,
        }
    }

    /// The command as a person names it, e.g. `HSET user:1 token`, for an
    /// error or a refusal (R7.4).
    ///
    /// Never the value: an error line is no place for a 200KB payload. The
    /// confirm dialog's fuller preview is
    /// [`crate::state::PendingMutation::command_text`].
    pub fn command_label(&self) -> String {
        let field = |f: &[u8]| String::from_utf8_lossy(f).into_owned();
        match self {
            Mutation::DeleteKey { key } => format!("DEL {key}"),
            Mutation::SetString { key, .. } => format!("SET {key}"),
            Mutation::SetHashField { key, field: f, .. } => format!("HSET {key} {}", field(f)),
            Mutation::AddHashField { key, field: f, .. } => format!("HSETNX {key} {}", field(f)),
            Mutation::DeleteHashField { key, field: f } => format!("HDEL {key} {}", field(f)),
            // No member in the label, unlike a Hash field's name: a member is
            // only a value (ADR-0016 D3), and an error line is no place for
            // one any more than it is for a String's value.
            Mutation::AddSetMember { key, .. } => format!("SADD {key}"),
            Mutation::DeleteSetMember { key, .. } => format!("SREM {key}"),
        }
    }
}

/// Why a guarded write did not write anything (PLAN M2 task 6, D1, ADR-0015).
///
/// One enum for every guard a write can trip, whichever mutation it was, so
/// the core's handler is one match on *why*, not one per command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotWritten {
    /// The key itself was already gone. Tombstoned, the same as a
    /// `ValueGone`; the key is never recreated (ADR-0014).
    KeyGone,
    /// A `SetHashField` found the field already gone.
    FieldGone,
    /// An `AddHashField` found the field already there.
    FieldExists,
    /// An `AddSetMember` found the member already there (ADR-0016 D2). Not
    /// [`NotWritten::FieldExists`] — a Set has no fields, and CLAUDE.md's
    /// glossary treats a member and a field as different things, so the
    /// error type keeps the distinction rather than blurring it (PLAN M2
    /// task 7).
    MemberExists,
}

/// What the server made of a [`Mutation`].
///
/// None of these is an error: the server did exactly as asked. An error is the
/// `Err` beside this in [`crate::Msg::MutationSettled`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationOutcome {
    /// It did what it says. For `DEL` that includes a key already gone: the
    /// key is gone either way, which is the only fact the Viewer badges.
    Done,
    /// A guard refused, and nothing was written.
    NotWritten(NotWritten),
    /// `HDEL` found no such field: nothing to remove, and nothing broken.
    NothingToRemove,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_label_names_the_command_and_its_target_but_never_the_value() {
        let key = KeyName::from("user:1");
        let cases = [
            (Mutation::DeleteKey { key: key.clone() }, "DEL user:1"),
            (
                Mutation::SetString {
                    key: key.clone(),
                    value: b"a very long value".to_vec(),
                },
                "SET user:1",
            ),
            (
                Mutation::SetHashField {
                    key: key.clone(),
                    field: b"token".to_vec(),
                    value: b"secret".to_vec(),
                },
                "HSET user:1 token",
            ),
            (
                Mutation::AddHashField {
                    key: key.clone(),
                    field: b"token".to_vec(),
                    value: b"secret".to_vec(),
                },
                "HSETNX user:1 token",
            ),
            (
                Mutation::DeleteHashField {
                    key: key.clone(),
                    field: b"token".to_vec(),
                },
                "HDEL user:1 token",
            ),
            (
                Mutation::AddSetMember {
                    key: key.clone(),
                    member: b"alpha".to_vec(),
                },
                "SADD user:1",
            ),
            (
                Mutation::DeleteSetMember {
                    key: key.clone(),
                    member: b"alpha".to_vec(),
                },
                "SREM user:1",
            ),
        ];
        for (mutation, label) in cases {
            assert_eq!(mutation.command_label(), label);
            assert_eq!(mutation.key(), &key);
        }
    }
}
