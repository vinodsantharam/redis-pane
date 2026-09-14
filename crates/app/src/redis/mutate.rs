//! The one place a confirmed write reaches the server (R4.4, review H1).
//!
//! [`execute`] is the whole interface the shell uses: a [`Mutation`] in, a
//! [`MutationOutcome`] out, and what that outcome means is the core's to
//! decide. The functions after it are its parts — each guards against a
//! different way a write can land on the wrong thing — and stay public so the
//! integration suite can pin each guard against a real server.

use fred::prelude::*;
use redis_pane_core::mutation::{Mutation, MutationOutcome, NotWritten};

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
