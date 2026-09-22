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
use crate::state::value::ListEnd;

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
    /// Guarded `LSET key index value`, refusing unless the element at
    /// `index` still holds `expected` (ADR-0017 D2). A List element has no
    /// recreate-the-key hazard the way `HSET`/`SADD` do — `LSET` cannot
    /// recreate a gone key — so the guard is a compare-and-set on the
    /// element itself, not `EXISTS` alone: a concurrent push/pop anywhere in
    /// the list shifts every index, and a plain `LSET` staged against a
    /// stale index would silently overwrite whatever moved into that slot.
    SetListElement {
        key: KeyName,
        index: usize,
        expected: Vec<u8>,
        value: Vec<u8>,
    },
    /// Guarded `LPUSH`/`RPUSH key value`, never recreating the key
    /// (ADR-0017 D2). No compare-and-set half: an add addresses no existing
    /// element, so there is nothing to compare against — only the ordinary
    /// `EXISTS` guard, because `LPUSH`/`RPUSH` (unlike `LSET`) do recreate a
    /// gone key.
    AddListElement {
        key: KeyName,
        end: ListEnd,
        value: Vec<u8>,
    },
    /// Duplicate-safe remove-by-index (`LREM`, via a disposable per-call
    /// sentinel, ADR-0017 D2), refusing unless the element at `index` still
    /// holds `expected`. Redis has no remove-by-index primitive — a plain
    /// `LREM key 1 value` removes the *first* match from the head, which is
    /// the wrong element whenever the list has an earlier duplicate — so the
    /// shell's script `LSET`s the target to a sentinel first, then `LREM`s
    /// the sentinel. **The sentinel itself is minted in the shell**
    /// (`crates/app/src/redis/mutate.rs`), not here: the core has no
    /// randomness source (`update()`'s contract is no I/O, no clock, no
    /// randomness) and none should be added for this — see ADR-0017's
    /// "sentinel is minted in the shell" note.
    DeleteListElement {
        key: KeyName,
        index: usize,
        expected: Vec<u8>,
    },
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
            | Mutation::DeleteSetMember { key, .. }
            | Mutation::SetListElement { key, .. }
            | Mutation::AddListElement { key, .. }
            | Mutation::DeleteListElement { key, .. } => key,
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
            // The index, not the element: an index is a position, safe to
            // show, unlike a value that could be arbitrarily large (ADR-0017).
            Mutation::SetListElement { key, index, .. } => format!("LSET {key} {index}"),
            Mutation::AddListElement { key, end, .. } => match end {
                ListEnd::Head => format!("LPUSH {key}"),
                ListEnd::Tail => format!("RPUSH {key}"),
            },
            Mutation::DeleteListElement { key, index, .. } => format!("LREM {key} {index}"),
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
    /// A `SetListElement` or `DeleteListElement` found the element at the
    /// staged index no longer held the bytes it was staged against (ADR-0017
    /// D2/D3). Not [`NotWritten::FieldGone`] — a stale index is expected to
    /// be the *routine* refusal on a busy list (any concurrent push, pop or
    /// edit anywhere in the list shifts every index, not just a write to the
    /// same element), so it gets its own variant and its own wording rather
    /// than borrowing a neighbour's "gone" framing.
    ElementMoved,
}

impl NotWritten {
    /// What to tell the reader, in the notification that names the refused
    /// command (R7.4).
    ///
    /// Here rather than at the two places that report it, so a new guard
    /// forces a wording decision once instead of silently inheriting a
    /// neighbour's — the shape the reporting code had before this, where a
    /// nested match needed an arm for a variant it could never see and gave
    /// it another guard's words.
    pub fn reason(&self) -> &'static str {
        match self {
            NotWritten::KeyGone => "key no longer exists",
            NotWritten::FieldGone => "field no longer exists",
            NotWritten::FieldExists => "field already exists",
            NotWritten::MemberExists => "member already exists",
            // ADR-0017 D3: deliberately longer than the others — this is
            // the refusal a reader sees routinely on a list under churn, so
            // it names the mechanism and tells them to look again rather
            // than reading as "the write itself is broken".
            NotWritten::ElementMoved => {
                "that element moved — the list changed underneath it, look again"
            }
        }
    }
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
            (
                Mutation::SetListElement {
                    key: key.clone(),
                    index: 3,
                    expected: b"old".to_vec(),
                    value: b"new".to_vec(),
                },
                "LSET user:1 3",
            ),
            (
                Mutation::AddListElement {
                    key: key.clone(),
                    end: ListEnd::Head,
                    value: b"new".to_vec(),
                },
                "LPUSH user:1",
            ),
            (
                Mutation::AddListElement {
                    key: key.clone(),
                    end: ListEnd::Tail,
                    value: b"new".to_vec(),
                },
                "RPUSH user:1",
            ),
            (
                Mutation::DeleteListElement {
                    key: key.clone(),
                    index: 2,
                    expected: b"gone".to_vec(),
                },
                "LREM user:1 2",
            ),
        ];
        for (mutation, label) in cases {
            assert_eq!(mutation.command_label(), label);
            assert_eq!(mutation.key(), &key);
        }
    }

    #[test]
    fn element_moved_reads_as_look_again_not_it_failed() {
        let reason = NotWritten::ElementMoved.reason();
        assert_eq!(
            reason,
            "that element moved — the list changed underneath it, look again"
        );
    }
}
