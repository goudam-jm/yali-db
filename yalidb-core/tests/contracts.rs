use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{Seek, SeekFrom, Write},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
};
use yalidb_core::{
    Clock, Condition as C, Db, DeleteCondition as D, Error, MAX_KEY_BYTES, MAX_VALUE_BYTES,
    Options, Result,
};

struct FakeClock(AtomicU64);
impl Clock for FakeClock {
    fn now_ms(&self) -> Result<u64> {
        Ok(self.0.load(Ordering::SeqCst))
    }
}
fn clock(time: u64) -> Arc<FakeClock> {
    Arc::new(FakeClock(AtomicU64::new(time)))
}
fn log(path: &std::path::Path) -> std::path::PathBuf {
    fs::read_dir(path)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|x| x == "log"))
        .unwrap()
}

#[test]
fn bytes_validation_owned_values_and_versions() {
    let db = Db::memory();
    assert!(db.get(b"key").unwrap().is_none());
    let r = db.put(b"\0\xff", b"", None, C::Absent).unwrap();
    assert_eq!(r.version, 1);
    let mut e = db.get(b"\0\xff").unwrap().unwrap();
    e.value.push(99);
    assert!(db.get(b"\0\xff").unwrap().unwrap().value.is_empty());
    assert!(matches!(
        db.put(b"\0\xff", b"new", None, C::Absent),
        Err(Error::Conflict)
    ));
    assert!(matches!(
        db.put(b"\0\xff", b"new", None, C::Version(99)),
        Err(Error::Conflict)
    ));
    assert_eq!(db.stats().unwrap().sequence, 1);
    assert!(db.delete(b"\0\xff", D::Version(1)).unwrap());
    assert!(!db.delete(b"\0\xff", D::Any).unwrap());
    assert!(matches!(
        db.delete(b"\0\xff", D::Version(1)),
        Err(Error::Conflict)
    ));
    assert_eq!(
        db.put(b"\0\xff", b"again", None, C::Absent)
            .unwrap()
            .version,
        3
    );
    assert!(db.get(b"").is_err());
    assert!(db.delete(b"", D::Any).is_err());
    assert!(db.put(b"", b"v", None, C::Any).is_err());
    assert!(
        db.put(&vec![0; MAX_KEY_BYTES + 1], b"v", None, C::Any)
            .is_err()
    );
    assert!(
        db.put(b"\0\xff", &vec![0; MAX_VALUE_BYTES + 1], None, C::Any)
            .is_err()
    );
    db.put(
        &vec![1; MAX_KEY_BYTES],
        &vec![2; MAX_VALUE_BYTES],
        None,
        C::Any,
    )
    .unwrap();
    assert_eq!(db.get(b"\0\xff").unwrap().unwrap().value, b"again");
}

#[test]
fn deterministic_expiry_and_clock_rollback() {
    let time = clock(100);
    let db = Db::memory_with(Options::default(), time.clone()).unwrap();
    db.put(b"k", b"v", Some(10), C::Any).unwrap();
    time.0.store(109, Ordering::SeqCst);
    assert!(db.get(b"k").unwrap().is_some());
    time.0.store(110, Ordering::SeqCst);
    assert!(db.get(b"k").unwrap().is_none());
    time.0.store(50, Ordering::SeqCst);
    assert!(db.get(b"k").unwrap().is_none());
    assert!(db.scan_prefix(b"", None, 10).unwrap().entries.is_empty());
    assert!(!db.delete(b"k", D::Any).unwrap());
    assert!(matches!(
        db.put(b"k", b"v", None, C::Version(1)),
        Err(Error::Conflict)
    ));
    let r = db.put(b"k", b"forever", None, C::Absent).unwrap();
    assert_eq!(r.version, 2);
    assert_eq!(r.expires_at_ms, None);
    assert!(db.put(b"k", b"bad", Some(0), C::Any).is_err());
    time.0.store(u64::MAX, Ordering::SeqCst);
    assert!(matches!(
        db.put(b"k", b"bad", Some(1), C::Any),
        Err(Error::Clock)
    ));
    assert_eq!(db.get(b"k").unwrap().unwrap().value, b"forever");
    assert_eq!(db.stats().unwrap().sequence, 2);
}

#[test]
fn key_capacity_purge_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let time = clock(100);
    let options = Options {
        max_keys: 1,
        ..Options::default()
    };
    let db = Db::open_with(&path, options.clone(), time.clone()).unwrap();
    db.put(b"a", b"v", Some(1), C::Any).unwrap();
    time.0.store(101, Ordering::SeqCst);
    assert!(matches!(
        db.put(b"b", b"v", None, C::Any),
        Err(Error::Capacity(_))
    ));
    assert_eq!(db.purge_expired().unwrap(), 1);
    db.put(b"b", b"v", None, C::Any).unwrap();
    drop(db);
    let db = Db::open_with(&path, options, time).unwrap();
    assert_eq!(db.get(b"b").unwrap().unwrap().version, 2);
    assert!(db.get(b"a").unwrap().is_none());
}

#[test]
fn scan_pages_byte_budget_and_binary_order() {
    let db = Db::memory();
    for n in (0..40u8).rev() {
        db.put(&[0, n], &vec![n; MAX_VALUE_BYTES], None, C::Any)
            .unwrap();
    }
    db.put(b"other", b"v", None, C::Any).unwrap();
    let mut all = Vec::new();
    let mut after = None;
    loop {
        let page = db.scan_prefix(&[0], after.as_deref(), 100).unwrap();
        assert!(
            page.entries
                .iter()
                .map(|(k, v)| k.len() + v.value.len())
                .sum::<usize>()
                <= 1024 * 1024
        );
        all.extend(page.entries.into_iter().map(|(k, _)| k));
        after = page.next_key;
        if after.is_none() {
            break;
        }
    }
    assert_eq!(all, (0..40u8).map(|n| vec![0, n]).collect::<Vec<_>>());
    let page = db.scan_prefix(&[0], Some(&[0, 5]), 2).unwrap();
    assert_eq!(page.entries[0].0, vec![0, 6]);
    assert_eq!(page.next_key, Some(vec![0, 7]));
    let page = db.scan_prefix(&[0], Some(&[0, 39]), 100).unwrap();
    assert!(page.entries.is_empty());
    assert!(page.next_key.is_none());
    assert!(db.scan_prefix(b"", None, 0).is_err());
    assert!(db.scan_prefix(b"", None, 101).is_err());
    assert!(db.scan_prefix(b"", Some(b""), 1).is_err());
}

#[test]
fn durable_reopen_compaction_and_offline_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let time = clock(100);
    let db = Db::open_with(&path, Options::default(), time.clone()).unwrap();
    db.put(b"k", b"old", None, C::Any).unwrap();
    let version = db.put(b"k", b"new", None, C::Any).unwrap().version;
    db.put(b"expired", b"e", Some(1), C::Any).unwrap();
    db.put(b"deleted", b"d", None, C::Any).unwrap();
    db.delete(b"deleted", D::Any).unwrap();
    time.0.store(101, Ordering::SeqCst);
    db.compact().unwrap();
    drop(db);
    let backup = dir.path().join("backup");
    fs::create_dir(&backup).unwrap();
    for file in fs::read_dir(&path).unwrap() {
        let file = file.unwrap();
        fs::copy(file.path(), backup.join(file.file_name())).unwrap();
    }
    let db = Db::open_with(&backup, Options::default(), time).unwrap();
    let e = db.get(b"k").unwrap().unwrap();
    assert_eq!(e.value, b"new");
    assert_eq!(e.version, version);
    assert!(db.get(b"expired").unwrap().is_none());
    assert!(db.get(b"deleted").unwrap().is_none());
    assert_eq!(
        db.put(b"deleted", b"reborn", None, C::Any).unwrap().version,
        6
    );
}

#[test]
fn lock_lifetime_is_shared_by_clones() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let db = Db::open(&path).unwrap();
    let clone = db.clone();
    drop(db);
    assert!(matches!(Db::open(&path), Err(Error::Locked)));
    drop(clone);
    assert!(Db::open(&path).is_ok());
}

#[test]
fn incomplete_tail_is_truncated_but_complete_corruption_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let db = Db::open(&path).unwrap();
    db.put(b"k", b"acknowledged", None, C::Any).unwrap();
    drop(db);
    let file = log(&path);
    let bytes = fs::read(&file).unwrap();
    for cut in 1..bytes.len() - 28 {
        let mut torn = bytes.clone();
        torn.extend_from_slice(&bytes[28..28 + cut]);
        fs::write(&file, torn).unwrap();
        let db = Db::open(&path).unwrap();
        assert_eq!(db.get(b"k").unwrap().unwrap().value, b"acknowledged");
        drop(db);
        assert_eq!(fs::metadata(&file).unwrap().len(), bytes.len() as u64);
    }
    let mut bad = bytes.clone();
    let last = bad.len() - 1;
    bad[last] ^= 1;
    fs::write(&file, bad).unwrap();
    assert!(matches!(Db::open(&path), Err(Error::Corruption { .. })));
    assert_eq!(fs::metadata(file).unwrap().len(), bytes.len() as u64);
}

#[test]
fn read_time_corruption_marks_owner_unhealthy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let db = Db::open(&path).unwrap();
    db.put(b"k", b"v", None, C::Any).unwrap();
    let mut file = OpenOptions::new().write(true).open(log(&path)).unwrap();
    file.seek(SeekFrom::End(-1)).unwrap();
    file.write_all(&[0]).unwrap();
    file.sync_all().unwrap();
    assert!(matches!(db.get(b"k"), Err(Error::Corruption { .. })));
    assert!(matches!(
        db.put(b"x", b"v", None, C::Any),
        Err(Error::Unhealthy)
    ));
}

#[test]
fn truncated_compaction_snapshot_is_not_silently_salvaged() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let db = Db::open(&path).unwrap();
    db.put(b"k", b"v", None, C::Any).unwrap();
    db.compact().unwrap();
    drop(db);
    let path = log(&path);
    let file = OpenOptions::new().write(true).open(&path).unwrap();
    file.set_len(29).unwrap();
    assert!(matches!(
        Db::open(dir.path().join("db")),
        Err(Error::Corruption { .. })
    ));
}

#[test]
fn bounded_log_automatically_compacts_hot_key_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let options = Options {
        max_log_bytes: 80_000,
        compact_after_bytes: 1000,
        ..Options::default()
    };
    let db = Db::open_with(&path, options, clock(100)).unwrap();
    for n in 0..500 {
        db.put(b"hot", &[n as u8; 1024], None, C::Any).unwrap();
    }
    assert!(db.stats().unwrap().log_bytes < 10_000);
    drop(db);
    let db = Db::open(&path).unwrap();
    assert_eq!(db.get(b"hot").unwrap().unwrap().version, 500);
}

#[test]
fn cloned_handles_cas_serializes_stale_client_updates() {
    let db = Db::memory();
    db.put(b"counter", &0u64.to_le_bytes(), None, C::Any)
        .unwrap();
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let db = db.clone();
            thread::spawn(move || {
                for _ in 0..100 {
                    loop {
                        let entry = db.get(b"counter").unwrap().unwrap();
                        let value = u64::from_le_bytes(entry.value.try_into().unwrap());
                        match db.put(
                            b"counter",
                            &(value + 1).to_le_bytes(),
                            None,
                            C::Version(entry.version),
                        ) {
                            Ok(_) => break,
                            Err(Error::Conflict) => continue,
                            Err(e) => panic!("{e}"),
                        }
                    }
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(
        u64::from_le_bytes(
            db.get(b"counter")
                .unwrap()
                .unwrap()
                .value
                .try_into()
                .unwrap()
        ),
        400
    );
    assert_eq!(db.stats().unwrap().sequence, 401);
}

#[test]
fn seeded_reference_model_with_reopen_and_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let mut db = Db::open(&path).unwrap();
    let mut expected = BTreeMap::new();
    let mut seed = 123456u64;
    let mut sequence = 0;
    for step in 0..600 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let key = vec![(seed % 32) as u8];
        match seed % 4 {
            0 => {
                if expected.remove(&key).is_some() {
                    sequence += 1;
                    assert!(db.delete(&key, D::Any).unwrap());
                } else {
                    assert!(!db.delete(&key, D::Any).unwrap());
                }
            }
            1 | 2 => {
                sequence += 1;
                let value = seed.to_le_bytes().to_vec();
                let r = db.put(&key, &value, None, C::Any).unwrap();
                assert_eq!(r.version, sequence);
                expected.insert(key, (value, sequence));
            }
            _ => {
                let actual = db.get(&key).unwrap().map(|e| (e.value, e.version));
                assert_eq!(actual, expected.get(&key).cloned());
            }
        }
        if step % 50 == 0 {
            db.compact().unwrap();
            drop(db);
            db = Db::open(&path).unwrap();
        }
    }
    let actual = db
        .scan_prefix(b"", None, 100)
        .unwrap()
        .entries
        .into_iter()
        .map(|(k, e)| (k, (e.value, e.version)))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(actual, expected);
    assert_eq!(db.stats().unwrap().sequence, sequence);
}

#[test]
fn manifest_loss_is_not_reinitialized_and_orphans_are_not_adopted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let db = Db::open(&path).unwrap();
    db.put(b"k", b"v", None, C::Any).unwrap();
    drop(db);
    fs::write(path.join("0000000000000099.log"), b"not a valid generation").unwrap();
    let db = Db::open(&path).unwrap();
    assert!(db.get(b"k").unwrap().is_some());
    drop(db);
    assert!(!path.join("0000000000000099.log").exists());
    fs::remove_file(path.join("MANIFEST")).unwrap();
    assert!(matches!(Db::open(&path), Err(Error::Corruption { .. })));
}

#[test]
fn independently_encoded_v1_fixture_can_be_read_and_extended() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(
        dir.path().join("MANIFEST"),
        include_bytes!("fixtures/v1-manifest.bin"),
    )
    .unwrap();
    fs::write(
        dir.path().join("0000000000000001.log"),
        include_bytes!("fixtures/v1-log.bin"),
    )
    .unwrap();
    let db = Db::open(dir.path()).unwrap();
    let entry = db.get(b"fixture").unwrap().unwrap();
    assert_eq!(entry.value, b"known-value");
    assert_eq!(entry.version, 1);
    assert_eq!(db.put(b"second", b"", None, C::Any).unwrap().version, 2);
}

#[test]
fn log_capacity_reserves_room_to_delete_and_then_resume_writing() {
    let dir = tempfile::tempdir().unwrap();
    let options = Options {
        max_log_bytes: 70_000,
        compact_after_bytes: 69_000,
        ..Options::default()
    };
    let db = Db::open_with(dir.path(), options, clock(1)).unwrap();
    db.put(b"full", &vec![0; 65_536], None, C::Any).unwrap();
    assert!(matches!(
        db.put(b"too-much", &vec![0; 5000], None, C::Any),
        Err(Error::Capacity(_))
    ));
    assert!(db.delete(b"full", D::Any).unwrap());
    db.put(b"new", &vec![0; 65_536], None, C::Any).unwrap();
    assert!(db.get(b"full").unwrap().is_none());
    assert_eq!(db.get(b"new").unwrap().unwrap().version, 3);
}

#[test]
fn dangling_lock_symlink_is_rejected_without_creating_its_target() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    fs::create_dir(&path).unwrap();
    let outside = dir.path().join("must-not-create");
    symlink(&outside, path.join("LOCK")).unwrap();
    assert!(matches!(Db::open(&path), Err(Error::Corruption { .. })));
    assert!(!outside.exists());
}
