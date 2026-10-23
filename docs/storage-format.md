# Storage format v1 (experimental)

The version identifies encoding, not a promise that every experimental format is production compatible. All integers are unsigned little-endian. CRC is IEEE CRC32 (the `crc32fast` implementation), not CRC32C. Checksums detect accidental damage, not hostile modification.

## Files and ownership

A private local directory contains `LOCK`, `MANIFEST`, and one selected `<generation>.log`, where generation is 16 lowercase hexadecimal digits starting at 1. Temporary `MANIFEST.tmp` and other generations can exist during interrupted publication. The lock file is stable and must never be removed while any owner is running. All participants must cooperate with its advisory OS lock.

New directories use Unix mode 0700 and files use 0600, subject to the process umask. Existing paths retain their permissions. The directory and its ancestry must be trusted; final managed symlinks/nonregular files are rejected, but race-free operation in an adversarial writable directory is not promised. The parent directory must exist before open.

The manifest is exactly 20 bytes:

- Offset 0, 8 bytes: ASCII `YALIM001`.
- Offset 8, 8 bytes: selected generation (nonzero).
- Offset 16, 4 bytes: CRC of bytes 0 through 15.

The log header is exactly 28 bytes:

- Offset 0, 8 bytes: ASCII `YALIL001`.
- Offset 8, 8 bytes: snapshot mutation high-water mark.
- Offset 16, 8 bytes: snapshot record count.
- Offset 24, 4 bytes: CRC of bytes 0 through 23.

Exactly `count` snapshot put frames follow the header. Their versions are strictly increasing and no greater than the high-water mark; keys are unique. No snapshot tombstones are allowed. Expired data can be omitted during snapshot construction. Zero snapshot count is valid even with a nonzero high-water mark. Subsequent frames are appends with sequence `high_water + 1`, then consecutive increments. Missing/deleted versions are therefore preserved by the high-water mark, rather than inferred from surviving values.

## Frame layout

Each frame has a 12-byte envelope:

- Offset 0, 4 bytes: ASCII `YLR1`.
- Offset 4, 4 bytes: body length, including body CRC.
- Offset 8, 4 bytes: CRC of envelope bytes 0 through 7.

The body is 29 + key length + value length bytes:

- Body offset 0, 1 byte: operation; 0 = put, 1 = delete.
- Body offset 1, 8 bytes: mutation sequence, nonzero.
- Body offset 9, 8 bytes: absolute expiry in UTC Unix milliseconds; zero = permanent.
- Body offset 17, 4 bytes: key length (1–1,024).
- Body offset 21, 4 bytes: value length (0–65,536).
- Body offset 25: key bytes followed by value bytes.
- Last 4 bytes: CRC of all preceding body bytes.

A tombstone has zero expiry and zero value length. Maximum total frame size is 66,601 bytes. Validate envelope CRC and bounds before allocating the body; validate body CRC, exact length, field bounds, operation, and sequence before applying. Decoder allocation is bounded independently of file-provided lengths.

## Recovery rules

Lock first. Validate the manifest-selected log and its complete snapshot, then replay append frames. Reject oversized existing logs/key counts according to explicit open options. Synchronize complete recovered frames before publishing the owner. A complete unacknowledged frame may become visible after recovery; callers must treat I/O failures or lost responses as unknown outcomes.

At the append tail only, fewer than 12 bytes, or a valid bounded envelope whose body is incomplete, is truncated to the previous frame boundary and synchronized. An incomplete snapshot is corruption, not a recoverable append tail. CRC/field/ordering failures are corruption even at EOF. Errors name the file and offset. The owner never automatically salvages by skipping damaged records.

After successful replay, remove recognized unselected generation files and `MANIFEST.tmp`, then synchronize the directory. Unknown filenames are not adopted or deleted. A missing manifest in a nonempty directory fails closed.

## Compaction publication

With operations paused, stream current live records into generation N+1 in version order, preserving versions and absolute expiry. Header high-water mark includes all previous successful mutations, including deletes. Build the replacement index using the new offsets.

Flush new log → flush directory → write and flush `MANIFEST.tmp` → rename to `MANIFEST` → flush directory → switch index/file → delete old logs → flush directory. Before publication, the old manifest selects complete old history. After durable publication, the new manifest selects the complete replacement snapshot. A failure at any persistence step invalidates the current owner; reopening resolves which complete generation is selected.

The first implementation has one append log and whole-generation compaction; it does not have incremental segmented compaction. Process interruption tests and injected I/O error tests cover this protocol. A [trace-driven power-loss model](power-loss-testing.md) now covers dropped/reordered dirty data and independently durable directory names; real OS/device validation remains a production release requirement.

## Compatibility and repair

Unknown magic/version fails closed. There is no automatic format migration or salvage tool yet. Before a format-changing release, provide a tested export/import or migration tool and keep golden fixtures for every supported format. Preserve an original copy for corruption investigation. Never remove or edit a manifest/log to make a production open succeed.
