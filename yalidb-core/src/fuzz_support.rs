//! Bounded entry points for cargo-fuzz; enabled only by the opt-in `fuzzing` feature.
use crate::{
    Condition, Db, DeleteCondition, Entry, Error, Options, SystemClock, Value, format, storage,
};
use std::{collections::BTreeMap, io::Cursor, path::Path, sync::Arc};

const MAX_INPUT: usize = 131_072;

/// Exercise real decoders on raw and checksum-aware input. No filesystem access.
pub fn decode(data: &[u8]) {
    if data.len() > MAX_INPUT {
        return;
    }
    let path = Path::new("fuzz");
    let _ = format::parse_manifest(data, path);
    let _ = format::parse_header(data, path);
    let _ = format::read_record(&mut Cursor::new(data), path, 0, data.len() as u64);
    let mut repaired = data.to_vec();
    if repaired.len() == 20 {
        let crc = format::checksum(&repaired[..16]);
        repaired[16..20].copy_from_slice(&crc.to_le_bytes());
        let _ = format::parse_manifest(&repaired, path);
    }
    if repaired.len() == 28 {
        let crc = format::checksum(&repaired[..24]);
        repaired[24..28].copy_from_slice(&crc.to_le_bytes());
        let _ = format::parse_header(&repaired, path);
    }
    repair_frames(&mut repaired, 0);
    let _ = format::read_record(&mut Cursor::new(&repaired), path, 0, repaired.len() as u64);
}

fn repair_frames(bytes: &mut [u8], mut offset: usize) {
    while bytes.len().saturating_sub(offset) >= 12 {
        let crc = format::checksum(&bytes[offset..offset + 8]);
        bytes[offset + 8..offset + 12].copy_from_slice(&crc.to_le_bytes());
        let mut len = [0; 4];
        len.copy_from_slice(&bytes[offset + 4..offset + 8]);
        let len = u32::from_le_bytes(len) as usize;
        if !(29..=format::MAX_FRAME - 12).contains(&len) || len > bytes.len() - offset - 12 {
            break;
        }
        let end = offset + 12 + len;
        let crc = format::checksum(&bytes[offset + 12..end - 4]);
        bytes[end - 4..end].copy_from_slice(&crc.to_le_bytes());
        offset = end;
    }
}

/// Exercise the production startup replay, plus valid structured histories with an independent map oracle.
pub fn recovery(data: &[u8]) {
    if data.len() > MAX_INPUT {
        return;
    }
    let path = Path::new("fuzz");
    let options = Options {
        max_keys: 256,
        ..Options::default()
    };
    let _ = storage::replay(&mut Cursor::new(data), path, data.len() as u64, &options);
    let mut repaired = data.to_vec();
    if repaired.len() >= 28 {
        let crc = format::checksum(&repaired[..24]);
        repaired[24..28].copy_from_slice(&crc.to_le_bytes());
        repair_frames(&mut repaired, 28);
        let _ = storage::replay(
            &mut Cursor::new(&repaired),
            path,
            repaired.len() as u64,
            &options,
        );
    }
    let mut expected = BTreeMap::new();
    let mut sequence = 0;
    let mut log = format::header(0, 0);
    for chunk in data.chunks(16).take(128) {
        if chunk.len() < 3 {
            break;
        }
        sequence += 1;
        let key = vec![chunk[1] % 32, chunk[2]];
        let delete = chunk[0] & 1 == 1;
        let value = if delete { &[][..] } else { &chunk[3..] };
        log.extend(format::encode(delete, sequence, 0, &key, value));
        if delete {
            expected.remove(&key);
        } else {
            expected.insert(key, (value.to_vec(), sequence));
        }
    }
    let replay = storage::replay(&mut Cursor::new(&log), path, log.len() as u64, &options)
        .expect("encoded valid history must replay");
    assert_eq!(replay.sequence, sequence);
    assert_eq!(replay.valid_len, log.len() as u64);
    let mut actual = BTreeMap::new();
    for (key, index) in replay.map {
        let Value::Disk { offset, .. } = index.value else {
            panic!("disk index expected");
        };
        let record = format::read_record(&mut Cursor::new(&log), path, offset, log.len() as u64)
            .unwrap()
            .unwrap();
        actual.insert(key, (record.value, index.version));
    }
    assert_eq!(actual, expected);
}

/// Differential operation sequences: ownership, CAS, missing deletes, limits, and ordered pagination.
pub fn state(data: &[u8]) {
    if data.len() > MAX_INPUT {
        return;
    }
    let db = Db::memory_with(
        Options {
            max_keys: 32,
            ..Options::default()
        },
        Arc::new(SystemClock),
    )
    .unwrap();
    let mut model: BTreeMap<Vec<u8>, Entry> = BTreeMap::new();
    let mut sequence = 0u64;
    for chunk in data.chunks(12).take(256) {
        if chunk.len() < 3 {
            break;
        }
        let key = vec![chunk[1] % 32];
        let op = chunk[0] % 5;
        match op {
            0..=2 => {
                let condition = match op {
                    0 => Condition::Any,
                    1 => Condition::Absent,
                    _ => Condition::Version(chunk[2] as u64),
                };
                let matches = match condition {
                    Condition::Any => true,
                    Condition::Absent => !model.contains_key(&key),
                    Condition::Version(v) => model.get(&key).is_some_and(|e| e.version == v),
                };
                let result = db.put(&key, &chunk[3..], None, condition);
                if matches {
                    sequence += 1;
                    assert_eq!(result.unwrap().version, sequence);
                    model.insert(
                        key,
                        Entry {
                            value: chunk[3..].to_vec(),
                            version: sequence,
                            expires_at_ms: None,
                        },
                    );
                } else {
                    assert!(matches!(result, Err(Error::Conflict)));
                }
            }
            3 => {
                let expected = model.remove(&key).is_some();
                assert_eq!(db.delete(&key, DeleteCondition::Any).unwrap(), expected);
                if expected {
                    sequence += 1;
                }
            }
            _ => assert_eq!(db.get(&key).unwrap(), model.get(&key).cloned()),
        }
        assert_eq!(db.stats().unwrap().sequence, sequence);
    }
    let mut actual = BTreeMap::new();
    let mut after = None;
    loop {
        let page = db.scan_prefix(b"", after.as_deref(), 3).unwrap();
        actual.extend(page.entries);
        after = page.next_key;
        if after.is_none() {
            break;
        }
    }
    assert_eq!(actual, model);
}
