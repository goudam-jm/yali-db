//! Experimental byte-key storage with sync-before-success, TTL, CAS and compaction.
//!
//! [`Db::memory`] is volatile; [`Db::open`] persists to a private local directory.
//! Every operation is serialized by a mutex; clones share the same owner/lock.
//! TTL assumes a maintained wall clock and is not an authorization mechanism.
//! See the repository's format and operations documents before evaluating durability.
#![forbid(unsafe_code)]

mod format;
mod storage;

#[cfg(feature = "fuzzing")]
#[doc(hidden)]
pub mod fuzz_support;

use std::{
    collections::HashMap,
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use storage::Disk;

/// Maximum nonempty key length in bytes.
pub const MAX_KEY_BYTES: usize = 1024;
/// Maximum value length; empty values are valid.
pub const MAX_VALUE_BYTES: usize = 65536;
/// Maximum scan page count.
pub const MAX_SCAN_ENTRIES: usize = 100;
const MAX_SCAN_BYTES: usize = 1024 * 1024;
// Keep one maximum tombstone worth of headroom so a full database can still delete.
const DELETE_RESERVE: usize = 41 + MAX_KEY_BYTES;

/// Database operation result.
pub type Result<T> = std::result::Result<T, Error>;

/// Failures do not include user keys/values. I/O failures can have an unknown write outcome.
#[derive(Debug)]
pub enum Error {
    /// Invalid public API input.
    Invalid(&'static str),
    /// A conditional write did not match current live state.
    Conflict,
    /// The sequence cannot be incremented without wrapping.
    VersionExhausted,
    /// A clock reading or deadline cannot be represented.
    Clock,
    /// Admission limit reached; compact, delete, or increase the explicit limit.
    Capacity(&'static str),
    /// Another owner holds the database lock.
    Locked,
    /// An earlier I/O/integrity error or panic invalidated this owner. Reopen it.
    Unhealthy,
    /// Damaged/unsupported storage. No automatic salvage is performed.
    Corruption {
        path: PathBuf,
        offset: u64,
        reason: &'static str,
    },
    /// An underlying filesystem error.
    Io(std::io::Error),
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(s) => write!(f, "invalid input: {s}"),
            Self::Conflict => write!(f, "condition conflict"),
            Self::VersionExhausted => write!(f, "mutation sequence exhausted"),
            Self::Clock => write!(f, "clock or TTL overflow"),
            Self::Capacity(s) => write!(f, "capacity limit: {s}"),
            Self::Locked => write!(f, "database locked by another owner"),
            Self::Unhealthy => write!(f, "database unhealthy; drop all handles and reopen"),
            Self::Corruption {
                path,
                offset,
                reason,
            } => write!(f, "{} at offset {offset}: {reason}", path.display()),
            Self::Io(e) => write!(
                f,
                "storage I/O error (mutation outcome may be unknown): {e}"
            ),
        }
    }
}
impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        if let Self::Io(e) = self {
            Some(e)
        } else {
            None
        }
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Injectable fallible wall clock. Implementations must not call back into the database.
pub trait Clock: Send + Sync {
    /// Absolute UTC milliseconds since the Unix epoch.
    fn now_ms(&self) -> Result<u64>;
}
/// Production wall clock. Backward movement is clamped within each owner lifetime.
pub struct SystemClock;
impl Clock for SystemClock {
    fn now_ms(&self) -> Result<u64> {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::Clock)?
            .as_millis()
            .try_into()
            .map_err(|_| Error::Clock)
    }
}

/// Resource admission limits. Log cap excludes temporary compaction headroom.
#[derive(Clone, Debug)]
pub struct Options {
    /// Maximum indexed keys, including lazily expired keys until purge/compaction.
    pub max_keys: usize,
    /// Maximum bytes in the selected log generation. Compaction may need twice this space.
    pub max_log_bytes: u64,
    /// Consider compaction above this size when obsolete history exceeds half the log.
    pub compact_after_bytes: u64,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            max_keys: 100_000,
            max_log_bytes: 64 * 1024 * 1024,
            compact_after_bytes: 8 * 1024 * 1024,
        }
    }
}
impl Options {
    fn validate(&self) -> Result<()> {
        if self.max_keys == 0
            || self.max_log_bytes
                < (format::MAX_FRAME as u64 + format::HEADER_LEN + DELETE_RESERVE as u64)
            || self.compact_after_bytes == 0
            || self.compact_after_bytes > self.max_log_bytes
        {
            return Err(Error::Invalid("invalid resource options"));
        }
        Ok(())
    }
}

/// Put precondition, checked against the live record under writer serialization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Condition {
    Any,
    Absent,
    Version(u64),
}
/// Delete precondition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeleteCondition {
    Any,
    Version(u64),
}
/// Owned read result; modifying it cannot change the database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub value: Vec<u8>,
    pub version: u64,
    pub expires_at_ms: Option<u64>,
}
/// Durable put receipt (volatile when using `Db::memory`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub version: u64,
    pub expires_at_ms: Option<u64>,
}
/// Sorted, bounded scan response. Continuation is exclusive and is not a snapshot.
#[derive(Debug, PartialEq, Eq)]
pub struct Page {
    pub entries: Vec<(Vec<u8>, Entry)>,
    pub next_key: Option<Vec<u8>>,
}
/// Low-cardinality owner diagnostics. Expired keys can remain indexed until maintenance.
#[derive(Debug)]
pub struct Stats {
    pub indexed_keys: usize,
    pub indexed_key_bytes: usize,
    pub log_bytes: u64,
    pub sequence: u64,
    pub healthy: bool,
}

#[derive(Clone)]
enum Value {
    Memory(Vec<u8>),
    Disk { offset: u64, size: u64 },
}
#[derive(Clone)]
struct Index {
    version: u64,
    expires: u64,
    value: Value,
}
impl Index {
    fn live(&self, now: u64) -> bool {
        self.expires == 0 || now < self.expires
    }
}

struct State {
    map: HashMap<Vec<u8>, Index>,
    sequence: u64,
    time_floor: u64,
    indexed_frame_bytes: u64,
    disk: Option<Disk>,
    healthy: bool,
}

/// Cloneable single-owner database. All reads/writes/maintenance share one mutex.
#[derive(Clone)]
pub struct Db {
    inner: Arc<Mutex<State>>,
    clock: Arc<dyn Clock>,
    options: Options,
}

impl Db {
    /// Open/create a persistent database. The parent directory must already exist.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with(path, Options::default(), Arc::new(SystemClock))
    }
    /// Open using explicit limits and clock. Lower limits may prevent reopening larger databases.
    pub fn open_with(
        path: impl AsRef<Path>,
        options: Options,
        clock: Arc<dyn Clock>,
    ) -> Result<Self> {
        options.validate()?;
        let (disk, map, sequence) = Disk::open(path.as_ref(), &options)?;
        let indexed_frame_bytes = frame_bytes(&map);
        Ok(Self {
            inner: Arc::new(Mutex::new(State {
                map,
                sequence,
                time_floor: 0,
                indexed_frame_bytes,
                disk: Some(disk),
                healthy: true,
            })),
            clock,
            options,
        })
    }
    /// Create a volatile store with default options and system time.
    pub fn memory() -> Self {
        Self::memory_with(Options::default(), Arc::new(SystemClock))
            .expect("default options are valid")
    }
    /// Create a volatile store with deterministic clock/explicit key admission limit.
    pub fn memory_with(options: Options, clock: Arc<dyn Clock>) -> Result<Self> {
        options.validate()?;
        Ok(Self {
            inner: Arc::new(Mutex::new(State {
                map: HashMap::new(),
                sequence: 0,
                time_floor: 0,
                indexed_frame_bytes: 0,
                disk: None,
                healthy: true,
            })),
            clock,
            options,
        })
    }
    fn run<T>(&self, op: impl FnOnce(&mut State) -> Result<T>) -> Result<T> {
        let mut s = self.inner.lock().map_err(|_| Error::Unhealthy)?;
        if !s.healthy {
            return Err(Error::Unhealthy);
        }
        let result = op(&mut s);
        if matches!(&result, Err(Error::Io(_) | Error::Corruption { .. })) {
            s.healthy = false;
        }
        result
    }
    fn now(&self, s: &mut State) -> Result<u64> {
        s.time_floor = s.time_floor.max(self.clock.now_ms()?);
        Ok(s.time_floor)
    }
    /// Lookup a valid nonempty key; expired keys return `None`. Returned bytes are owned.
    pub fn get(&self, key: &[u8]) -> Result<Option<Entry>> {
        validate_key(key)?;
        self.run(|s| {
            let now = self.now(s)?;
            match s.map.get(key).filter(|i| i.live(now)).cloned() {
                Some(i) => Ok(Some(read_entry(s, key, &i)?)),
                None => Ok(None),
            }
        })
    }
    /// Insert/replace after validating a positive optional TTL and condition.
    /// A successful disk-backed call has flushed before returning. I/O error/timeout is ambiguous.
    pub fn put(
        &self,
        key: &[u8],
        value: &[u8],
        ttl_ms: Option<u64>,
        condition: Condition,
    ) -> Result<Receipt> {
        validate_key(key)?;
        if value.len() > MAX_VALUE_BYTES {
            return Err(Error::Invalid("value exceeds 65536 bytes"));
        }
        if ttl_ms == Some(0) {
            return Err(Error::Invalid("TTL must be positive"));
        }
        self.run(|s| {
            let now = self.now(s)?;
            let current = s.map.get(key).filter(|i| i.live(now));
            if !match condition {
                Condition::Any => true,
                Condition::Absent => current.is_none(),
                Condition::Version(v) => current.is_some_and(|i| i.version == v),
            } {
                return Err(Error::Conflict);
            }
            let expires = match ttl_ms {
                Some(ttl) => now.checked_add(ttl).ok_or(Error::Clock)?,
                None => 0,
            };
            let version = s.sequence.checked_add(1).ok_or(Error::VersionExhausted)?;
            if !s.map.contains_key(key) && s.map.len() >= self.options.max_keys {
                return Err(Error::Capacity(
                    "indexed keys; purge or compact expired keys",
                ));
            }
            prepare(
                s,
                now,
                &self.options,
                41 + key.len() + value.len() + DELETE_RESERVE,
            )?;
            let location = if let Some(disk) = &mut s.disk {
                let (offset, size) = disk.append(false, version, expires, key, value)?;
                Value::Disk { offset, size }
            } else {
                Value::Memory(value.to_vec())
            };
            if let Some(old) = s.map.get(key) {
                s.indexed_frame_bytes -= index_size(key, old);
            }
            s.indexed_frame_bytes += (41 + key.len() + value.len()) as u64;
            s.map.insert(
                key.to_vec(),
                Index {
                    version,
                    expires,
                    value: location,
                },
            );
            s.sequence = version;
            Ok(Receipt {
                version,
                expires_at_ms: expiry(expires),
            })
        })
    }
    /// Delete a live key; unconditional deletion of an absent/expired key returns false.
    pub fn delete(&self, key: &[u8], condition: DeleteCondition) -> Result<bool> {
        validate_key(key)?;
        self.run(|s| {
            let now = self.now(s)?;
            let current = s.map.get(key).filter(|i| i.live(now));
            if let DeleteCondition::Version(v) = condition
                && current.is_none_or(|i| i.version != v)
            {
                return Err(Error::Conflict);
            }
            if current.is_none() {
                return Ok(false);
            }
            let version = s.sequence.checked_add(1).ok_or(Error::VersionExhausted)?;
            prepare(s, now, &self.options, 41 + key.len())?;
            if let Some(disk) = &mut s.disk {
                disk.append(true, version, 0, key, &[])?;
            }
            if let Some(old) = s.map.remove(key) {
                s.indexed_frame_bytes -= index_size(key, &old);
            }
            s.sequence = version;
            Ok(true)
        })
    }
    /// Byte-lexicographic scan, at most 100 entries/1 MiB key+value bytes per page.
    /// Temporary candidate storage is bounded by `limit+1`, but all indexed keys are visited.
    pub fn scan_prefix(&self, prefix: &[u8], after: Option<&[u8]>, limit: usize) -> Result<Page> {
        if prefix.len() > MAX_KEY_BYTES || !(1..=MAX_SCAN_ENTRIES).contains(&limit) {
            return Err(Error::Invalid("invalid prefix or scan limit"));
        }
        if let Some(key) = after {
            validate_key(key)?;
        }
        self.run(|s| {
            let now = self.now(s)?;
            let mut candidates: Vec<Vec<u8>> = Vec::with_capacity(limit + 1);
            for (key, index) in &s.map {
                if index.live(now)
                    && key.starts_with(prefix)
                    && after.is_none_or(|a| key.as_slice() > a)
                {
                    let position = candidates.binary_search(key).unwrap_or_else(|p| p);
                    if position <= limit {
                        candidates.insert(position, key.clone());
                        if candidates.len() > limit + 1 {
                            candidates.pop();
                        }
                    }
                }
            }
            let mut entries = Vec::new();
            let mut bytes = 0;
            for key in &candidates {
                if entries.len() == limit {
                    break;
                }
                let index = s.map.get(key).cloned().ok_or(Error::Unhealthy)?;
                let entry = read_entry(s, key, &index)?;
                if bytes + key.len() + entry.value.len() > MAX_SCAN_BYTES {
                    break;
                }
                bytes += key.len() + entry.value.len();
                entries.push((key.clone(), entry));
            }
            let next_key = if candidates.len() > entries.len() {
                entries.last().map(|(k, _)| k.clone())
            } else {
                None
            };
            Ok(Page { entries, next_key })
        })
    }
    /// Remove logically expired entries from the index. Does not make clock rollback safe across restarts.
    pub fn purge_expired(&self) -> Result<usize> {
        self.run(|s| {
            let now = self.now(s)?;
            let before = s.map.len();
            compact(s, now, &self.options)?;
            Ok(before - s.map.len())
        })
    }
    /// Stop-the-world compaction; preserves versions/high-water mark and permanently drops expired history.
    pub fn compact(&self) -> Result<()> {
        self.run(|s| {
            let now = self.now(s)?;
            compact(s, now, &self.options)
        })
    }
    /// Owner diagnostics. This remains accessible after a storage error.
    pub fn stats(&self) -> Result<Stats> {
        let s = self.inner.lock().map_err(|_| Error::Unhealthy)?;
        Ok(Stats {
            indexed_keys: s.map.len(),
            indexed_key_bytes: s.map.keys().map(Vec::len).sum(),
            log_bytes: s.disk.as_ref().map_or(0, |d| d.len),
            sequence: s.sequence,
            healthy: s.healthy,
        })
    }
}

fn validate_key(key: &[u8]) -> Result<()> {
    if key.is_empty() || key.len() > MAX_KEY_BYTES {
        Err(Error::Invalid("key must be 1..=1024 bytes"))
    } else {
        Ok(())
    }
}
fn expiry(expires: u64) -> Option<u64> {
    if expires == 0 { None } else { Some(expires) }
}
fn read_entry(s: &mut State, key: &[u8], index: &Index) -> Result<Entry> {
    let value = match &index.value {
        Value::Memory(v) => v.clone(),
        Value::Disk { offset, size } => s.disk.as_mut().ok_or(Error::Unhealthy)?.read(
            key,
            index.version,
            index.expires,
            *offset,
            *size,
        )?,
    };
    Ok(Entry {
        value,
        version: index.version,
        expires_at_ms: expiry(index.expires),
    })
}
fn compact(s: &mut State, now: u64, options: &Options) -> Result<()> {
    if s.disk.is_some() {
        // Disk::compact streams one value at a time and returns only the replacement keydir.
        let map = s
            .disk
            .as_mut()
            .ok_or(Error::Unhealthy)?
            .compact(&s.map, s.sequence, now, options)?;
        s.map = map;
    } else {
        s.map.retain(|_, i| i.live(now));
    }
    s.indexed_frame_bytes = frame_bytes(&s.map);
    Ok(())
}
fn prepare(s: &mut State, now: u64, options: &Options, additional: usize) -> Result<()> {
    if let Some(disk) = &s.disk {
        let live = s.indexed_frame_bytes;
        if disk.len + additional as u64 > options.max_log_bytes
            || (disk.len >= options.compact_after_bytes
                && disk.len > 2 * (live + format::HEADER_LEN))
        {
            compact(s, now, options)?;
        }
        if s.disk.as_ref().ok_or(Error::Unhealthy)?.len + additional as u64 > options.max_log_bytes
        {
            return Err(Error::Capacity("log bytes; delete or raise limit"));
        }
    }
    Ok(())
}
fn index_size(key: &[u8], index: &Index) -> u64 {
    match &index.value {
        Value::Disk { size, .. } => *size,
        Value::Memory(value) => (41 + key.len() + value.len()) as u64,
    }
}
fn frame_bytes(map: &HashMap<Vec<u8>, Index>) -> u64 {
    map.iter().map(|(key, index)| index_size(key, index)).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_exhaustion_does_not_mutate_or_wrap() {
        let db = Db::memory();
        db.put(b"k", b"old", None, Condition::Any).unwrap();
        db.inner.lock().unwrap().sequence = u64::MAX;
        assert!(matches!(
            db.put(b"k", b"new", None, Condition::Any),
            Err(Error::VersionExhausted)
        ));
        assert!(matches!(
            db.delete(b"k", DeleteCondition::Any),
            Err(Error::VersionExhausted)
        ));
        assert!(matches!(
            db.put(b"k", b"new", None, Condition::Version(99)),
            Err(Error::Conflict)
        ));
        assert!(!db.delete(b"missing", DeleteCondition::Any).unwrap());
        assert_eq!(db.get(b"k").unwrap().unwrap().value, b"old");
        assert_eq!(db.stats().unwrap().sequence, u64::MAX);
    }
}
