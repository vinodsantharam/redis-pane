//! The one place a confirmed write reaches the server (R4.4, review H1).
//!
//! [`execute`] is the whole interface the shell uses: a [`Mutation`] in, a
//! [`MutationOutcome`] out, and what that outcome means is the core's to
//! decide. The functions after it are its parts — each guards against a
//! different way a write can land on the wrong thing — and stay public so the
//! integration suite can pin each guard against a real server.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use fred::prelude::*;
use redis_pane_core::mutation::{Mutation, MutationOutcome, NotWritten};
use redis_pane_core::state::value::ListEnd;

/// Execute a confirmed write.
///
/// The caller must not treat success as the value now on screen: the core
/// re-reads through the one read path afterward, never trusting what it just
/// wrote (ADR-0006).
pub async fn execute(client: &Client, mutation: &Mutation) -> Result<MutationOutcome, Error> {
    let key = mutation.key().as_bytes();
    Ok(match mutation {
        Mutation::DeleteKey { .. } => {
            delete_key(client, key).await?;
            MutationOutcome::Done
        }
        Mutation::SetString { value, .. } => {
            if set_value(client, key, value).await? {
                MutationOutcome::Done
            } else {
                MutationOutcome::NotWritten(NotWritten::KeyGone)
            }
        }
        Mutation::SetHashField { field, value, .. } => {
            match set_hash_field(client, key, field, value).await? {
                FieldWrite::Written => MutationOutcome::Done,
                FieldWrite::FieldGone => MutationOutcome::NotWritten(NotWritten::FieldGone),
                FieldWrite::KeyGone => MutationOutcome::NotWritten(NotWritten::KeyGone),
            }
        }
        Mutation::AddHashField { field, value, .. } => {
            match add_hash_field(client, key, field, value).await? {
                FieldAdd::Added => MutationOutcome::Done,
                FieldAdd::FieldExists => MutationOutcome::NotWritten(NotWritten::FieldExists),
                FieldAdd::KeyGone => MutationOutcome::NotWritten(NotWritten::KeyGone),
            }
        }
        Mutation::DeleteHashField { field, .. } => {
            if delete_hash_field(client, key, field).await? {
                MutationOutcome::Done
            } else {
                MutationOutcome::NothingToRemove
            }
        }
        Mutation::AddSetMember { member, .. } => match add_set_member(client, key, member).await? {
            MemberAdd::Added => MutationOutcome::Done,
            MemberAdd::MemberExists => MutationOutcome::NotWritten(NotWritten::MemberExists),
            MemberAdd::KeyGone => MutationOutcome::NotWritten(NotWritten::KeyGone),
        },
        Mutation::DeleteSetMember { member, .. } => {
            if delete_set_member(client, key, member).await? {
                MutationOutcome::Done
            } else {
                MutationOutcome::NothingToRemove
            }
        }
        Mutation::SetListElement {
            index,
            expected,
            value,
            ..
        } => match set_list_element(client, key, *index, expected, value).await? {
            ListElementWrite::Written => MutationOutcome::Done,
            ListElementWrite::ElementMoved => MutationOutcome::NotWritten(NotWritten::ElementMoved),
            ListElementWrite::KeyGone => MutationOutcome::NotWritten(NotWritten::KeyGone),
        },
        Mutation::AddListElement { end, value, .. } => {
            match add_list_element(client, key, *end, value).await? {
                ListElementAdd::Added => MutationOutcome::Done,
                ListElementAdd::KeyGone => MutationOutcome::NotWritten(NotWritten::KeyGone),
            }
        }
        Mutation::DeleteListElement {
            index, expected, ..
        } => match delete_list_element(client, key, *index, expected).await? {
            ListElementDelete::Removed => MutationOutcome::Done,
            ListElementDelete::ElementMoved => {
                MutationOutcome::NotWritten(NotWritten::ElementMoved)
            }
            ListElementDelete::KeyGone => MutationOutcome::NotWritten(NotWritten::KeyGone),
        },
        Mutation::SetZSetScore { member, score, .. } => {
            match set_zset_score(client, key, member, *score).await? {
                ZSetScoreWrite::Written => MutationOutcome::Done,
                ZSetScoreWrite::MemberGone => MutationOutcome::NotWritten(NotWritten::MemberGone),
                ZSetScoreWrite::KeyGone => MutationOutcome::NotWritten(NotWritten::KeyGone),
            }
        }
        Mutation::AddZSetMember { member, score, .. } => {
            match add_zset_member(client, key, member, *score).await? {
                ZSetMemberAdd::Added => MutationOutcome::Done,
                ZSetMemberAdd::MemberExists => {
                    MutationOutcome::NotWritten(NotWritten::MemberExists)
                }
                ZSetMemberAdd::KeyGone => MutationOutcome::NotWritten(NotWritten::KeyGone),
            }
        }
        Mutation::DeleteZSetMember { member, .. } => {
            if delete_zset_member(client, key, member).await? {
                MutationOutcome::Done
            } else {
                MutationOutcome::NothingToRemove
            }
        }
        Mutation::SetTtl { seconds, .. } => match set_ttl(client, key, *seconds).await? {
            SetTtlWrite::Written => MutationOutcome::Done,
            SetTtlWrite::KeyGone => MutationOutcome::NotWritten(NotWritten::KeyGone),
        },
        Mutation::PersistTtl { .. } => match persist_ttl(client, key).await? {
            PersistTtlWrite::Written => MutationOutcome::Done,
            PersistTtlWrite::NoExpiry => MutationOutcome::NothingToRemove,
            PersistTtlWrite::KeyGone => MutationOutcome::NotWritten(NotWritten::KeyGone),
        },
        Mutation::ShiftTtl { delta_seconds, .. } => {
            match shift_ttl(client, key, *delta_seconds).await? {
                ShiftTtlWrite::Written => MutationOutcome::Done,
                ShiftTtlWrite::NoExpiry => MutationOutcome::NotWritten(NotWritten::NoExpiry),
                ShiftTtlWrite::WouldExpireNow => {
                    MutationOutcome::NotWritten(NotWritten::WouldExpireNow)
                }
                ShiftTtlWrite::KeyGone => MutationOutcome::NotWritten(NotWritten::KeyGone),
            }
        }
    })
}

/// Delete one key outright (`DEL`, R4.3, PLAN M2.3).
///
/// Whether the key still existed when this ran is not this function's
/// question: `DEL` on a key that is already gone is a success reporting zero
/// removed, and what the core does with `MutationOutcome::Done` is the same either way —
/// the key is gone now, which is the only fact the Viewer badges.
///
/// **Must build a `Key` before calling `del`, never hand it a bare `Vec<u8>`.**
/// `fred`'s `del<R, K: Into<MultipleKeys>>` takes *one or more* keys, and
/// `Vec<T>` converts to "many keys" whenever `T: Into<Key>` — which `u8` is,
/// for the numeric-key convenience (`DEL 1` meaning the key named `"1"`). So
/// `client.del(name.to_vec())` compiles, returns `Ok`, and does something
/// else entirely: it sends `DEL <byte0> <byte1> …`, one key per raw byte
/// value of `name`, none of which exist, so nothing is ever deleted while the
/// call still reports success. Wrapping `name` in a single `Key` first (via
/// `Key`'s own `From<&[u8]>`) is what selects the single-binary-key
/// conversion instead of the elementwise one. The same shape of trap as the
/// `Options { caching: Some(true) }` note above — a fred call that compiles
/// clean and lies about what it sent.
pub async fn delete_key(client: &Client, name: &[u8]) -> Result<(), Error> {
    let key = fred::types::Key::from(name);
    let _: i64 = client.del(key).await?;
    Ok(())
}

/// Overwrite a String value (`SET`, R4.1, PLAN M2 task 4).
///
/// `set`'s key parameter is `K: Into<Key>` — a single key, never
/// `Into<MultipleKeys>` — so it does not carry `del`'s trap above: there is
/// no elementwise `Vec<T> -> Key` conversion to fall into by accident. Built
/// the same way regardless (`Key::from(name)`, `name: &[u8]`) for the same
/// reason `delete_key` is: `Vec<u8>` has no `Into<Key>` impl of its own, only
/// `&[u8]` does, so passing `name.to_vec()` here would simply fail to
/// compile rather than silently misbehave — a narrower trap than `del`'s, but
/// worth the explicit build-a-`Key`-first habit regardless, given fred has
/// already surprised this module once.
///
/// Sent as `SET name new KEEPTTL XX`, the command the confirm dialog
/// previews. A plain `SET` clears the key's TTL, which quietly made every
/// edited key permanent; `KEEPTTL` keeps whatever TTL the key has when the
/// write lands. `XX` writes only if the key still exists: `Ok(false)` means
/// it was gone — expired or deleted under the dialog — and nothing was
/// written. The key is never recreated (ADR-0014).
///
/// The caller must not treat a successful call as the value now on screen —
/// the core always re-reads through the one read path afterward, never
/// trusting what it just wrote (ADR-0006).
pub async fn set_value(client: &Client, name: &[u8], new: &[u8]) -> Result<bool, Error> {
    let key = fred::types::Key::from(name);
    // `XX` answers nil rather than `OK` when it wrote nothing.
    let reply: Option<String> = client
        .set(
            key,
            new.to_vec(),
            Some(fred::types::Expiration::KEEPTTL),
            Some(fred::types::SetOptions::XX),
            false,
        )
        .await?;
    Ok(reply.is_some())
}

/// Lua guard for editing an existing Hash field's value (PLAN M2 task 6, D1,
/// ADR-0015).
///
/// `KEYS[1]` is the hash key; `ARGV[1]`/`ARGV[2]` are the field and its new
/// value, both binary-safe. Returns `-1` if the key is already gone (never
/// recreated), `0` if the field is already gone (nothing written), or `1` on
/// success.
///
/// A plain `HSET` would do the write but not the guard: it recreates a key
/// that expired or was deleted under the dialog (the same hazard
/// [`set_value`] closes for Strings), and it silently clears the field's own
/// TTL (`HEXPIRE`, 7.4+) as a side effect. So the field's expire time is read
/// before the write and reapplied after. `HPEXPIRETIME` is 7.4+ only;
/// `redis.pcall`, not `redis.call`, is what lets this one script still run to
/// completion on 6.0–7.0, where the unknown subcommand comes back as a Lua
/// error table instead of aborting the script — verified against
/// `redis:6.2-alpine` below.
const HASH_FIELD_EDIT_SCRIPT: &str = r#"
if redis.call('EXISTS', KEYS[1]) == 0 then
  return -1
end
if redis.call('HEXISTS', KEYS[1], ARGV[1]) == 0 then
  return 0
end
local ttl = redis.pcall('HPEXPIRETIME', KEYS[1], 'FIELDS', 1, ARGV[1])
redis.call('HSET', KEYS[1], ARGV[1], ARGV[2])
if type(ttl) == 'table' and not ttl.err and ttl[1] and tonumber(ttl[1]) and tonumber(ttl[1]) > 0 then
  redis.call('HPEXPIREAT', KEYS[1], ttl[1], 'FIELDS', 1, ARGV[1])
end
return 1
"#;

/// Lua guard for adding a Hash field that must not already exist (PLAN M2
/// task 6, D1, ADR-0015).
///
/// `KEYS[1]` is the hash key; `ARGV[1]`/`ARGV[2]` are the new field and its
/// value. Returns `-1` if the key is already gone (never recreated), or
/// `HSETNX`'s own `0`/`1` otherwise — `0` means an existing field, including
/// one outside the 500-field read window, was left untouched. `HSETNX`
/// already refuses to overwrite a field on its own; the only thing the script
/// adds is the key-existence guard in front of it.
const HASH_FIELD_ADD_SCRIPT: &str = r#"
if redis.call('EXISTS', KEYS[1]) == 0 then
  return -1
end
return redis.call('HSETNX', KEYS[1], ARGV[1], ARGV[2])
"#;

/// What a guarded field edit did on the server ([`set_hash_field`], D1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldWrite {
    /// The field was overwritten; its own TTL, if any, was preserved.
    Written,
    /// The field was already gone; nothing was written.
    FieldGone,
    /// The key was already gone; nothing was written, nothing recreated.
    KeyGone,
}

/// What a guarded field add did on the server ([`add_hash_field`], D1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldAdd {
    /// The field did not exist and was created.
    Added,
    /// The field already existed; the existing value was left untouched.
    FieldExists,
    /// The key was already gone; nothing was written, nothing recreated.
    KeyGone,
}

/// Edit an existing Hash field's value, keeping the field's own TTL (`EVAL`,
/// PLAN M2 task 6, D1, ADR-0015).
///
/// `name` is built as a binary-safe `Key` exactly like [`delete_key`] — see
/// its doc comment for the `Vec<u8>` conversion trap this avoids. `field` and
/// `value` travel as `Vec<u8>` `ARGV` entries: fred specialises
/// `Vec<u8> -> Value` as one `Bytes` value rather than an elementwise array,
/// so each stays one binary-safe argument, not one per byte — a different
/// corner of the same trap, on the `EVAL` args side rather than the keys
/// side.
///
/// Sent with `EVAL`, never `EVALSHA`: there is no script cache to manage, and
/// a cache miss on a fresh connection would otherwise need a fallback path.
///
/// The caller must not treat [`FieldWrite::Written`] as the value now on
/// screen — the read path is always re-run afterward (ADR-0006), same as
/// [`set_value`].
pub async fn set_hash_field(
    client: &Client,
    name: &[u8],
    field: &[u8],
    value: &[u8],
) -> Result<FieldWrite, Error> {
    let key = fred::types::Key::from(name);
    let result: i64 = client
        .eval(
            HASH_FIELD_EDIT_SCRIPT,
            vec![key],
            vec![field.to_vec(), value.to_vec()],
        )
        .await?;
    Ok(match result {
        -1 => FieldWrite::KeyGone,
        0 => FieldWrite::FieldGone,
        _ => FieldWrite::Written,
    })
}

/// Add a new Hash field, never overwriting one that already exists (`EVAL`,
/// PLAN M2 task 6, D1, ADR-0015).
///
/// Guards the same gone-key hazard as [`set_hash_field`] — see its doc
/// comment for the binary-safety notes, which apply here unchanged.
pub async fn add_hash_field(
    client: &Client,
    name: &[u8],
    field: &[u8],
    value: &[u8],
) -> Result<FieldAdd, Error> {
    let key = fred::types::Key::from(name);
    let result: i64 = client
        .eval(
            HASH_FIELD_ADD_SCRIPT,
            vec![key],
            vec![field.to_vec(), value.to_vec()],
        )
        .await?;
    Ok(match result {
        -1 => FieldAdd::KeyGone,
        0 => FieldAdd::FieldExists,
        _ => FieldAdd::Added,
    })
}

/// Remove one Hash field (`HDEL`, PLAN M2 task 6).
///
/// `false` means the field was already gone — including the case where the
/// whole key is gone, since `HDEL` on a missing key is simply zero fields
/// removed, the same "already true" shape [`delete_key`] reports for `DEL`.
/// Deleting the last field deletes the key itself; that is Redis's own
/// behaviour, not something this function arranges.
///
/// `field` is wrapped in a single `Key` before the call, for the same reason
/// `delete_key` wraps `name`: `hdel`'s `fields` parameter is
/// `Into<MultipleKeys>`, and a bare `Vec<u8>` would convert elementwise (one
/// key per byte) rather than as one binary-safe field.
pub async fn delete_hash_field(client: &Client, name: &[u8], field: &[u8]) -> Result<bool, Error> {
    let key = fred::types::Key::from(name);
    let field_key = fred::types::Key::from(field);
    let removed: i64 = client.hdel(key, field_key).await?;
    Ok(removed > 0)
}

/// Lua guard for adding a Set member, never recreating a key that is gone
/// (PLAN M2 task 7, D2, ADR-0016).
///
/// `KEYS[1]` is the set key; `ARGV[1]` is the member, binary-safe. Returns
/// `-1` if the key is already gone (never recreated), or `SADD`'s own `0`/`1`
/// otherwise — `0` means the member was already there, including one outside
/// the 500-member read window. Unlike the Hash edit script, there is no
/// per-member TTL to preserve (`HEXPIRE` is Hash-only, ADR-0016) — the only
/// thing this guards is the key's own existence.
const SET_MEMBER_ADD_SCRIPT: &str = r#"
if redis.call('EXISTS', KEYS[1]) == 0 then
  return -1
end
return redis.call('SADD', KEYS[1], ARGV[1])
"#;

/// What a guarded member add did on the server ([`add_set_member`], D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberAdd {
    /// The member did not exist and was added.
    Added,
    /// The member already existed; nothing was written.
    MemberExists,
    /// The key was already gone; nothing was written, nothing recreated.
    KeyGone,
}

/// Add a new Set member, never recreating a key that is gone (`EVAL`, PLAN M2
/// task 7, D2, ADR-0016).
///
/// `name` and `member` travel the same binary-safe way [`set_hash_field`]'s
/// `name`/`field` do — see its doc comment for the `Vec<u8>` conversion traps
/// this avoids, on both the key side and the `EVAL` args side.
pub async fn add_set_member(
    client: &Client,
    name: &[u8],
    member: &[u8],
) -> Result<MemberAdd, Error> {
    let key = fred::types::Key::from(name);
    let result: i64 = client
        .eval(SET_MEMBER_ADD_SCRIPT, vec![key], vec![member.to_vec()])
        .await?;
    Ok(match result {
        -1 => MemberAdd::KeyGone,
        0 => MemberAdd::MemberExists,
        _ => MemberAdd::Added,
    })
}

/// Remove one Set member (`SREM`, PLAN M2 task 7, ADR-0016).
///
/// `false` means the member was already gone — including the case where the
/// whole key is gone, since `SREM` on a missing key is simply zero members
/// removed, the same "already true" shape [`delete_hash_field`] reports for
/// `HDEL`. Deleting the last member deletes the key itself; that is Redis's
/// own behaviour, not something this function arranges.
pub async fn delete_set_member(client: &Client, name: &[u8], member: &[u8]) -> Result<bool, Error> {
    let key = fred::types::Key::from(name);
    let member_key = fred::types::Key::from(member);
    let removed: i64 = client.srem(key, member_key).await?;
    Ok(removed > 0)
}

/// Lua guard for editing an existing List element's value by index (PLAN M2
/// task 8, D2, ADR-0017).
///
/// `KEYS[1]` is the list key; `ARGV[1]` is the index as a decimal string,
/// `ARGV[2]` the bytes the read found there, `ARGV[3]` the new value.
/// Returns `-1` if the key is already gone (`LSET` on a missing key errors
/// rather than recreating it, so this guard exists purely to *report* that
/// case apart from the next one), `-2` if the element at the index no
/// longer holds `ARGV[2]` — the list shifted under a concurrent push, pop or
/// edit — or `1` on success.
///
/// Unlike [`HASH_FIELD_EDIT_SCRIPT`], there is no per-element TTL to
/// preserve: Redis 7.4's field expiry is Hash-only, and a key's own TTL is
/// untouched by `LSET`/`LPUSH`/`RPUSH`/`LREM` (ADR-0017's verified facts).
const LIST_ELEMENT_EDIT_SCRIPT: &str = r#"
if redis.call('EXISTS', KEYS[1]) == 0 then
  return -1
end
if redis.call('LINDEX', KEYS[1], ARGV[1]) ~= ARGV[2] then
  return -2
end
redis.call('LSET', KEYS[1], ARGV[1], ARGV[3])
return 1
"#;

/// Lua guard for a duplicate-safe remove-by-index (PLAN M2 task 8, D2,
/// ADR-0017).
///
/// `KEYS[1]` is the list key; `ARGV[1]` the index, `ARGV[2]` the bytes the
/// read found there, `ARGV[3]` a disposable per-call sentinel (never a
/// constant — see [`delete_list_element`]'s doc comment). Same `-1`/`-2`/`1`
/// shape as [`LIST_ELEMENT_EDIT_SCRIPT`]. Redis has no remove-by-index
/// primitive: a plain `LREM key 1 value` removes the *first* match from the
/// head, which is the wrong element whenever an earlier duplicate exists
/// (ADR-0017's verified fact on `[x, y, x, z]`). `LSET`ing the target to a
/// sentinel first, then `LREM`ing the sentinel, is what makes the remove
/// exact regardless of duplicates — and because the script runs atomically,
/// no other client can ever observe the list in its momentary sentinel
/// state.
const LIST_ELEMENT_DELETE_SCRIPT: &str = r#"
if redis.call('EXISTS', KEYS[1]) == 0 then
  return -1
end
if redis.call('LINDEX', KEYS[1], ARGV[1]) ~= ARGV[2] then
  return -2
end
redis.call('LSET', KEYS[1], ARGV[1], ARGV[3])
redis.call('LREM', KEYS[1], 1, ARGV[3])
return 1
"#;

/// Lua guard for adding a List element at either end, never recreating a key
/// that is gone (PLAN M2 task 8, D2, D6, ADR-0017).
///
/// `KEYS[1]` is the list key; `ARGV[1]` is the literal command name —
/// `"LPUSH"` or `"RPUSH"`, chosen by the caller from
/// [`redis_pane_core::state::value::ListEnd`] — and `ARGV[2]` the new
/// element. Returns `-1` if the key is already gone (never recreated,
/// unlike a bare `LPUSH`/`RPUSH`, which would), or the pushed length
/// otherwise. No compare-and-set half: an add addresses no existing element,
/// so there is nothing to compare against.
const LIST_ELEMENT_ADD_SCRIPT: &str = r#"
if redis.call('EXISTS', KEYS[1]) == 0 then
  return -1
end
return redis.call(ARGV[1], KEYS[1], ARGV[2])
"#;

/// What a guarded element edit did on the server ([`set_list_element`], D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListElementWrite {
    /// The element was overwritten.
    Written,
    /// The element at the index no longer held the expected bytes; the list
    /// shifted underneath the stage (ADR-0017 D3).
    ElementMoved,
    /// The key was already gone; nothing was written, nothing recreated.
    KeyGone,
}

/// What a guarded element delete did on the server ([`delete_list_element`],
/// D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListElementDelete {
    /// The element was removed; if it was the list's only element, the key
    /// went with it (Redis's own behaviour, not something this arranges).
    Removed,
    /// The element at the index no longer held the expected bytes; the list
    /// shifted underneath the stage (ADR-0017 D3).
    ElementMoved,
    /// The key was already gone; nothing was written, nothing recreated.
    KeyGone,
}

/// What a guarded element add did on the server ([`add_list_element`], D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListElementAdd {
    /// The element was pushed to the requested end.
    Added,
    /// The key was already gone; nothing was written, nothing recreated.
    KeyGone,
}

/// Process-lifetime counter feeding [`list_delete_sentinel`] — see its doc
/// comment for why a sentinel is minted per call rather than a constant.
static LIST_SENTINEL_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Mint a fresh `__redis-pane-rp:<suffix>` sentinel for one delete call
/// (PLAN M2 task 8, D2, ADR-0017).
///
/// **Minted in the shell, not the core.** `Mutation::DeleteListElement`
/// carries the key, the index and the bytes that were read — no sentinel
/// field — because the core has no randomness source: `update()`'s contract
/// is no I/O, no clock, no randomness, and there is no `rand` dependency
/// anywhere in the workspace. This is a detail of *how* `mutate.rs`
/// implements a delete-by-index Redis has no primitive for, not something
/// the reader asked for.
///
/// The suffix needs to be unlikely to equal a real element of one list, not
/// cryptographically random, so it is the wall clock's nanoseconds paired
/// with a process-lifetime [`AtomicU64`] counter — no new dependency, and a
/// constant sentinel would be wrong the moment two elements of a list ever
/// happened to equal it, however unlikely (ADR-0017 D2).
fn list_delete_sentinel() -> Vec<u8> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let count = LIST_SENTINEL_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("__redis-pane-rp:{nanos}-{count}").into_bytes()
}

/// Edit an existing List element's value by index, refusing unless it still
/// holds `expected` (`EVAL`, PLAN M2 task 8, D2, ADR-0017).
///
/// `name` is built as a binary-safe `Key` exactly like [`set_hash_field`] —
/// see its doc comment for the `Vec<u8>` conversion traps this avoids, on
/// both the key side and the `EVAL` args side. `index` travels as a decimal
/// string `ARGV` entry: Lua's `LINDEX`/`LSET` take a numeric index, and
/// `EVAL`'s `ARGV` is binary-safe strings, not integers.
///
/// The caller must not treat [`ListElementWrite::Written`] as the value now
/// on screen — the read path is always re-run afterward (ADR-0006), same as
/// [`set_value`].
pub async fn set_list_element(
    client: &Client,
    name: &[u8],
    index: usize,
    expected: &[u8],
    new: &[u8],
) -> Result<ListElementWrite, Error> {
    let key = fred::types::Key::from(name);
    let result: i64 = client
        .eval(
            LIST_ELEMENT_EDIT_SCRIPT,
            vec![key],
            vec![
                index.to_string().into_bytes(),
                expected.to_vec(),
                new.to_vec(),
            ],
        )
        .await?;
    Ok(match result {
        -1 => ListElementWrite::KeyGone,
        -2 => ListElementWrite::ElementMoved,
        _ => ListElementWrite::Written,
    })
}

/// Add a new List element at either end, never recreating a key that is gone
/// (`EVAL`, PLAN M2 task 8, D2, D6, ADR-0017).
///
/// `end` picks the literal command name the script runs — `LPUSH` for
/// [`ListEnd::Head`], `RPUSH` for [`ListEnd::Tail`] — travelling as an
/// `ARGV` entry the same way [`LIST_ELEMENT_ADD_SCRIPT`]'s doc comment
/// describes.
pub async fn add_list_element(
    client: &Client,
    name: &[u8],
    end: ListEnd,
    value: &[u8],
) -> Result<ListElementAdd, Error> {
    let key = fred::types::Key::from(name);
    let command: &[u8] = match end {
        ListEnd::Head => b"LPUSH",
        ListEnd::Tail => b"RPUSH",
    };
    let result: i64 = client
        .eval(
            LIST_ELEMENT_ADD_SCRIPT,
            vec![key],
            vec![command.to_vec(), value.to_vec()],
        )
        .await?;
    Ok(match result {
        -1 => ListElementAdd::KeyGone,
        _ => ListElementAdd::Added,
    })
}

/// Remove one List element by index, duplicate-safely, refusing unless it
/// still holds `expected` (`EVAL`, PLAN M2 task 8, D2, ADR-0017).
///
/// Mints a fresh [`list_delete_sentinel`] for this call alone — see its doc
/// comment for why the sentinel is generated here, per call, rather than
/// being a constant or living in the core's `Mutation`.
pub async fn delete_list_element(
    client: &Client,
    name: &[u8],
    index: usize,
    expected: &[u8],
) -> Result<ListElementDelete, Error> {
    let key = fred::types::Key::from(name);
    let sentinel = list_delete_sentinel();
    let result: i64 = client
        .eval(
            LIST_ELEMENT_DELETE_SCRIPT,
            vec![key],
            vec![index.to_string().into_bytes(), expected.to_vec(), sentinel],
        )
        .await?;
    Ok(match result {
        -1 => ListElementDelete::KeyGone,
        -2 => ListElementDelete::ElementMoved,
        _ => ListElementDelete::Removed,
    })
}

/// Lua guard for editing an existing ZSet member's score (PLAN M2 task 9,
/// D2, ADR-0018).
///
/// `KEYS[1]` is the ZSet key; `ARGV[1]` is the member, binary-safe;
/// `ARGV[2]` the new score, as a decimal string. Returns `-1` if the key is
/// already gone, `-2` if the member is already gone, or `1` on success.
///
/// **`ZADD XX` already refuses on its own to create a key that is gone** —
/// unlike `HSET`/`SADD`/`LPUSH` — so the `EXISTS` half is redundant for
/// safety; it is kept only so a gone key can be *reported* apart from a gone
/// member, the same reason [`LIST_ELEMENT_EDIT_SCRIPT`] keeps its redundant
/// `EXISTS`. The `ZSCORE` half is not redundant, and is the real reason for
/// the script: `ZADD XX CH` returns the count of elements *changed*, so it
/// answers `0` both when the member is absent **and** when it is present
/// with that score already — two completely different things to tell the
/// reader, which a bare `ZADD` cannot separate (ADR-0018 D2).
const ZSET_SCORE_EDIT_SCRIPT: &str = r#"
if redis.call('EXISTS', KEYS[1]) == 0 then
  return -1
end
if redis.call('ZSCORE', KEYS[1], ARGV[1]) == false then
  return -2
end
redis.call('ZADD', KEYS[1], 'XX', ARGV[2], ARGV[1])
return 1
"#;

/// Lua guard for adding a ZSet member+score, never recreating a key that is
/// gone and never overwriting an existing member's score (PLAN M2 task 9,
/// D2, ADR-0018).
///
/// `KEYS[1]` is the ZSet key; `ARGV[1]` the member, binary-safe; `ARGV[2]`
/// the score, as a decimal string. Returns `-1` if the key is already gone
/// (never recreated), or `ZADD NX`'s own `0`/`1` otherwise — `0` means the
/// member was already there, including one outside the 500-member read
/// window, and its score was left untouched.
const ZSET_MEMBER_ADD_SCRIPT: &str = r#"
if redis.call('EXISTS', KEYS[1]) == 0 then
  return -1
end
return redis.call('ZADD', KEYS[1], 'NX', ARGV[2], ARGV[1])
"#;

/// What a guarded score edit did on the server ([`set_zset_score`], D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZSetScoreWrite {
    /// The score was overwritten.
    Written,
    /// The member was already gone; nothing was written.
    MemberGone,
    /// The key was already gone; nothing was written, nothing recreated.
    KeyGone,
}

/// What a guarded member add did on the server ([`add_zset_member`], D2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZSetMemberAdd {
    /// The member did not exist and was added with the given score.
    Added,
    /// The member already existed; its score was left untouched.
    MemberExists,
    /// The key was already gone; nothing was written, nothing recreated.
    KeyGone,
}

/// Edit an existing ZSet member's score, refusing unless the member is
/// still there (`EVAL`, PLAN M2 task 9, D1, D2, ADR-0018).
///
/// `name` and `member` travel the same binary-safe way [`set_hash_field`]'s
/// `name`/`field` do — see its doc comment for the `Vec<u8>` conversion
/// traps this avoids. `score` travels as a decimal-string `ARGV` entry via
/// `f64`'s `Display`, which is Rust's shortest-round-trip representation —
/// the same guarantee [`crate::state::value::format_score`] (in
/// `redis-pane-core`) relies on for the Viewer's SCORE column, so what
/// reaches the server is exactly the value that was staged, never a
/// precision-lossy approximation of it.
///
/// The caller must not treat [`ZSetScoreWrite::Written`] as the value now on
/// screen — the read path is always re-run afterward (ADR-0006), same as
/// [`set_value`].
pub async fn set_zset_score(
    client: &Client,
    name: &[u8],
    member: &[u8],
    score: f64,
) -> Result<ZSetScoreWrite, Error> {
    let key = fred::types::Key::from(name);
    let result: i64 = client
        .eval(
            ZSET_SCORE_EDIT_SCRIPT,
            vec![key],
            vec![member.to_vec(), score.to_string().into_bytes()],
        )
        .await?;
    Ok(match result {
        -1 => ZSetScoreWrite::KeyGone,
        -2 => ZSetScoreWrite::MemberGone,
        _ => ZSetScoreWrite::Written,
    })
}

/// Add a new ZSet member+score, never recreating a key that is gone and
/// never overwriting an existing member's score (`EVAL`, PLAN M2 task 9, D2,
/// ADR-0018).
///
/// Guards the same gone-key hazard as [`set_zset_score`] — see its doc
/// comment for the binary-safety and score-formatting notes, which apply
/// here unchanged.
pub async fn add_zset_member(
    client: &Client,
    name: &[u8],
    member: &[u8],
    score: f64,
) -> Result<ZSetMemberAdd, Error> {
    let key = fred::types::Key::from(name);
    let result: i64 = client
        .eval(
            ZSET_MEMBER_ADD_SCRIPT,
            vec![key],
            vec![member.to_vec(), score.to_string().into_bytes()],
        )
        .await?;
    Ok(match result {
        -1 => ZSetMemberAdd::KeyGone,
        0 => ZSetMemberAdd::MemberExists,
        _ => ZSetMemberAdd::Added,
    })
}

/// Remove one ZSet member (`ZREM`, PLAN M2 task 9, D2, D8, ADR-0018).
///
/// `false` means the member was already gone — including the case where the
/// whole key is gone, since `ZREM` on a missing key is simply zero members
/// removed, the same "already true" shape [`delete_set_member`] reports for
/// `SREM`. Deleting the last member deletes the key itself; that is Redis's
/// own behaviour, not something this function arranges.
pub async fn delete_zset_member(
    client: &Client,
    name: &[u8],
    member: &[u8],
) -> Result<bool, Error> {
    let key = fred::types::Key::from(name);
    let member_key = fred::types::Key::from(member);
    let removed: i64 = client.zrem(key, member_key).await?;
    Ok(removed > 0)
}

/// What a set TTL did on the server ([`set_ttl`], PLAN M2 task 10, D5,
/// ADR-0019).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetTtlWrite {
    /// The TTL was set.
    Written,
    /// The key was already gone; nothing was written, nothing recreated.
    KeyGone,
}

/// Set a key's TTL outright (`EXPIRE`, PLAN M2 task 10, D1, D5, ADR-0019).
///
/// `name` is built as a binary-safe `Key`, the same way [`delete_key`]'s doc
/// comment explains. No script: `EXPIRE` already refuses on its own to
/// create a key that is gone, and its `0` means exactly that and nothing
/// else — unlike `PERSIST`'s `0` and unlike `ZADD XX CH`'s (ADR-0019 D5).
///
/// **`seconds` must never be `0` or negative here** — `EXPIRE key 0` (or a
/// negative argument) deletes the key immediately, verified against a live
/// server for ADR-0019. `crate::state::ttl::parse_ttl_edit`'s D4 grammar
/// refuses `0` at the keyboard before a `Mutation::SetTtl` carrying one can
/// even be built, so this function does not re-check it.
pub async fn set_ttl(client: &Client, name: &[u8], seconds: i32) -> Result<SetTtlWrite, Error> {
    let key = fred::types::Key::from(name);
    let result: i64 = client.expire(key, i64::from(seconds), None).await?;
    Ok(if result == 1 {
        SetTtlWrite::Written
    } else {
        SetTtlWrite::KeyGone
    })
}

/// Lua guard for clearing a key's TTL (PLAN M2 task 10, D5, ADR-0019).
///
/// `KEYS[1]` is the key. Returns `-1` if the key is already gone, `0` if the
/// key already had no expiry, or `1` if the expiry was removed. Verified end
/// to end for ADR-0019: a gone key, a key with no expiry, and a key with an
/// expiry, all three return codes exercised. A bare `PERSIST` answers `0` to
/// both of the first two cases — the same ambiguity `ZADD XX CH` had, and
/// "the key you were looking at is gone" and "it already never expired" are
/// different things to tell a reader who just asked to persist.
const TTL_PERSIST_SCRIPT: &str = r#"
if redis.call('EXISTS', KEYS[1]) == 0 then
  return -1
end
return redis.call('PERSIST', KEYS[1])
"#;

/// Lua guard for extending or shortening a key's TTL by a signed delta
/// (PLAN M2 task 10, D5, D7, ADR-0019).
///
/// `KEYS[1]` is the key; `ARGV[1]` the signed delta in seconds, as a decimal
/// string. Returns `-1` if the key is already gone, `-2` if the key has no
/// expiry to shift, `-3` if the shift would land at or below zero — which
/// `EXPIRE` would immediately delete the key for, verified against a live
/// server for ADR-0019 — or `1` on success. Verified end to end for
/// ADR-0019: a gone key, a no-expiry key, a would-expire-now shorten (which
/// left the key's TTL untouched), and both an extend and a shorten that
/// landed. The delta applies to the TTL as the server sees it at write
/// time, not the TTL staged at open time (D7) — the whole reason this needs
/// a script rather than a plain `EXPIRE`, along with `EXPIRE ... GT/LT`
/// being 7.0+, below the 6.0 floor (ADR-0007).
const TTL_SHIFT_SCRIPT: &str = r#"
local t = redis.call('TTL', KEYS[1])
if t == -2 then
  return -1
end
if t == -1 then
  return -2
end
local n = t + tonumber(ARGV[1])
if n <= 0 then
  return -3
end
redis.call('EXPIRE', KEYS[1], n)
return 1
"#;

/// What a guarded persist did on the server ([`persist_ttl`], D5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistTtlWrite {
    /// The expiry was removed.
    Written,
    /// The key already had no expiry; nothing was written. Not a
    /// `NotWritten` (D6): nothing was refused, and the core settles this as
    /// `MutationOutcome::NothingToRemove`, the same shape `HDEL`'s own
    /// "nothing to remove" case has.
    NoExpiry,
    /// The key was already gone; nothing was written, nothing recreated.
    KeyGone,
}

/// Clear a key's TTL, telling a gone key apart from one that already had no
/// expiry (`EVAL`, PLAN M2 task 10, D1, D5, ADR-0019).
///
/// `name` travels the same binary-safe way [`set_hash_field`]'s `name` does
/// — see its doc comment for the `Vec<u8>` conversion traps this avoids.
pub async fn persist_ttl(client: &Client, name: &[u8]) -> Result<PersistTtlWrite, Error> {
    let key = fred::types::Key::from(name);
    let result: i64 = client
        .eval(TTL_PERSIST_SCRIPT, vec![key], Vec::<Vec<u8>>::new())
        .await?;
    Ok(match result {
        -1 => PersistTtlWrite::KeyGone,
        0 => PersistTtlWrite::NoExpiry,
        _ => PersistTtlWrite::Written,
    })
}

/// What a guarded shift did on the server ([`shift_ttl`], D5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShiftTtlWrite {
    /// The TTL was extended or shortened.
    Written,
    /// The key has no expiry to shift.
    NoExpiry,
    /// The shift would land at or below zero; the key's TTL is untouched.
    WouldExpireNow,
    /// The key was already gone; nothing was written, nothing recreated.
    KeyGone,
}

/// Extend or shorten a key's TTL by a signed delta, atomically against the
/// TTL the server sees at write time (`EVAL`, PLAN M2 task 10, D1, D5, D7,
/// ADR-0019).
///
/// `name` travels the same binary-safe way [`set_hash_field`]'s `name` does.
/// `delta_seconds` travels as a decimal-string `ARGV` entry — signed, unlike
/// [`set_list_element`]'s unsigned index — Lua's `tonumber` parses a leading
/// `-` directly.
pub async fn shift_ttl(
    client: &Client,
    name: &[u8],
    delta_seconds: i32,
) -> Result<ShiftTtlWrite, Error> {
    let key = fred::types::Key::from(name);
    let result: i64 = client
        .eval(
            TTL_SHIFT_SCRIPT,
            vec![key],
            vec![delta_seconds.to_string().into_bytes()],
        )
        .await?;
    Ok(match result {
        -1 => ShiftTtlWrite::KeyGone,
        -2 => ShiftTtlWrite::NoExpiry,
        -3 => ShiftTtlWrite::WouldExpireNow,
        _ => ShiftTtlWrite::Written,
    })
}
