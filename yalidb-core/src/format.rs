//! Bounded, checksummed format decoding. No unchecked on-disk allocations.
use crate::{Error, MAX_KEY_BYTES, MAX_VALUE_BYTES, Result};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub(crate) const HEADER_LEN: u64 = 28;
pub(crate) const MAX_FRAME: usize = 12 + 29 + MAX_KEY_BYTES + MAX_VALUE_BYTES;

pub(crate) fn corruption(path: &Path, offset: u64, reason: &'static str) -> Error {
    Error::Corruption {
        path: path.to_owned(),
        offset,
        reason,
    }
}

pub(crate) fn checksum(bytes: &[u8]) -> u32 {
    crc32fast::hash(bytes)
}

pub(crate) fn manifest(generation: u64) -> Vec<u8> {
    let mut b = b"YALIM001".to_vec();
    b.extend(generation.to_le_bytes());
    b.extend(checksum(&b).to_le_bytes());
    b
}

pub(crate) fn parse_manifest(b: &[u8], path: &Path) -> Result<u64> {
    if b.len() != 20 || &b[..8] != b"YALIM001" || checksum(&b[..16]) != u32_at(b, 16) {
        return Err(corruption(
            path,
            0,
            "invalid manifest or unsupported format",
        ));
    }
    let generation = u64_at(b, 8);
    if generation == 0 {
        return Err(corruption(path, 8, "zero generation"));
    }
    Ok(generation)
}

pub(crate) fn header(sequence: u64, count: u64) -> Vec<u8> {
    let mut b = b"YALIL001".to_vec();
    b.extend(sequence.to_le_bytes());
    b.extend(count.to_le_bytes());
    b.extend(checksum(&b).to_le_bytes());
    b
}

pub(crate) fn parse_header(b: &[u8], path: &Path) -> Result<(u64, u64)> {
    if b.len() != HEADER_LEN as usize
        || &b[..8] != b"YALIL001"
        || checksum(&b[..24]) != u32_at(b, 24)
    {
        return Err(corruption(
            path,
            0,
            "invalid log header or unsupported format",
        ));
    }
    Ok((u64_at(b, 8), u64_at(b, 16)))
}

#[derive(Debug)]
pub(crate) struct Record {
    pub delete: bool,
    pub sequence: u64,
    pub expires: u64,
    pub key: Vec<u8>,
    pub value: Vec<u8>,
    pub size: u64,
}

pub(crate) fn encode(
    delete: bool,
    sequence: u64,
    expires: u64,
    key: &[u8],
    value: &[u8],
) -> Vec<u8> {
    let body_len = 29 + key.len() + value.len();
    let mut b = b"YLR1".to_vec();
    b.extend((body_len as u32).to_le_bytes());
    b.extend(checksum(&b).to_le_bytes());
    b.push(u8::from(delete));
    b.extend(sequence.to_le_bytes());
    b.extend(expires.to_le_bytes());
    b.extend((key.len() as u32).to_le_bytes());
    b.extend((value.len() as u32).to_le_bytes());
    b.extend(key);
    b.extend(value);
    b.extend(checksum(&b[12..]).to_le_bytes());
    b
}

/// None denotes an incomplete active-tail frame; only recovery may truncate it.
pub(crate) fn read_record<R: Read + Seek>(
    reader: &mut R,
    path: &Path,
    offset: u64,
    end: u64,
) -> Result<Option<Record>> {
    reader.seek(SeekFrom::Start(offset))?;
    if end.saturating_sub(offset) < 12 {
        return Ok(None);
    }
    let mut framing = [0; 12];
    reader.read_exact(&mut framing)?;
    if &framing[..4] != b"YLR1" || checksum(&framing[..8]) != u32_at(&framing, 8) {
        return Err(corruption(path, offset, "invalid frame header"));
    }
    let len = u32_at(&framing, 4) as usize;
    if !(29..=MAX_FRAME - 12).contains(&len) {
        return Err(corruption(path, offset, "invalid frame length"));
    }
    if end.saturating_sub(offset) < (12 + len) as u64 {
        return Ok(None);
    }
    let mut b = vec![0; len];
    reader.read_exact(&mut b)?;
    if checksum(&b[..len - 4]) != u32_at(&b, len - 4) {
        return Err(corruption(path, offset, "record checksum mismatch"));
    }
    let k = u32_at(&b, 17) as usize;
    let v = u32_at(&b, 21) as usize;
    if k == 0 || k > MAX_KEY_BYTES || v > MAX_VALUE_BYTES || 29 + k + v != len || b[0] > 1 {
        return Err(corruption(path, offset, "invalid record fields"));
    }
    let sequence = u64_at(&b, 1);
    let expires = u64_at(&b, 9);
    if sequence == 0 || (b[0] == 1 && (expires != 0 || v != 0)) {
        return Err(corruption(path, offset, "invalid sequence or tombstone"));
    }
    Ok(Some(Record {
        delete: b[0] == 1,
        sequence,
        expires,
        key: b[25..25 + k].to_vec(),
        value: b[25 + k..25 + k + v].to_vec(),
        size: (12 + len) as u64,
    }))
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}
fn u64_at(b: &[u8], at: usize) -> u64 {
    let mut bytes = [0; 8];
    bytes.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn crc_known_vector_and_roundtrip() {
        assert_eq!(checksum(b"123456789"), 0xcbf43926);
        let b = encode(false, 9, 123, b"\0key", b"\xffvalue");
        let r = read_record(&mut Cursor::new(&b), Path::new("test"), 0, b.len() as u64)
            .unwrap()
            .unwrap();
        assert_eq!(
            (r.sequence, r.expires, r.key, r.value),
            (9, 123, b"\0key".to_vec(), b"\xffvalue".to_vec())
        );
    }

    #[test]
    fn every_frame_cut_is_incomplete_and_complete_bit_flips_fail() {
        let b = encode(false, 1, 0, b"k", b"value");
        for n in 0..b.len() {
            assert!(
                read_record(&mut Cursor::new(&b[..n]), Path::new("t"), 0, n as u64)
                    .unwrap()
                    .is_none()
            );
        }
        for i in 0..b.len() {
            let mut bad = b.clone();
            bad[i] ^= 1;
            assert!(
                read_record(&mut Cursor::new(&bad), Path::new("t"), 0, bad.len() as u64).is_err(),
                "byte {i}"
            );
        }
    }

    #[test]
    fn decoder_rejects_arbitrary_bounded_input_without_panicking() {
        let mut seed = 42u64;
        for len in 0..1024 {
            let bytes: Vec<_> = (0..len)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    seed as u8
                })
                .collect();
            let _ = read_record(&mut Cursor::new(&bytes), Path::new("fuzz"), 0, len as u64);
            let _ = parse_manifest(&bytes, Path::new("fuzz"));
            let _ = parse_header(&bytes, Path::new("fuzz"));
        }
    }
}
