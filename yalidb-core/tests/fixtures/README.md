# v1 fixtures

`v1-manifest.bin` selects generation one. `v1-log.bin` has an empty snapshot header followed by a permanent put at sequence one: key `fixture`, value `known-value`.

These files were encoded independently using Python's `struct.pack` little-endian integer layouts and `zlib.crc32`, following `docs/storage-format.md`. They are static test data; tests must not regenerate them with YaliDB's encoder. Future migrations should retain these fixtures and add independently constructed snapshot/tombstone/expiry cases.

Additional independent Python/zlib fixtures cover a nonempty snapshot plus append, deletion, an incomplete active tail, an incomplete snapshot, and a sequence gap. Their original seed forms are in `fuzz/seeds/recovery`; `tests/format_fixtures.rs` asserts both recovery results and fail-closed behavior without modifying the fixture files.
