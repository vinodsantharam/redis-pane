//! Reading a value, arming tracking in the same breath (PLAN M1.9, M1.10).
//!
//! Every read of the open key goes through [`read_value`]. There is no sibling
//! that reads without arming, and there should never be one: tracking is
//! consumed by the invalidation it produces, so a read that skipped arming
//! would leave the Viewer dark while the header still said live (ADR-0006).

use fred::prelude::*;
use redis_pane_core::state::value::{
    BinaryValue, IndexedValue, JsonValue, MemberValue, PairValue, ScoredValue, StreamValue,
    StringValue, Value,
};

/// How much of a large collection to fetch. The Viewer is for reading, not for
/// exporting; a bounded window keeps a 4MB list from arriving as one reply.
const WINDOW: i64 = 500;

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
) -> Result<Option<ReadValue>, Error> {
    let key: Key = name.into();

    // Arm first: `CLIENT CACHING YES` applies to the next read-only command on
    // this connection. Do not "simplify" this into fred's Options.caching,
    // which is inert in 10.1.0.
    let _: () = client.client_caching(true).await?;
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
    let entries: Vec<(String, Vec<(String, String)>)> = client
        .xrange(key, "-", "+", Some(WINDOW as u64))
        .await
        .unwrap_or_default();
    Ok(Value::Stream(StreamValue {
        entries,
        total: total.max(0) as usize,
    }))
}
