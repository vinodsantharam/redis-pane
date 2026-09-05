//! Reading a value, arming tracking in the same breath (PLAN M1.9, M1.10).
//!
//! Every read of the open key goes through [`read_value`], which arms tracking
//! in the same breath — because tracking is consumed by the invalidation it
//! produces, and a read that skipped arming would leave the Viewer dark while
//! the header still said live (ADR-0006).
//!
//! The one refinement, found against a real managed server: **there is nothing
//! to arm on a server that refuses `CLIENT TRACKING`.** Upstash rejects
//! `CLIENT CACHING` outright, so sending it unconditionally made every read
//! fail — the app could browse a keyspace and open nothing in it. Arming is
//! therefore driven by the capability probe, and by nothing else, which is what
//! [`Arming`] exists to make explicit. A bare `bool` here would invite a caller
//! to pass `false` for convenience, and that caller would silently go dark.

use fred::prelude::*;
use redis_pane_core::state::value::{
    BinaryValue, IndexedValue, JsonValue, MemberValue, PairValue, ScoredValue, StreamValue,
    StringValue, Value,
};

/// How much of a large collection to fetch. The Viewer is for reading, not for
/// exporting; a bounded window keeps a 4MB list from arriving as one reply.
const WINDOW: i64 = 500;

/// Whether this connection can arm tracking at all.
///
/// Comes from the capability probe at connect (ADR-0007) and from nowhere else.
/// It is not a preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arming {
    /// The server accepted `CLIENT TRACKING`; every read re-arms.
    Enabled,
    /// The server refused it. Liveness is already `○ manual`, so there is
    /// nothing to arm and sending `CLIENT CACHING` would only fail the read.
    Unsupported,
}

/// Serialises reads on one connection, and supersedes the ones nobody wants.
///
/// `CLIENT CACHING YES` arms **the next read-only command on the connection**,
/// not a command it is bundled with. Two reads running at once can therefore
/// interleave — A arms, B arms, A reads, B reads — and leave the server
/// tracking a key the Viewer is not showing. The header would go on saying
/// `● live` over a value nothing will ever push an update for, which is the
/// failure ADR-0006 exists to make unreachable, reached by a different route.
///
/// So the arm-and-read pair is indivisible: [`ReadGate::begin`] cancels the
/// previous read and hands out a permit, and the holder runs to completion.
/// Cancelling matters as much as serialising — dropping a *reply* does not
/// un-send the `CLIENT CACHING` that went with it, and only a read that never
/// runs arms nothing.
#[derive(Debug, Default)]
pub struct ReadGate {
    lock: std::sync::Arc<tokio::sync::Mutex<()>>,
    current: Option<tokio_util::sync::CancellationToken>,
}

/// The right to perform one read, once it is this read's turn.
pub struct ReadPermit {
    lock: std::sync::Arc<tokio::sync::Mutex<()>>,
    cancel: tokio_util::sync::CancellationToken,
}

impl ReadGate {
    /// Supersede whatever is in flight and take a permit for a new read.
    pub fn begin(&mut self) -> ReadPermit {
        let cancel = tokio_util::sync::CancellationToken::new();
        if let Some(previous) = self.current.replace(cancel.clone()) {
            previous.cancel();
        }
        ReadPermit {
            lock: self.lock.clone(),
            cancel,
        }
    }
}

impl ReadPermit {
    /// Wait for the connection, then run `read`. Returns `None` if this read was
    /// superseded before it reached the wire.
    ///
    /// Two chances to drop out before touching the connection: while queueing,
    /// and on acquiring it. Past that point the read completes — interrupting it
    /// mid-way is what would leave the connection armed for a key nobody is
    /// looking at.
    pub async fn run<F, T>(self, read: F) -> Option<T>
    where
        F: std::future::Future<Output = T>,
    {
        let _guard = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => return None,
            guard = self.lock.lock() => guard,
        };
        if self.cancel.is_cancelled() {
            return None;
        }
        Some(read.await)
    }
}

/// What a completed read produced.
pub struct ReadValue {
    pub value: Value,
    pub ttl_seconds: i32,
    pub size_bytes: u32,
}

/// Read a key and re-arm tracking for it.
pub async fn read_value(
    client: &Client,
    name: &[u8],
    pane_width: usize,
    arming: Arming,
) -> Result<Option<ReadValue>, Error> {
    let key: Key = name.into();

    // Arm first: `CLIENT CACHING YES` applies to the next read-only command on
    // this connection. Do not "simplify" this into fred's Options.caching,
    // which is inert in 10.1.0.
    if arming == Arming::Enabled {
        let _: () = client.client_caching(true).await?;
    }
    let kind: String = client.r#type(key.clone()).await?;
    if kind == "none" {
        return Ok(None);
    }

    let ttl: i64 = client.ttl(key.clone()).await.unwrap_or(-1);
    let size: i64 = client
        .memory_usage(key.clone(), None)
        .await
        .unwrap_or(Some(0))
        .unwrap_or(0);

    let value = match kind.as_str() {
        "string" => string_value(client, key, pane_width).await?,
        "hash" => {
            let map: Vec<(String, String)> = client.hgetall(key).await?;
            Value::Hash(PairValue { pairs: map })
        }
        "list" => {
            let total: i64 = client.llen(key.clone()).await.unwrap_or(0);
            let items: Vec<String> = client.lrange(key, 0, WINDOW - 1).await?;
            Value::List(IndexedValue {
                items,
                total: total.max(0) as usize,
            })
        }
        "set" => {
            let total: i64 = client.scard(key.clone()).await.unwrap_or(0);
            let members: Vec<String> = client.smembers(key).await?;
            Value::Set(MemberValue {
                members,
                total: total.max(0) as usize,
            })
        }
        "zset" => {
            let total: i64 = client.zcard(key.clone()).await.unwrap_or(0);
            let entries: Vec<(String, f64)> = client
                .zrange(key, 0, WINDOW - 1, None, false, None, true)
                .await?;
            Value::ZSet(ScoredValue {
                entries,
                total: total.max(0) as usize,
            })
        }
        "stream" => stream_value(client, key).await?,
        // A type this build does not model still gets shown, as bytes. Refusing
        // to display something is worse than displaying it plainly.
        _ => {
            let bytes: Vec<u8> = client
                .get::<Option<Vec<u8>>, _>(key)
                .await?
                .unwrap_or_default();
            Value::Binary(BinaryValue { bytes })
        }
    };

    Ok(Some(ReadValue {
        value,
        ttl_seconds: ttl.clamp(-1, i32::MAX as i64) as i32,
        size_bytes: size.clamp(0, u32::MAX as i64 - 1) as u32,
    }))
}

/// Strings are the one type whose *shape* is not given by its Redis type: it
/// may be JSON, text, or arbitrary bytes, and each wants a different viewer.
async fn string_value(client: &Client, key: Key, width: usize) -> Result<Value, Error> {
    let bytes: Vec<u8> = client
        .get::<Option<Vec<u8>>, _>(key)
        .await?
        .unwrap_or_default();
    match String::from_utf8(bytes.clone()) {
        Ok(text) => {
            let trimmed = text.trim_start();
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                Ok(Value::Json(JsonValue::parse(&text)))
            } else {
                Ok(Value::Str(StringValue::new(&text, width)))
            }
        }
        // Not valid UTF-8, so it is a blob. A hex dump is honest; mojibake is not.
        Err(_) => Ok(Value::Binary(BinaryValue { bytes })),
    }
}

async fn stream_value(client: &Client, key: Key) -> Result<Value, Error> {
    let total: i64 = client.xlen(key.clone()).await.unwrap_or(0);
    // XREVRANGE, not XRANGE: DESIGN §6.3 asks for a reverse-chronological
    // timeline, and it is not only ordering. XRANGE("-", "+", COUNT) takes the
    // *oldest* COUNT entries — on a stream past the window size, that was the
    // ancient history, not the recent activity a triage view actually needs.
    // XREVRANGE("+", "-", COUNT) takes the most recent COUNT, newest first.
    let entries: Vec<(String, Vec<(String, String)>)> = client
        .xrevrange(key, "+", "-", Some(WINDOW as u64))
        .await
        .unwrap_or_default();
    Ok(Value::Stream(StreamValue {
        entries,
        total: total.max(0) as usize,
    }))
}
