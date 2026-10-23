# Power-loss simulation evidence and limits

Process kill leaves the kernel/page cache alive. It cannot test loss or reordering of unsynced disk data. The new model reconstructs crash images from the **actual engine's** file create/write/truncate/sync and directory rename/remove/sync operations, using test-only tracing in the storage helpers. It does not replace the engine with a second implementation of the publication algorithm.

## Durability model

Each modeled inode has independently tracked volatile bytes, durable bytes and pending changes. Each directory has separately tracked visible names, durable names and pending atomic namespace operations. File sync commits file data; it does not commit its name. Directory sync commits names; it does not flush file data. All subsets of pending directory changes are explored, including a rename surviving independently of the earlier temporary-name creation. Rename is atomic; old and new manifest fragments are not spliced together.

At each traced step, crash images include dropping all dirty bytes, preserving all dirty bytes, sampled partial-write prefixes, and seeded reverse-order partial 8-byte fragments. Previously flushed bytes remain intact; this model assumes working flush barriers and excludes media damage to acknowledged bytes. Images are materialized as real files and reopened through the actual `Db::open_with` recovery decoder. The recovered state and high-water mark must equal the acknowledged state or the single mutation that might have completed without acknowledgment. A complete unacknowledged mutation may be adopted; a partial one must never become a successful value.

Reordered unsynced append fragments may produce invalid framing/checksums. In those specific cases, fail-closed corruption is accepted according to the current format contract; the test does **not** call this uninterrupted availability or successful automatic recovery. Lost names, damaged selected snapshots, and silently returning older acknowledged state are failures. Before any initialization acknowledgment, an interrupted first manifest may fail closed as documented; after open completes, normal durability rules apply.

## Covered scenarios

- Append, overwrite, tombstone/delete, preserving TTL metadata, and append after compaction.
- Every traced step of first manifest publication and whole-generation replacement, including old-file deletion.
- Expiry purge, empty snapshots, repeated compaction, and preserving sequence high-water marks with no live keys.
- Partial append errors injected as ENOSPC after 0, 1, 7, 12, 20, 40 and 80 bytes; unhealthy-owner handling and recovery that itself encounters a late sync failure.
- Sync errors reported after data may already have become durable, verifying unknown outcomes.
- Negative controls that intentionally remove an append-file sync, replacement-log sync, or post-rename directory sync from the trace. The harness must detect each broken protocol; removing a barrier cannot merely keep the tests green.

The principal publication scenarios currently check **458 distinct modeled crash cases**: 216 append/compaction, 88 initialization, and 154 empty-snapshot/purge images. Additional partial-write and delayed-error images are checked by separate tests. Deduplication is local to each scenario and includes the expected acknowledgment state and persistence mode; different modes can yield identical file bytes. These counts describe this finite suite, not exhaustive device-state coverage.

Run:

```sh
cargo test -p yalidb-core storage::simulation -- --nocapture
cargo test -p yalidb-core --test process_crash
```

## Remaining evidence required

This is a test-only trace-driven durability model, not a complete injectable filesystem or block-device emulator. It samples mid-write cuts/reordering rather than enumerating all possible byte schedules. It does not model filesystem journaling, hardware caches that ignore flush, parent-directory loss, sector garbage affecting already flushed bytes, real disk-full allocation behavior, or every compound I/O failure. The database-directory parent is assumed to exist and remain durable. Existing decoder tests separately explore all cuts of a small record and complete bit flips.

Next deployment validation must run on the intended Linux filesystem/device stack: replay seeded workloads with an external acknowledged-operation ledger, interrupt storage at selected points in an isolated VM/disposable volume, reboot/remount, and check the acknowledged prefix and permitted unknown outcomes. Record filesystem/mount options, kernel/device/cache settings, seed and cut point. Keep immutable pre-test fixtures and avoid testing on a user's normal volumes. OS-level error/ENOSPC tests and longer randomized histories remain release requirements; do not mark them complete based on this simulator.

The separation of anomaly, crash, and simulated power-loss tests follows the distinction described in [SQLite's testing documentation](https://www.sqlite.org/testing.html). Working sync semantics remain an explicit environmental assumption, not a property a unit test can prove.

## Linux validation increment

The [Linux harness](linux-validation.md) now adds real ext4 process-kill/recovery and volume-reattachment checks, plus short-write/ENOSPC handling on bounded tmpfs. [Recorded results](validation/2026-10-23.md) distinguish native ARM64 from emulated x86-64. These checks leave the kernel alive; actual power interruption, journaling-filesystem disk-full behavior and deployment-device validation remain open.

## Disposable VM cut increment

The [QEMU guest harness](vm-power-loss-testing.md) now boots a real Linux kernel on ext4/XFS, removes guest RAM and unflushed virtual-device writes, then boots an independent recovery guest. [Measured results](validation/vm-power-loss-2026-10-23.md) distinguish this evidence from process kills, trace simulations and physical host/drive power cuts. The earlier simulator remains complementary: virtual-device flushes are atomic with respect to controller cuts, so this VM campaign does not explore every mid-flush or sector-reordering outcome.
