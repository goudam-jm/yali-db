# Production readiness review — 2026-10-23

**Decision: experimental implementation; production release blocked.** The technical spec now provides an executable contract, not proof of reliability under every deployment failure. A passing local test suite is necessary and insufficient for a new database.

## Review findings resolved in this revision

- Replaced vague segment rotation with one implementable generation-publication protocol.
- Specified checksums for frame lengths as well as bodies, bounds before allocation, complete snapshot validation, strict sequence replay, and fail-closed corruption handling.
- Defined single-owner locking, clone lifetime, synchronization/linearization, ambiguous write outcomes, and unhealthy-owner behavior.
- Specified concrete key/log/page limits, deletion headroom, maintenance memory/headroom requirements, and capacity refusal without live-data eviction.
- Made TTL boundary, within-owner clock clamping, rollback-across-restart limitation, and explicit maintenance behavior concrete.
- Preserved versions through deletion, expiry, compaction, and reopen; added CAS and bounded lexicographic pagination.
- Defined independent offline backup/restore and a production/evaluation distinction for the HTTP adapter.

## Implemented evidence

The workspace contains a safe Rust embedded engine, a volatile mode using the same semantics, a disk-backed keydir, a bounded experimental loopback HTTP adapter, and an example. `Cargo.lock` is included in the workspace for reproducible dependency resolution. Tests exercise:

- Byte round trips, limits, owned read results, sequence exhaustion, stale writes, missing deletes, and nonmutation on invalid operations.
- Exact TTL boundaries, rollback clamping, purge/reopen, and overflow.
- Scan count/byte limits, binary ordering, and continuation over an unchanged dataset.
- Threaded CAS contention and deterministic reference-model workloads with reopen/compaction.
- Record bit flips, all cuts of a frame, bounded arbitrary parser input, read-time corruption, missing manifests, incomplete snapshots, and orphan handling.
- I/O failures at append/compaction persistence boundaries; unhealthy-owner behavior and acknowledged-state recovery.
- Process kill after observed acknowledgments with concurrent opportunities for compaction; independent offline backup restore.
- Simulated power-loss crash images, partial ENOSPC writes, late sync failures, and negative controls that detect missing durability barriers.
- Bounded HTTP storage queue, expired-job suppression, absolute write-window timing, real slow-header/body/framing/connection-cap tests, graceful in-flight write drain, and Python/Node interoperability.

Consult the current test run for counts/results. Bounded arbitrary-input tests are not a substitute for coverage-guided fuzzing; threaded CAS is not an exhaustive linearizability check; the trace model now exercises partial/reordered dirty data and metadata publication, but is not a complete filesystem/device emulator. These distinctions are intentional.

## Release checklist

- [x] Run the 32-test workspace suite locally on Rust 1.89.0 and Rust 1.99.0 (macOS/Apple Silicon); formatting, strict Clippy and documentation checks pass on 1.99.0. The one ignored entry is a child-process crash-test helper invoked by its parent test.
- [x] Run restricted Linux ext4 storage checks locally on native ARM64 and emulated x86-64: 29 core tests, 32 crash/recovery seeds and 1,008 acknowledged operations per architecture; cross-process locking and reattachment verification pass. [Harness and limits](linux-validation.md).
- [x] Run native Linux x86-64 and ARM64 hosted storage CI: 64 crash/recovery seeds and 2,016 acknowledged operations per architecture, core suite, locking, ENOSPC and container reattachment passed. Stable/MSRV Rust checks also passed on Linux and macOS. [Hosted evidence](validation/2026-10-23.md#hosted-validation-follow-up).
- [ ] Run failure tests on the intended deployment filesystem/device/cache stack. Hosted process-recovery checks do not establish power-loss durability, especially with runner mounts using `nobarrier` and `data=writeback`.
- [x] Add a trace-driven durability model for lost/reordered dirty bytes, independently durable namespace operations, partial ENOSPC writes, late sync errors and compound recovery failures. Exercise every traced step in append/compaction/initialization/empty-snapshot scenarios. [Evidence and limits](power-loss-testing.md): 458 principal crash cases, additional injected-error cases, and negative controls for omitted barriers.
- [x] Exercise real Linux tmpfs short writes and ENOSPC, poisoned-owner refusal and recovery preserving acknowledged data; observed a 131,072-byte partial POSIX write. This validates kernel error handling, not journaling disk-full durability.
- [x] Run disposable QEMU Linux guest power cuts on ext4/XFS locally and in native ARM64/x86-64 CI: local 20 normal recoveries plus two negative controls; hosted 40 normal recoveries plus four negative controls passed. Cuts remove guest RAM and discard unflushed virtual-device writes. [Measured evidence and boundaries](validation/vm-power-loss-2026-10-23.md).
- [ ] Extend OS/device-level fault validation on disposable Linux storage: real short-write/ENOSPC behavior, filesystem/cache settings, parent-directory loss, broader schedules/seeds and compound failures. The finite trace model does not certify a real storage stack.
- [x] Add coverage-guided decoding, actual recovery replay and reference-model state fuzz targets; run a bounded ASan campaign with corpus minimization and independent v1 binary fixtures: **25,785,630 executions**, no findings. [Measured results](validation/2026-10-23.md).
- [ ] Complete multi-hour native Linux fuzz campaigns with retained corpora and regression promotion for every finding. Five minutes per target on macOS is initial evidence, not a completed soak gate.
- [ ] Run randomized crash/recovery and concurrent history validation across substantially more seeds and extended workloads. Verify no acknowledged state loss beyond the stated storage assumptions.
- [ ] Publish workload-specific p50/p95/p99, throughput, RSS/key, recovery duration, write amplification, disk headroom, and worst compaction pause. Compare with SQLite under matched durability. Define and meet workload budgets before approval; do not invent universal targets.
- [ ] Prove backup/restore procedures on representative sizes and independent volumes. Set backup frequency, acceptable backup data loss, restore-time objectives, and retention for the actual application.
- [x] Implement HTTP connection/task and storage-queue bounds, header/body/write/idle/lifetime limits, skipped expired work, overload refusal, and SIGINT/SIGTERM drain. [Resource contract and tests](http-hardening.md) include real slow-client/framing/saturation/shutdown checks.
- [x] Add [OpenAPI](openapi.json) and verify real Python/Node shared-state CAS interoperability.
- [ ] Obtain independent HTTP protocol/security review and prolonged overload/RSS measurements; broaden interoperability and operational fault tests before approving untrusted traffic. Loopback-only binding remains in force.
- [ ] Add complete service metrics for latency/sync/recovery/compaction/queue state; current core diagnostics only expose key/index/log counts, sequence, and health.
- [ ] Review dependencies and license/security reporting, establish a security contact and support policy, and document migration/export before format stabilization. Do not publish credentials or assign a nonexistent security contact.
- [ ] Obtain an independent storage-protocol/code review and resolve actionable findings. Do not label this done merely because tests pass.

## Initial operational envelope

One owner and local persistent disk; default selected-log cap 64 MiB and indexed-key cap 100,000. At least twice the log cap plus filesystem margin must be available for compaction. These are admission limits, not performance or RAM guarantees. Compaction builds a replacement keydir and sorted references and stops all operations. Disk-backed purge currently performs whole-generation compaction; no periodic background scheduler is included.

For the core, successful writes flush before acknowledgment; process death is covered by tests under normal filesystem behavior. There is no host/volume-loss guarantee, replication, automatic failover, cross-key transaction, security-sensitive TTL contract, or safe live file-copy backup. A clock reset across restart can extend the visibility of records not durably purged. If these limitations are incompatible with an application, it is outside the initial supported scope.

For a production approval, record the deployment, expected traffic/cardinality/value sizes, measurement results, backup/outage objectives, failure-test results, remaining risks, reviewer, and approval date. Leave this decision blocked until the evidence exists.

## HTTP and power-loss increment: local validation

- `cargo test --workspace --locked`: 32 passed on Rust 1.99.0 and separately on Rust 1.89.0; crash-test helper deliberately excluded from ordinary direct invocation and run by its parent.
- `cargo fmt --all --check`, strict workspace Clippy, and `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked`: passed on 1.99.0.
- `python3 scripts/http-smoke.py`, `python3 scripts/http-hardening.py`, and `python3 scripts/http-interop.py`: passed with actual temporary loopback services; Node 24 used for interoperability.
- OpenAPI JSON and local schema references, documentation links and code fences: structurally checked. A dedicated OpenAPI conformance/security review remains part of independent protocol review.
- Initial publication, append/compaction, and expiry/empty-snapshot power-loss scenarios: 458 finite modeled crash cases, plus separate partial-write/late-error cases. Some corrupt unsynced tails are intentionally fail-closed outcomes; the model does not claim automatic recovery from all reorderings.

No actual power was cut, no user volume was faulted, and no hosted CI run or production load benchmark is claimed. The implementation changes close concrete engineering tasks while the remaining environmental and review gates stay open.

## Linux and fuzzing increment: local validation

The [2026-10-23 evidence report](validation/2026-10-23.md) records source fingerprints, environments, campaign counts and remaining limits. The expanded workspace suite passes **33 tests** on Rust 1.99.0; the added independent format fixture test raises the previous count by one. Rust 1.89.0 passes the expanded all-target/all-feature compilation check. Formatting, strict Clippy, rustdoc and workflow lint pass.

Native Linux storage CI and bounded push/PR fuzz campaigns are configured, with manual 30-minute-per-target campaigns available. The initial local report predates repository publication. The [hosted follow-up](validation/2026-10-23.md#hosted-validation-follow-up) now records successful native ARM64/x86-64 storage jobs and the stable/MSRV matrix. No real power cut or XFS run is claimed.

### Hosted follow-up

Published to [goudam-jm/yali-db](https://github.com/goudam-jm/yali-db). [Rust CI](https://github.com/goudam-jm/yali-db/actions/runs/39944032192) passed the stable/MSRV matrix on Linux and macOS. [Storage/fuzz CI](https://github.com/goudam-jm/yali-db/actions/runs/39944068524) passed native ARM64/x86-64 storage checks and **19,915,971 native Linux ASan fuzz executions** across three 301-second targets, with no findings and successful corpus minimization. [Preserved measurements](validation/2026-10-23.md#hosted-validation-follow-up) include the tested commit, source fingerprints and runner mount options. Longer soaks and power-loss/device validation remain blocked release gates.

## Disposable VM power-loss increment

The [VM harness](vm-power-loss-testing.md) runs a real guest kernel/filesystem with a bounded volatile NBD device and an external fsynced ACK ledger. It uses only private regular-file images in a non-root restricted container. The [local and hosted native campaigns](validation/vm-power-loss-2026-10-23.md) passed ext4/XFS recovery and negative controls. [Hosted CI](https://github.com/goudam-jm/yali-db/actions/runs/39947533475) checked 408 acknowledged calls across 40 normal cuts and detected all four deliberately broken-disk controls; all downloaded crash-image archive checksums were verified. This closes the initial disposable guest-cut exercise, while physical/deployment device power-loss and cache assumptions remain unvalidated.
