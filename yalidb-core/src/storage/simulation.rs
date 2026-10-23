//! Test-only crash-image model driven by the real engine's persistence trace.
//! File data and directory names have independent durability. This is not a device emulator.
use crate::{Clock, Condition, Db, DeleteCondition, Entry, Error, Options, Result};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Expected {
    entries: BTreeMap<Vec<u8>, Entry>,
    sequence: u64,
}
impl Expected {
    fn read(db: &Db) -> Self {
        Self {
            entries: db
                .scan_prefix(b"", None, 100)
                .unwrap()
                .entries
                .into_iter()
                .collect(),
            sequence: db.stats().unwrap().sequence,
        }
    }
}
#[derive(Clone, Debug)]
pub(super) enum Event {
    Create(String),
    Write(String, u64, Vec<u8>),
    Truncate(String, u64),
    SyncFile(String),
    SyncDir,
    Rename(String, String),
    Remove(String),
    Begin(Expected),
    Ack(Expected),
}
thread_local! {
    static TRACE:RefCell<Option<(PathBuf,Vec<Event>)>>=const {RefCell::new(None)};
    static PARTIAL:Cell<Option<usize>>=const {Cell::new(None)};
    static LATE_SYNC:Cell<bool>=const {Cell::new(false)};
}
pub(super) fn name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}
pub(super) fn record(event: Event) {
    TRACE.with_borrow_mut(|trace| {
        if let Some((_, events)) = trace {
            events.push(event);
        }
    });
}
pub(super) fn sync_directory(path: &Path) {
    TRACE.with_borrow_mut(|trace| {
        if let Some((root, events)) = trace
            && root == path
        {
            events.push(Event::SyncDir);
        }
    });
}
pub(super) fn partial_write_limit() -> Option<usize> {
    PARTIAL.with(|v| v.take())
}
pub(super) fn late_sync_error() -> bool {
    LATE_SYNC.with(|v| v.replace(false))
}
fn capture(path: &Path) {
    TRACE.with_borrow_mut(|t| *t = Some((path.to_owned(), Vec::new())));
}
fn finish() -> Vec<Event> {
    TRACE.with_borrow_mut(|t| t.take().unwrap().1)
}

#[derive(Clone, Default)]
struct Inode {
    stable: Vec<u8>,
    cache: Vec<u8>,
    pending: Vec<Change>,
}
#[derive(Clone)]
enum Change {
    Write(usize, Vec<u8>),
    Truncate(usize),
}
#[derive(Clone)]
enum NameChange {
    Insert(String, u64),
    Rename(String, String, u64),
    Remove(String),
}
type Names = BTreeMap<String, u64>;
type Image = BTreeMap<String, Vec<u8>>;
struct Model {
    files: BTreeMap<u64, Inode>,
    names: Names,
    durable_names: Names,
    pending_names: Vec<NameChange>,
    next: u64,
    acked: Expected,
    pending: Option<Expected>,
    initialized: bool,
    now: u64,
}
impl Model {
    fn new(image: &Image, expected: Expected) -> Self {
        let mut model = Self {
            files: BTreeMap::new(),
            names: Names::new(),
            durable_names: Names::new(),
            pending_names: Vec::new(),
            next: 0,
            acked: expected,
            pending: None,
            initialized: image.contains_key("MANIFEST"),
            now: 1000,
        };
        for (name, bytes) in image {
            model.next += 1;
            model.names.insert(name.clone(), model.next);
            model.files.insert(
                model.next,
                Inode {
                    stable: bytes.clone(),
                    cache: bytes.clone(),
                    pending: Vec::new(),
                },
            );
        }
        model.durable_names = model.names.clone();
        model
    }
    fn apply(&mut self, event: &Event) {
        match event {
            Event::Create(name) => {
                self.next += 1;
                self.files.insert(self.next, Inode::default());
                self.names.insert(name.clone(), self.next);
                self.pending_names
                    .push(NameChange::Insert(name.clone(), self.next));
            }
            Event::Write(name, offset, bytes) => {
                let inode = self.files.get_mut(&self.names[name]).unwrap();
                apply_write(&mut inode.cache, *offset as usize, bytes);
                inode
                    .pending
                    .push(Change::Write(*offset as usize, bytes.clone()));
            }
            Event::Truncate(name, len) => {
                let inode = self.files.get_mut(&self.names[name]).unwrap();
                inode.cache.resize(*len as usize, 0);
                inode.pending.push(Change::Truncate(*len as usize));
            }
            Event::SyncFile(name) => {
                let inode = self.files.get_mut(&self.names[name]).unwrap();
                inode.stable = inode.cache.clone();
                inode.pending.clear();
            }
            Event::SyncDir => {
                self.durable_names = self.names.clone();
                self.pending_names.clear();
            }
            Event::Rename(from, to) => {
                let id = self.names.remove(from).unwrap();
                self.names.insert(to.clone(), id);
                self.pending_names
                    .push(NameChange::Rename(from.clone(), to.clone(), id));
            }
            Event::Remove(name) => {
                self.names.remove(name);
                self.pending_names.push(NameChange::Remove(name.clone()));
            }
            Event::Begin(expected) => self.pending = Some(expected.clone()),
            Event::Ack(expected) => {
                self.acked = expected.clone();
                self.pending = None;
                self.initialized = true;
            }
        }
    }
    fn names_after_crash(&self) -> Vec<Names> {
        // Namespace operations are atomic, but any subset after the last directory barrier may survive.
        // A persisted rename carries its inode even when the previous temporary name wasn't persisted.
        assert!(self.pending_names.len() < 10);
        let mut variants = Vec::new();
        for mask in 0..1usize << self.pending_names.len() {
            let mut names = self.durable_names.clone();
            for (n, change) in self.pending_names.iter().enumerate() {
                if mask & (1 << n) == 0 {
                    continue;
                }
                match change {
                    NameChange::Insert(name, id) => {
                        names.insert(name.clone(), *id);
                    }
                    NameChange::Rename(from, to, id) => {
                        names.remove(from);
                        names.insert(to.clone(), *id);
                    }
                    NameChange::Remove(name) => {
                        names.remove(name);
                    }
                }
            }
            variants.push(names);
        }
        variants
    }
    fn image(&self, names: &Names, mode: Mode) -> Image {
        names
            .iter()
            .map(|(name, id)| (name.clone(), self.files[id].crash_bytes(mode)))
            .collect()
    }
    fn allowed(&self, actual: &Expected) -> bool {
        actual == &self.acked || self.pending.as_ref() == Some(actual)
    }
}
fn apply_write(bytes: &mut Vec<u8>, offset: usize, value: &[u8]) {
    bytes.resize(bytes.len().max(offset + value.len()), 0);
    bytes[offset..offset + value.len()].copy_from_slice(value);
}
#[derive(Clone, Copy, Debug)]
enum Mode {
    Drop,
    All,
    Prefix(usize),
    Reordered(u64),
}
impl Inode {
    fn crash_bytes(&self, mode: Mode) -> Vec<u8> {
        let mut bytes = self.stable.clone();
        match mode {
            Mode::Drop => (),
            Mode::All => return self.cache.clone(),
            Mode::Prefix(mut remaining) => {
                for change in &self.pending {
                    match change {
                        Change::Write(offset, value) => {
                            let n = remaining.min(value.len());
                            apply_write(&mut bytes, *offset, &value[..n]);
                            remaining -= n;
                            if n < value.len() {
                                break;
                            }
                        }
                        Change::Truncate(len) => bytes.resize(*len, 0),
                    }
                }
            }
            Mode::Reordered(mut seed) => {
                for change in self.pending.iter().rev() {
                    match change {
                        Change::Write(offset, value) => {
                            for (chunk, part) in value.chunks(8).enumerate().rev() {
                                seed ^= seed << 13;
                                seed ^= seed >> 7;
                                seed ^= seed << 17;
                                if seed & 1 == 1 {
                                    apply_write(&mut bytes, offset + chunk * 8, part);
                                }
                            }
                        }
                        Change::Truncate(len) => {
                            if seed & 1 == 1 {
                                bytes.resize(*len, 0);
                            }
                        }
                    }
                }
            }
        }
        bytes
    }
}
struct FixedClock(u64);
impl Clock for FixedClock {
    fn now_ms(&self) -> Result<u64> {
        Ok(self.0)
    }
}
fn snapshot(path: &Path) -> Image {
    fs::read_dir(path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (name(&entry.path()), fs::read(entry.path()).unwrap())
        })
        .collect()
}
fn reopen_image(image: &Image, model: &Model, mode: Mode) -> bool {
    let dir = tempfile::tempdir().unwrap();
    for (name, bytes) in image {
        fs::write(dir.path().join(name), bytes).unwrap();
    }
    match Db::open_with(
        dir.path(),
        Options::default(),
        Arc::new(FixedClock(model.now)),
    ) {
        Ok(db) => model.allowed(&Expected::read(&db)),
        Err(_) if !model.initialized && model.acked.sequence == 0 => true,
        // Reordered unsynced append bytes may deliberately require fail-closed manual salvage.
        Err(Error::Corruption { reason, .. }) if matches!(mode, Mode::Reordered(_)) => {
            matches!(reason, "invalid frame header" | "record checksum mismatch")
        }
        Err(_) => false,
    }
}

fn check_trace(image: &Image, baseline: Expected, trace: &[Event], expect_success: bool) -> usize {
    check_trace_at_time(image, baseline, trace, expect_success, 1000)
}
fn check_trace_at_time(
    image: &Image,
    baseline: Expected,
    trace: &[Event],
    expect_success: bool,
    now: u64,
) -> usize {
    let mut model = Model::new(image, baseline);
    model.now = now;
    let mut seen = BTreeSet::new();
    let mut checked = 0;
    for (step, event) in trace.iter().enumerate() {
        model.apply(event);
        for names in model.names_after_crash() {
            for mode in [
                Mode::Drop,
                Mode::All,
                Mode::Prefix(1),
                Mode::Prefix(7),
                Mode::Prefix(12),
                Mode::Prefix(40),
                Mode::Reordered(42),
                Mode::Reordered(123),
            ] {
                let image = model.image(&names, mode);
                let identity = format!("{image:?}{:?}{:?}{mode:?}", model.acked, model.pending);
                if !seen.insert(identity) {
                    continue;
                }
                checked += 1;
                let valid = reopen_image(&image, &model, mode);
                if !expect_success && !valid {
                    return checked;
                }
                assert!(
                    valid,
                    "crash image violated acknowledged-prefix recovery at step {step}: {event:?}, mode {mode:?}"
                );
            }
        }
    }
    assert!(
        expect_success,
        "negative control failed to detect a missing persistence barrier"
    );
    checked
}
fn begin_put(expected: &Expected, key: &[u8], value: &[u8], expires: Option<u64>) -> Expected {
    let mut next = expected.clone();
    next.sequence += 1;
    next.entries.insert(
        key.to_vec(),
        Entry {
            value: value.to_vec(),
            version: next.sequence,
            expires_at_ms: expires,
        },
    );
    record(Event::Begin(next.clone()));
    next
}

#[test]
fn power_loss_at_every_append_and_compaction_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open_with(dir.path(), Options::default(), Arc::new(FixedClock(1000))).unwrap();
    db.put(b"anchor", b"old", None, Condition::Any).unwrap();
    let image = snapshot(dir.path());
    let baseline = Expected::read(&db);
    let mut expected = baseline.clone();
    capture(dir.path());
    for (key, value, ttl) in [
        (b"anchor".as_slice(), b"new".as_slice(), None),
        (b"deleted", b"temporary", None),
        (b"ttl", b"expires", Some(100)),
    ] {
        expected = begin_put(&expected, key, value, ttl.map(|ttl| 1000 + ttl));
        db.put(key, value, ttl, Condition::Any).unwrap();
        record(Event::Ack(expected.clone()));
    }
    expected.entries.remove(b"deleted".as_slice());
    expected.sequence += 1;
    record(Event::Begin(expected.clone()));
    db.delete(b"deleted", DeleteCondition::Any).unwrap();
    record(Event::Ack(expected.clone()));
    db.compact().unwrap();
    expected = begin_put(&expected, b"after", b"survives", None);
    db.put(b"after", b"survives", None, Condition::Any).unwrap();
    record(Event::Ack(expected));
    let trace = finish();
    drop(db);
    let count = check_trace(&image, baseline.clone(), &trace, true);
    assert!(count > 100);
    eprintln!("power-loss model validated {count} distinct crash cases");
    // Negative controls: the harness must catch broken implementations of both barriers.
    let missing_file_sync: Vec<_> = trace
        .iter()
        .filter(|event| !matches!(event,Event::SyncFile(name) if name=="0000000000000002.log"))
        .cloned()
        .collect();
    check_trace(&image, baseline.clone(), &missing_file_sync, false);
    let mut missing_append_sync = trace.clone();
    let sync = missing_append_sync
        .iter()
        .position(|event| matches!(event,Event::SyncFile(name) if name=="0000000000000001.log"))
        .unwrap();
    missing_append_sync.remove(sync);
    check_trace(&image, baseline.clone(), &missing_append_sync, false);
    let mut missing_publish_barrier = trace.clone();
    let rename = missing_publish_barrier
        .iter()
        .position(|e| matches!(e, Event::Rename(..)))
        .unwrap();
    assert!(matches!(
        missing_publish_barrier[rename + 1],
        Event::SyncDir
    ));
    missing_publish_barrier.remove(rename + 1);
    check_trace(&image, baseline, &missing_publish_barrier, false);
}

#[test]
fn short_enospc_writes_and_compound_recovery_failure() {
    for cut in [0, 1, 7, 12, 20, 40, 80] {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_with(dir.path(), Options::default(), Arc::new(FixedClock(1000))).unwrap();
        db.put(b"anchor", b"old", None, Condition::Any).unwrap();
        let image = snapshot(dir.path());
        let baseline = Expected::read(&db);
        capture(dir.path());
        begin_put(&baseline, b"anchor", &[1; 128], None);
        PARTIAL.with(|v| v.set(Some(cut)));
        assert!(matches!(
            db.put(b"anchor", &[1; 128], None, Condition::Any),
            Err(Error::Io(_))
        ));
        assert!(!db.stats().unwrap().healthy);
        drop(db);
        // Opening a torn tail may itself fail during the truncation sync. Reopen again afterwards.
        LATE_SYNC.with(|v| v.set(true));
        assert!(Db::open_with(dir.path(), Options::default(), Arc::new(FixedClock(1000))).is_err());
        let trace = finish();
        check_trace(&image, baseline, &trace, true);
        let db = Db::open_with(dir.path(), Options::default(), Arc::new(FixedClock(1000))).unwrap();
        assert_eq!(db.get(b"anchor").unwrap().unwrap().value, b"old");
    }
}

#[test]
fn delayed_sync_error_can_recover_a_complete_unacknowledged_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open_with(dir.path(), Options::default(), Arc::new(FixedClock(1000))).unwrap();
    db.put(b"anchor", b"old", None, Condition::Any).unwrap();
    let image = snapshot(dir.path());
    let baseline = Expected::read(&db);
    capture(dir.path());
    begin_put(&baseline, b"anchor", b"new", None);
    LATE_SYNC.with(|v| v.set(true));
    assert!(matches!(
        db.put(b"anchor", b"new", None, Condition::Any),
        Err(Error::Io(_))
    ));
    let trace = finish();
    assert!(!db.stats().unwrap().healthy);
    drop(db);
    check_trace(&image, baseline, &trace, true);
    let db = Db::open_with(dir.path(), Options::default(), Arc::new(FixedClock(1000))).unwrap();
    assert_eq!(db.get(b"anchor").unwrap().unwrap().value, b"new");
}

#[test]
fn first_manifest_publication_is_crash_safe_before_any_mutation_ack() {
    let dir = tempfile::tempdir().unwrap();
    let image = Image::new();
    let baseline = Expected {
        entries: BTreeMap::new(),
        sequence: 0,
    };
    capture(dir.path());
    let db = Db::open_with(dir.path(), Options::default(), Arc::new(FixedClock(1000))).unwrap();
    record(Event::Ack(baseline.clone()));
    let next = begin_put(&baseline, b"first", b"value", None);
    db.put(b"first", b"value", None, Condition::Any).unwrap();
    record(Event::Ack(next));
    let trace = finish();
    drop(db);
    let count = check_trace(&image, baseline, &trace, true);
    eprintln!("initialization model validated {count} distinct crash cases");
}

#[test]
fn expiry_purge_and_empty_snapshot_preserve_high_water_mark() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open_with(dir.path(), Options::default(), Arc::new(FixedClock(1000))).unwrap();
    db.put(b"expired", b"data", Some(1), Condition::Any)
        .unwrap();
    drop(db);
    let db = Db::open_with(dir.path(), Options::default(), Arc::new(FixedClock(1100))).unwrap();
    let image = snapshot(dir.path());
    let baseline = Expected::read(&db);
    assert_eq!(baseline.sequence, 1);
    assert!(baseline.entries.is_empty());
    capture(dir.path());
    assert_eq!(db.purge_expired().unwrap(), 1);
    db.compact().unwrap();
    let next = begin_put(&baseline, b"new", b"next-sequence", None);
    db.put(b"new", b"next-sequence", None, Condition::Any)
        .unwrap();
    record(Event::Ack(next));
    let trace = finish();
    drop(db);
    let count = check_trace_at_time(&image, baseline, &trace, true, 1100);
    eprintln!("empty-snapshot model validated {count} distinct crash cases");
}
