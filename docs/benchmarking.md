# Embedded benchmark baseline

This harness measures the existing engine before making performance changes. It compares one bounded session-state workload with SQLite from Python's standard library. It adds **no YaliDB runtime dependencies** and does not change the disk format or storage API.

## Workload and durability contract

The initial profile, selected by the owner, is 10,000 live keys, 8-byte binary keys, 1 KiB values and one sequential writer. Each repetition populates all keys, performs 1,000 deterministic warm reads, then 1,000 overwrites. Values repeat a deterministic byte; there is no expiry, contention, scan or HTTP traffic. Three repetitions alternate engine order and use recorded seeds. Inputs must fit the default 64 MiB selected-log limit with margin; no resource options are silently increased.

Both stores persist values and the same global mutation version. YaliDB uses its normal `Db::put` sync-before-success behavior and default compaction policy. SQLite uses a `WITHOUT ROWID` BLOB-key table plus a one-row sequence table, with both changes in **one transaction per acknowledged put**. The explicit metadata-table write is part of SQLite's measured cost. Settings are applied and read back: WAL, synchronous FULL, 1,000-page automatic checkpoint, 2 MiB page-cache budget, and no mmap.

These settings request durability at acknowledgment; they do not prove that a particular host/device honors flushes. Filesystem/mount options, kernel, CPU information, cgroup limits, compiler, Python and SQLite versions accompany each report. Hosted runner and Docker storage are development environments, not certified production SSDs. SQLite's WAL/FULL commit and checkpoint behavior is documented in [SQLite pragmas](https://sqlite.org/pragma.html#pragma_synchronous) and [WAL](https://sqlite.org/wal.html).

## Measurements

- Per-call p50/p95/p99 and maximum, using nearest-rank percentiles. Put time includes a complete YaliDB API call or complete SQLite transaction; reads fetch and validate values/versions. Key/value generation and result validation are outside the individual call timer, but included in phase throughput.
- Achieved phase throughput uses elapsed loop time, including driver and validation overhead. This is **closed-loop**, with no offered-load schedule or concurrent queue. It must not be used to claim overload capacity or an end-user request-latency SLO.
- Process RSS before open, after open, after population and after overwrites. Incremental process RSS per key is `(populated RSS - empty-open RSS) / key_count`, including benchmark timing-buffer and allocator overhead. It is not a precise keydir allocation count or a constant valid at every cardinality.
- Fresh-process open/recovery after the load worker exits without destructors/`sqlite3_close`. SQLite's WAL is not cleanly checkpointed by connection close first. The kernel remains alive and its cache is warm; this is neither cold-cache I/O nor power-loss recovery. All values, versions and the global sequence are checked after reopening and maintenance.
- Explicit YaliDB `compact` duration. SQLite's full-rebuild observation runs `wal_checkpoint(TRUNCATE)`, `VACUUM`, then another truncating checkpoint. These are different maintenance algorithms. The maximum across three observations is an observed pause, not a worst-case guarantee. RSS/high-water RSS is recorded before validation allocates its reference map.
- Logical/allocated database-file bytes and a 5 ms sampled peak. These cover visible files in the managed directory, **exclude unlinked temporary files**, and can miss short peaks. They are lower bounds, not complete physical-device headroom measurements.
- Linux process `write_bytes` divided by logical key/value mutation bytes, including initialization, recovery and maintenance. This is process I/O accounting, not block-device/controller/NAND write amplification. Per-stage counters remain in the report.

Rust and Python/SQLite use different client runtimes and timer overhead. Absolute RSS includes the runtime; raw read-call latency is not a language-neutral storage-engine contest. Kernel page-cache memory is not included in process RSS. Cold-cache reads, open-loop load, concurrency, TTL, binary-size increment, true device write amplification and complete disk headroom remain unmeasured.

## Reproduce on Linux

Use a new report directory. Assertions are mandatory: the harness rejects `python -O` and `PYTHONOPTIMIZE` before creating output or opening a database.

```sh
cargo build -p yalidb-core --release --example benchmark --locked
python3 -m unittest discover -s scripts/benchmark -p 'test_*.py'
python3 scripts/benchmark/run.py --keys 10000 --value-bytes 1024 --operations 1000 --repetitions 3 --output validation-results/benchmark-run
```

`--work-dir EXISTING_DIRECTORY` places a new private temporary data directory on a selected filesystem. It never reuses a database already in that directory. Report directories cannot be reused. Successful runs remove their synthetic data; failures retain data and stderr/TSV diagnostics, including captured partial output on worker timeout, copying data into the report when a separate work parent was used.

The controller runs one engine process at a time, with a separate fresh process for recovery. The workers are internal implementation details; do not invoke them against application data. SQLite temporary files stay in that repetition's private directory, allowing an otherwise read-only container. No cache dropping, host-device access or root privileges are needed.

For macOS development, use the Linux image with a fresh Docker volume:

```sh
docker build --platform linux/arm64 -t yalidb-benchmark:arm64 -f scripts/benchmark/Dockerfile .
benchmark_volume="yalidb-benchmark-$(uuidgen)"
docker volume create "$benchmark_volume"
trap 'docker volume rm "$benchmark_volume"' EXIT
mkdir -p validation-results/benchmark-container-run
docker run --rm --platform linux/arm64 --user "$(id -u):$(id -g)" --read-only --network none --cap-drop ALL --security-opt no-new-privileges=true --memory 1g --cpus 2 --pids-limit 128 --mount "type=volume,source=$benchmark_volume,target=/work" --mount "type=bind,source=$PWD/validation-results/benchmark-container-run,target=/results" yalidb-benchmark:arm64 --work-dir /work --output /results/run
```

For x86-64, replace architecture/tag with amd64. Container architecture must match the engine or be explicitly identified as emulated. Source fingerprints cover the core, benchmark code/tests and Cargo locks; changing the Docker base/packages can still change the environment, so retain version/mount metadata. CI runs for pull requests and pushes to `main`; workflow dispatch supports other refs. It runs natively on `ubuntu-24.04` and `ubuntu-24.04-arm`, uploads raw TSVs and JSON for 14 days, and makes no timing-based pass/fail assertion. Correctness checks, successful recovery and valid sample counts are required.

The [production checklist](production-readiness.md) requires workload budgets and target-system validation before approval. This baseline does not supply those budgets.
