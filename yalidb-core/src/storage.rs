//! Generation replacement makes the publication boundary explicit: one manifest rename.
use crate::{
    Error, Index, Options, Result, Value,
    format::{self, HEADER_LEN},
};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions, TryLockError},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

#[cfg(not(unix))]
compile_error!(
    "The initial storage implementation supports Unix only; Windows durability is not validated."
);

pub(crate) struct Disk {
    dir: PathBuf,
    _lock: File,
    file: File,
    generation: u64,
    pub len: u64,
}

fn log_path(dir: &Path, generation: u64) -> PathBuf {
    dir.join(format!("{generation:016x}.log"))
}
fn regular(path: &Path) -> Result<()> {
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(format::corruption(
            path,
            0,
            "managed path must be a regular file, not a symlink",
        ));
    }
    Ok(())
}
fn create(path: &Path) -> Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    #[cfg(test)]
    simulation::record(simulation::Event::Create(simulation::name(path)));
    Ok(file)
}
fn sync(file: &File, _path: &Path) -> Result<()> {
    fault()?;
    file.sync_all()?;
    #[cfg(test)]
    simulation::record(simulation::Event::SyncFile(simulation::name(_path)));
    #[cfg(test)]
    if simulation::late_sync_error() {
        return Err(Error::Io(std::io::Error::other("injected late sync error")));
    }
    Ok(())
}
fn sync_dir(dir: &Path) -> Result<()> {
    fault()?;
    File::open(dir)?.sync_all()?;
    #[cfg(test)]
    simulation::sync_directory(dir);
    Ok(())
}
fn write(file: &mut File, b: &[u8], _path: &Path) -> Result<()> {
    fault()?;
    #[cfg(test)]
    let offset = file.stream_position()?;
    #[cfg(test)]
    if let Some(limit) = simulation::partial_write_limit() {
        let partial = &b[..limit.min(b.len())];
        file.write_all(partial)?;
        simulation::record(simulation::Event::Write(
            simulation::name(_path),
            offset,
            partial.to_vec(),
        ));
        return Err(Error::Io(std::io::Error::from_raw_os_error(28))); // ENOSPC on supported Unix.
    }
    file.write_all(b)?;
    #[cfg(test)]
    simulation::record(simulation::Event::Write(
        simulation::name(_path),
        offset,
        b.to_vec(),
    ));
    Ok(())
}
fn publish(dir: &Path, generation: u64) -> Result<()> {
    let tmp = dir.join("MANIFEST.tmp");
    let mut file = create(&tmp)?;
    write(&mut file, &format::manifest(generation), &tmp)?;
    sync(&file, &tmp)?;
    fault()?;
    fs::rename(&tmp, dir.join("MANIFEST"))?;
    #[cfg(test)]
    simulation::record(simulation::Event::Rename(
        "MANIFEST.tmp".into(),
        "MANIFEST".into(),
    ));
    sync_dir(dir)
}

type Recovered = (Disk, HashMap<Vec<u8>, Index>, u64);
impl Disk {
    pub fn open(dir: &Path, options: &Options) -> Result<Recovered> {
        if !dir.exists() {
            use std::os::unix::fs::DirBuilderExt;
            fs::DirBuilder::new().mode(0o700).create(dir)?;
        }
        if !fs::symlink_metadata(dir)?.is_dir() {
            return Err(Error::Invalid(
                "database path must be a directory, not a symlink",
            ));
        }
        // Ensure creation of the database directory itself is durable before acknowledgments.
        sync_dir(
            dir.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?;
        let lock_path = dir.join("LOCK");
        match fs::symlink_metadata(&lock_path) {
            Ok(_) => regular(&lock_path)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
        use std::os::unix::fs::OpenOptionsExt;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&lock_path)?;
        match lock.try_lock() {
            Ok(()) => (),
            Err(TryLockError::WouldBlock) => return Err(Error::Locked),
            Err(TryLockError::Error(e)) => return Err(e.into()),
        }
        let manifest = dir.join("MANIFEST");
        let generation = if manifest.exists() {
            regular(&manifest)?;
            let mut bytes = Vec::new();
            File::open(&manifest)?.take(21).read_to_end(&mut bytes)?;
            format::parse_manifest(&bytes, &manifest)?
        } else {
            // Never infer a manifest from an orphan log: it could be acknowledged data with a lost manifest.
            if fs::read_dir(dir)?.any(|e| e.map_or(true, |e| e.file_name() != "LOCK")) {
                return Err(format::corruption(
                    &manifest,
                    0,
                    "missing manifest in nonempty database; preserve files for investigation",
                ));
            }
            let mut first = create(&log_path(dir, 1))?;
            write(&mut first, &format::header(0, 0), &log_path(dir, 1))?;
            sync(&first, &log_path(dir, 1))?;
            sync_dir(dir)?;
            publish(dir, 1)?;
            1
        };
        let path = log_path(dir, generation);
        regular(&path)?;
        let mut file = OpenOptions::new().read(true).write(true).open(&path)?;
        let original_len = file.metadata()?.len();
        let Replay {
            map,
            sequence,
            valid_len: len,
        } = replay(&mut file, &path, original_len, options)?;
        if len < original_len {
            fault()?;
            file.set_len(len)?;
            #[cfg(test)]
            simulation::record(simulation::Event::Truncate(simulation::name(&path), len));
            sync(&file, &path)?;
        }
        // Recovered complete but unacknowledged frames must be durable before becoming visible.
        sync(&file, &path)?;
        let disk = Self {
            dir: dir.to_owned(),
            _lock: lock,
            file,
            generation,
            len,
        };
        disk.cleanup()?;
        Ok((disk, map, sequence))
    }

    pub fn append(
        &mut self,
        delete: bool,
        sequence: u64,
        expires: u64,
        key: &[u8],
        value: &[u8],
    ) -> Result<(u64, u64)> {
        let bytes = format::encode(delete, sequence, expires, key, value);
        self.file.seek(SeekFrom::Start(self.len))?;
        write(
            &mut self.file,
            &bytes,
            &log_path(&self.dir, self.generation),
        )?;
        sync(&self.file, &log_path(&self.dir, self.generation))?;
        let offset = self.len;
        self.len += bytes.len() as u64;
        Ok((offset, bytes.len() as u64))
    }

    pub fn read(
        &mut self,
        key: &[u8],
        sequence: u64,
        expires: u64,
        offset: u64,
        size: u64,
    ) -> Result<Vec<u8>> {
        let path = log_path(&self.dir, self.generation);
        let record = format::read_record(&mut self.file, &path, offset, self.len)?
            .ok_or_else(|| format::corruption(&path, offset, "indexed record is incomplete"))?;
        if record.delete
            || record.sequence != sequence
            || record.expires != expires
            || record.key != key
            || record.size != size
        {
            return Err(format::corruption(
                &path,
                offset,
                "record does not match index",
            ));
        }
        Ok(record.value)
    }

    pub fn compact(
        &mut self,
        map: &HashMap<Vec<u8>, Index>,
        sequence: u64,
        now: u64,
        options: &Options,
    ) -> Result<HashMap<Vec<u8>, Index>> {
        let mut live: Vec<_> = map.iter().filter(|(_, i)| i.live(now)).collect();
        live.sort_unstable_by_key(|(_, i)| i.version);
        let size = HEADER_LEN
            + live
                .iter()
                .map(|(_, i)| match i.value {
                    Value::Disk { size, .. } => size,
                    Value::Memory(_) => 0,
                })
                .sum::<u64>();
        if size > options.max_log_bytes {
            return Err(Error::Capacity("compacted snapshot exceeds log cap"));
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(Error::Capacity("generation exhausted"))?;
        let mut file = create(&log_path(&self.dir, generation))?;
        write(
            &mut file,
            &format::header(sequence, live.len() as u64),
            &log_path(&self.dir, generation),
        )?;
        let mut offset = HEADER_LEN;
        let mut next = HashMap::with_capacity(live.len());
        for (key, index) in live {
            let Value::Disk {
                offset: old_offset,
                size: old_size,
            } = index.value
            else {
                return Err(Error::Unhealthy);
            };
            let value = self.read(key, index.version, index.expires, old_offset, old_size)?;
            let bytes = format::encode(false, index.version, index.expires, key, &value);
            write(&mut file, &bytes, &log_path(&self.dir, generation))?;
            next.insert(
                key.clone(),
                Index {
                    version: index.version,
                    expires: index.expires,
                    value: Value::Disk {
                        offset,
                        size: bytes.len() as u64,
                    },
                },
            );
            offset += bytes.len() as u64;
        }
        sync(&file, &log_path(&self.dir, generation))?;
        sync_dir(&self.dir)?;
        publish(&self.dir, generation)?;
        // Manifest and its name are durable; switching memory can no longer expose old history.
        self.file = file;
        self.generation = generation;
        self.len = offset;
        self.cleanup()?;
        Ok(next)
    }

    fn cleanup(&self) -> Result<()> {
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let old_log = name.len() == 20
                && name.ends_with(".log")
                && name[..16].bytes().all(|c| c.is_ascii_hexdigit())
                && name != format!("{:016x}.log", self.generation);
            if old_log || name == "MANIFEST.tmp" {
                regular(&entry.path())?;
                fault()?;
                fs::remove_file(entry.path())?;
                #[cfg(test)]
                simulation::record(simulation::Event::Remove(name.to_owned()));
            }
        }
        sync_dir(&self.dir)
    }
}

// Unit tests can fail any persistence boundary on their own thread without exposing a production switch.
#[cfg(test)]
thread_local! {static FAIL_AFTER:std::cell::Cell<Option<usize>>=const {std::cell::Cell::new(None)};}
fn fault() -> Result<()> {
    #[cfg(test)]
    FAIL_AFTER.with(|counter| {
        if let Some(left) = counter.get() {
            if left == 0 {
                return Err(Error::Io(std::io::Error::other(
                    "injected persistence failure",
                )));
            }
            counter.set(Some(left - 1));
        }
        Ok(())
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Condition, Db};

    #[test]
    fn compaction_failure_boundaries_preserve_acknowledged_state() {
        // Includes output writes, flushes, publication, deletion and directory sync.
        for fail in 0..16 {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("db");
            let db = Db::open(&path).unwrap();
            db.put(b"a", b"old", None, Condition::Any).unwrap();
            let receipt = db.put(b"a", b"new", None, Condition::Any).unwrap();
            db.put(b"deleted", b"gone", None, Condition::Any).unwrap();
            db.delete(b"deleted", crate::DeleteCondition::Any).unwrap();
            FAIL_AFTER.with(|c| c.set(Some(fail)));
            let result = db.compact();
            FAIL_AFTER.with(|c| c.set(None));
            if result.is_err() {
                assert!(matches!(db.get(b"a"), Err(Error::Unhealthy)));
            }
            drop(db);
            let db = Db::open(&path).unwrap();
            let entry = db.get(b"a").unwrap().unwrap();
            assert_eq!(entry.value, b"new");
            assert_eq!(entry.version, receipt.version);
            assert!(db.get(b"deleted").unwrap().is_none());
            assert_eq!(db.stats().unwrap().sequence, 4);
        }
    }

    #[test]
    fn append_errors_poison_and_recover_with_unknown_outcome() {
        for fail in 0..2 {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("db");
            let db = Db::open(&path).unwrap();
            db.put(b"k", b"old", None, Condition::Any).unwrap();
            FAIL_AFTER.with(|c| c.set(Some(fail)));
            assert!(db.put(b"k", b"new", None, Condition::Any).is_err());
            FAIL_AFTER.with(|c| c.set(None));
            assert!(!db.stats().unwrap().healthy);
            assert!(matches!(db.get(b"k"), Err(Error::Unhealthy)));
            drop(db);
            let db = Db::open(&path).unwrap();
            assert_eq!(
                db.get(b"k").unwrap().unwrap().value,
                if fail == 0 { b"old" } else { b"new" }
            );
        }
    }
}

#[cfg(test)]
mod simulation;

/// Parsed selected generation; data synchronization and truncation are the owner's responsibility.
pub(crate) struct Replay {
    pub map: HashMap<Vec<u8>, Index>,
    pub sequence: u64,
    pub valid_len: u64,
}
pub(crate) fn replay<R: Read + Seek>(
    reader: &mut R,
    path: &Path,
    mut len: u64,
    options: &Options,
) -> Result<Replay> {
    reader.seek(SeekFrom::Start(0))?;
    if len < HEADER_LEN {
        return Err(format::corruption(path, 0, "truncated log header"));
    }
    if len > options.max_log_bytes {
        return Err(Error::Capacity("existing log exceeds max_log_bytes"));
    }
    let mut header = [0; HEADER_LEN as usize];
    reader.read_exact(&mut header)?;
    let (mut sequence, count) = format::parse_header(&header, path)?;
    if count > options.max_keys as u64 {
        return Err(Error::Capacity("snapshot exceeds max_keys"));
    }
    let mut map = HashMap::new();
    let mut offset = HEADER_LEN;
    let mut previous_snapshot = 0;
    let mut seen = 0u64;
    while offset < len {
        let Some(record) = format::read_record(reader, path, offset, len)? else {
            if seen < count {
                return Err(format::corruption(path, offset, "incomplete snapshot"));
            }
            len = offset;
            break;
        };
        if seen < count {
            if record.delete
                || record.sequence <= previous_snapshot
                || record.sequence > sequence
                || map.contains_key(&record.key)
            {
                return Err(format::corruption(
                    path,
                    offset,
                    "invalid snapshot order or duplicate",
                ));
            }
            previous_snapshot = record.sequence;
        } else {
            if sequence.checked_add(1) != Some(record.sequence) {
                return Err(format::corruption(
                    path,
                    offset,
                    "noncontiguous mutation sequence",
                ));
            }
            sequence = record.sequence;
        }
        if record.delete {
            map.remove(&record.key);
        } else {
            if !map.contains_key(&record.key) && map.len() >= options.max_keys {
                return Err(Error::Capacity("recovery exceeds max_keys"));
            }
            map.insert(
                record.key,
                Index {
                    version: record.sequence,
                    expires: record.expires,
                    value: Value::Disk {
                        offset,
                        size: record.size,
                    },
                },
            );
        }
        offset += record.size;
        seen += 1;
    }
    if seen < count {
        return Err(format::corruption(path, offset, "missing snapshot records"));
    }
    Ok(Replay {
        map,
        sequence,
        valid_len: len,
    })
}
