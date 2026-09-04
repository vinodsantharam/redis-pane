//! The Loaded set: every scanned key, stored columnar and capped (ADR-0010).
//!
//! Key names live in one contiguous byte arena addressed by `(offset, len)`.
//! Metadata lives in parallel arrays of primitives — never a `Vec` of per-key
//! structs, and never `Option<T>`, whose niche padding would cost more than the
//! sentinel it replaces. Sorting permutes an index vector rather than moving
//! any of this.
//!
//! Why hold everything rather than a window: `SCAN` cursors are forward-only
//! with no random access, the iteration order is unspecified, and a full
//! iteration may return the same key more than once. A sliding window cannot
//! seek, so scrolling backwards would mean replaying cursors while the visible
//! set shifts underneath the reader. Holding everything is both simpler and
//! better — bounded by a documented cap rather than by hope.

/// The Redis type of a key. Stored as one byte per key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum KeyKind {
    String = 1,
    List = 2,
    Set = 3,
    ZSet = 4,
    Hash = 5,
    Stream = 6,
    Json = 7,
    Other = 8,
}

impl KeyKind {
    pub fn label(&self) -> &'static str {
        match self {
            KeyKind::String => "string",
            KeyKind::List => "list",
            KeyKind::Set => "set",
            KeyKind::ZSet => "zset",
            KeyKind::Hash => "hash",
            KeyKind::Stream => "stream",
            KeyKind::Json => "json",
            KeyKind::Other => "other",
        }
    }

    pub fn from_redis(s: &str) -> KeyKind {
        match s {
            "string" => KeyKind::String,
            "list" => KeyKind::List,
            "set" => KeyKind::Set,
            "zset" => KeyKind::ZSet,
            "hash" => KeyKind::Hash,
            "stream" => KeyKind::Stream,
            "ReJSON-RL" => KeyKind::Json,
            _ => KeyKind::Other,
        }
    }

    fn from_byte(b: u8) -> Option<KeyKind> {
        match b {
            1 => Some(KeyKind::String),
            2 => Some(KeyKind::List),
            3 => Some(KeyKind::Set),
            4 => Some(KeyKind::ZSet),
            5 => Some(KeyKind::Hash),
            6 => Some(KeyKind::Stream),
            7 => Some(KeyKind::Json),
            8 => Some(KeyKind::Other),
            _ => None,
        }
    }
}

/// Metadata is fetched lazily (R2.4), so every column needs a "not yet known"
/// state. These sentinels cost nothing; `Option<u32>` would cost 4 bytes a key.
const TTL_UNKNOWN: i32 = i32::MIN;
/// Redis reports `-1` for a key with no expiry.
pub const TTL_NONE: i32 = -1;
const SIZE_UNKNOWN: u32 = u32::MAX;
/// A key that was scanned but had vanished by the time its metadata was
/// fetched. It rides in the `kinds` array — `0` already means "not yet known"
/// and `1..=8` are the types, so a fourth state costs no bytes at all across a
/// two-million-key set. A parallel `Vec<bool>` would have cost 2MB to say the
/// same thing about the handful of keys it is ever true for.
const KIND_GONE: u8 = u8::MAX;

/// The default cap on retained keys.
///
/// At roughly 15 bytes of index per key plus the name itself, two million keys
/// of average 45 bytes is about 120MB — comfortably inside the 250MB budget
/// (PRD §7) with room for a prefix index and everything else. Reaching it stops
/// the scan and says so; it does not grow until the OOM killer intervenes.
pub const DEFAULT_CAP: usize = 2_000_000;

/// Every key the session has scanned, and what is known about each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedSet {
    /// One contiguous allocation holding every key name back to back.
    arena: Vec<u8>,
    offsets: Vec<u32>,
    lens: Vec<u16>,
    kinds: Vec<u8>,
    ttls: Vec<i32>,
    sizes: Vec<u32>,
    cap: usize,
    capped: bool,
}

impl Default for LoadedSet {
    fn default() -> Self {
        Self::with_cap(DEFAULT_CAP)
    }
}

impl LoadedSet {
    pub fn with_cap(cap: usize) -> Self {
        Self {
            arena: Vec::new(),
            offsets: Vec::new(),
            lens: Vec::new(),
            kinds: Vec::new(),
            ttls: Vec::new(),
            sizes: Vec::new(),
            cap,
            capped: false,
        }
    }

    pub fn len(&self) -> usize {
        self.offsets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    pub fn cap(&self) -> usize {
        self.cap
    }

    /// Whether the cap has been reached. When true the scan must stop and the
    /// status bar must say so — a limit the user cannot see is a limit they
    /// will mistake for the whole keyspace.
    pub fn is_capped(&self) -> bool {
        self.capped
    }

    /// Add a key. Returns `false` once the cap is reached, at which point the
    /// caller should stop scanning.
    pub fn push(&mut self, name: &[u8]) -> bool {
        if self.len() >= self.cap {
            self.capped = true;
            return false;
        }
        // A name longer than u16 can address is pathological; truncating would
        // silently show the wrong key, so it is refused instead.
        let Ok(len) = u16::try_from(name.len()) else {
            return true;
        };
        self.offsets.push(self.arena.len() as u32);
        self.lens.push(len);
        self.arena.extend_from_slice(name);
        self.kinds.push(0);
        self.ttls.push(TTL_UNKNOWN);
        self.sizes.push(SIZE_UNKNOWN);
        true
    }

    pub fn name(&self, i: usize) -> Option<&[u8]> {
        let start = *self.offsets.get(i)? as usize;
        let len = *self.lens.get(i)? as usize;
        self.arena.get(start..start + len)
    }

    /// The key name as text. Redis keys are arbitrary bytes, so this is lossy
    /// by necessity; the binary Viewer is where exact bytes are shown.
    pub fn name_str(&self, i: usize) -> Option<std::borrow::Cow<'_, str>> {
        self.name(i).map(String::from_utf8_lossy)
    }

    /// Where key `i`'s name starts in the arena.
    ///
    /// Exposed so a prefix index can address segments of a name without copying
    /// them (R2.3).
    pub fn name_offset(&self, i: usize) -> Option<u32> {
        self.offsets.get(i).copied()
    }

    /// A borrowed slice of the arena.
    pub fn arena_slice(&self, offset: u32, len: u16) -> Option<&[u8]> {
        let start = offset as usize;
        self.arena.get(start..start + len as usize)
    }

    pub fn kind(&self, i: usize) -> Option<KeyKind> {
        self.kinds.get(i).copied().and_then(KeyKind::from_byte)
    }

    pub fn set_kind(&mut self, i: usize, kind: KeyKind) {
        if let Some(slot) = self.kinds.get_mut(i) {
            *slot = kind as u8;
        }
    }

    /// Whether key `i` was found to be gone when its metadata was fetched.
    ///
    /// `SCAN` walks a keyspace that moves underneath it, so a scanned key may
    /// already be deleted, expired or evicted by the time the row is drawn. The
    /// row stays where it is and is badged: silently dropping it would reorder
    /// everything below the reader's cursor, which is worse than a stale row.
    pub fn is_gone(&self, i: usize) -> bool {
        self.kinds.get(i).copied() == Some(KIND_GONE)
    }

    /// Mark key `i` gone.
    ///
    /// TTL and size keep whatever was last known, for the same reason a deleted
    /// open key keeps its value on screen: during an incident the question is
    /// usually what was in it. The type byte is what the tombstone displaces,
    /// and that costs nothing on screen — the type is drawn as a dot in the one
    /// column the `✕` badge now occupies, so there was never room for both.
    pub fn set_gone(&mut self, i: usize) {
        if let Some(slot) = self.kinds.get_mut(i) {
            *slot = KIND_GONE;
        }
    }

    /// TTL in seconds. `None` means not yet fetched; `Some(TTL_NONE)` means the
    /// key has no expiry. The distinction matters: one is a gap in what we
    /// know, the other is a fact about the key.
    pub fn ttl(&self, i: usize) -> Option<i32> {
        match self.ttls.get(i).copied() {
            Some(TTL_UNKNOWN) | None => None,
            Some(v) => Some(v),
        }
    }

    pub fn set_ttl(&mut self, i: usize, seconds: i32) {
        if let Some(slot) = self.ttls.get_mut(i) {
            *slot = seconds;
        }
    }

    /// Memory usage in bytes, or `None` if not yet fetched.
    pub fn size(&self, i: usize) -> Option<u32> {
        match self.sizes.get(i).copied() {
            Some(SIZE_UNKNOWN) | None => None,
            Some(v) => Some(v),
        }
    }

    pub fn set_size(&mut self, i: usize, bytes: u32) {
        if let Some(slot) = self.sizes.get_mut(i) {
            *slot = bytes.min(SIZE_UNKNOWN - 1);
        }
    }

    /// How many keys have their size fetched.
    ///
    /// Sorting by a lazily-fetched column orders what has arrived and states
    /// this count (R2.5); it never triggers a mass fetch.
    pub fn known_sizes(&self) -> usize {
        self.sizes.iter().filter(|s| **s != SIZE_UNKNOWN).count()
    }

    pub fn clear(&mut self) {
        self.arena.clear();
        self.offsets.clear();
        self.lens.clear();
        self.kinds.clear();
        self.ttls.clear();
        self.sizes.clear();
        self.capped = false;
    }

    /// Bytes held by this structure's allocations.
    ///
    /// This measures the data structure, not process RSS — it is what the
    /// memory budget in PRD §7 is spent on, and it can be asserted
    /// deterministically in a test where RSS cannot.
    pub fn heap_bytes(&self) -> usize {
        self.arena.capacity()
            + self.offsets.capacity() * 4
            + self.lens.capacity() * 2
            + self.kinds.capacity()
            + self.ttls.capacity() * 4
            + self.sizes.capacity() * 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_of(names: &[&str]) -> LoadedSet {
        let mut s = LoadedSet::default();
        for n in names {
            assert!(s.push(n.as_bytes()));
        }
        s
    }

    #[test]
    fn names_round_trip_through_the_arena() {
        let s = set_of(&["user:8812:session", "cart:91af", ""]);
        assert_eq!(s.len(), 3);
        assert_eq!(s.name_str(0).unwrap(), "user:8812:session");
        assert_eq!(s.name_str(1).unwrap(), "cart:91af");
        assert_eq!(s.name_str(2).unwrap(), "");
        assert!(s.name(3).is_none());
    }

    #[test]
    fn keys_are_arbitrary_bytes_not_utf8() {
        let mut s = LoadedSet::default();
        s.push(&[0xff, 0x00, 0x41]);
        assert_eq!(s.name(0).unwrap(), &[0xff, 0x00, 0x41]);
        // Lossy for display; the binary Viewer shows the real bytes.
        assert!(s.name_str(0).unwrap().contains('A'));
    }

    #[test]
    fn metadata_starts_unknown_and_unknown_is_not_a_value() {
        let mut s = set_of(&["k"]);
        assert_eq!(s.kind(0), None);
        assert_eq!(s.ttl(0), None);
        assert_eq!(s.size(0), None);

        s.set_kind(0, KeyKind::Hash);
        s.set_size(0, 2_150);
        assert_eq!(s.kind(0), Some(KeyKind::Hash));
        assert_eq!(s.size(0), Some(2_150));
    }

    #[test]
    fn a_key_with_no_expiry_is_a_fact_not_a_gap() {
        // The distinction the sentinel exists for: "we have not asked" and
        // "there is no TTL" must not render the same way.
        let mut s = set_of(&["k"]);
        assert_eq!(s.ttl(0), None, "not yet fetched");
        s.set_ttl(0, TTL_NONE);
        assert_eq!(s.ttl(0), Some(TTL_NONE), "fetched, and there is no expiry");
    }

    #[test]
    fn known_sizes_counts_what_has_actually_arrived() {
        let mut s = set_of(&["a", "b", "c"]);
        assert_eq!(s.known_sizes(), 0);
        s.set_size(1, 400);
        assert_eq!(
            s.known_sizes(),
            1,
            "sorting by size orders this many, and says so"
        );
    }

    #[test]
    fn the_cap_stops_the_set_growing_and_is_visible() {
        let mut s = LoadedSet::with_cap(3);
        for i in 0..3 {
            assert!(s.push(format!("k{i}").as_bytes()), "within the cap");
        }
        assert!(!s.is_capped());
        assert!(
            !s.push(b"k3"),
            "push must report the cap rather than growing"
        );
        assert!(
            s.is_capped(),
            "and the state must be visible to the status bar"
        );
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn a_tombstone_costs_no_memory_and_reports_no_type() {
        let mut s = LoadedSet::default();
        s.push(b"a");
        s.push(b"b");
        s.set_kind(0, KeyKind::Hash);
        s.set_ttl(0, 90);
        s.set_size(0, 4_096);
        let before = s.heap_bytes();

        s.set_gone(0);

        assert!(s.is_gone(0));
        assert!(!s.is_gone(1), "its neighbours are untouched");
        assert_eq!(
            s.heap_bytes(),
            before,
            "the tombstone rides in the kinds array; it must allocate nothing"
        );
        assert_eq!(
            s.kind(0),
            None,
            "the type byte is what the tombstone displaces"
        );
        // The row keeps what was last known about it, for the same reason a
        // deleted open key keeps its value on screen (update.rs, ValueGone).
        assert_eq!(s.ttl(0), Some(90));
        assert_eq!(s.size(0), Some(4_096));
    }

    #[test]
    fn a_rescan_clears_tombstones() {
        let mut s = LoadedSet::default();
        s.push(b"a");
        s.set_gone(0);
        s.clear();
        s.push(b"a");
        assert!(
            !s.is_gone(0),
            "a rescan re-reads the keyspace, so nothing carries over"
        );
    }

    #[test]
    fn clearing_resets_the_cap_flag_too() {
        let mut s = LoadedSet::with_cap(1);
        s.push(b"a");
        s.push(b"b");
        assert!(s.is_capped());
        s.clear();
        assert!(s.is_empty());
        assert!(!s.is_capped());
    }

    /// M1.1's proof (R2.6, PRD §7): a million keys inside the memory budget.
    ///
    /// This measures the structure's own allocations rather than process RSS,
    /// which is what the budget is actually spent on and what can be asserted
    /// deterministically.
    #[test]
    fn a_million_keys_fit_inside_the_budget() {
        let mut s = LoadedSet::default();
        for i in 0..1_000_000u32 {
            // Representative of real keyspaces: a namespaced key of ~25 bytes.
            assert!(s.push(format!("user:{i:08}:session").as_bytes()));
        }
        assert_eq!(s.len(), 1_000_000);

        let bytes = s.heap_bytes();
        let budget = 250 * 1024 * 1024;
        assert!(
            bytes < budget / 2,
            "1M keys took {}MB; the whole process budget is {}MB and the key \
             list must leave room for a prefix index, the Viewer, and the runtime",
            bytes / 1024 / 1024,
            budget / 1024 / 1024
        );

        // Per-key overhead should stay in the tens of bytes, not the hundreds.
        let per_key = bytes / s.len();
        assert!(per_key < 80, "{per_key} bytes per key is too much");
    }

    #[test]
    fn a_pathologically_long_name_is_refused_rather_than_truncated() {
        // Truncating would silently display, and later act on, the wrong key.
        let mut s = LoadedSet::default();
        let huge = vec![b'x'; u16::MAX as usize + 1];
        assert!(s.push(&huge), "not a cap failure");
        assert_eq!(s.len(), 0, "but not stored either");
    }
}
