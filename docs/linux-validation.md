# Linux storage validation harness

The default runner uses **fresh Docker-managed volumes** for disk-backed tests and a bounded 64 MiB tmpfs for real short-write/ENOSPC tests. It does not require privileged containers, block devices, filesystem formatting, or host mounts beyond its report directory. It runs as the calling user's UID/GID, drops all capabilities, enables `no-new-privileges`, disables networking, and makes the image filesystem read-only. Memory, CPUs and PIDs are bounded.

## Run the default harness

Docker must already be available. From the project root, choose the architecture matching your Docker engine:

```sh
docker build --platform linux/arm64 -t yalidb-validation:arm64 -f scripts/linux-validation/Dockerfile .
python3 scripts/run-linux.py --platform linux/arm64 --image yalidb-validation:arm64 --seeds 32 --output validation-results/linux-arm64-run
```

For x86-64, replace `arm64` with `amd64`. A nonmatching Docker engine may emulate the requested architecture; reports explicitly record engine architecture, container architecture and emulation. This is not equivalent to testing native x86-64 hardware. The output directory must be new so previous evidence is preserved.

The base Rust image is pinned by multi-architecture digest. The image compiles test executables and a synthetic workload driver at build time; runtime tests execute those binaries directly, with no writable compiler cache or network access. The runner creates one UUID-named volume, verifies its state in a second container, and removes only that newly created container/volume. It never prunes existing Docker resources or accepts an existing volume name/device to test.

## What is tested

- The complete core test suite runs with `TMPDIR` on the managed Linux volume. Its actual filesystem and mount options are recorded with `findmnt`, rather than inferred from the host OS.
- Two separate processes contend for the owner lock; reopening succeeds after the first exits.
- Each seeded workload performs puts/deletes and compactions, with an external fsynced ACK ledger outside the database volume. A GO/BEGIN/ACK handshake permits at most one mutation beyond the last ledger entry. After SIGKILL, the entire recovered key/value/version map and sequence must equal the acknowledged state or that one known pending operation. Merely observing a greater counter is insufficient.
- Independent seeds vary keys, operations and cut delay. Defaults run 32 seeds; CI runs 64. Actual observed ACK counts are included in the report.
- On bounded tmpfs, the harness fills space, frees a small reservation, observes a successful short POSIX write, then confirms ENOSPC. The real database must return an I/O error, become unhealthy, and preserve every acknowledged value after space is released and the owner reopens. Speculative filesystem preallocation is tolerated by tracking all puts that actually succeeded before the failure.
- The container exits; a second restricted container mounts the same fresh volume and verifies every recovered workload state and sequence. This verifies persistence across container recreation, **not a kernel crash or power cut**. Tmpfs data is checked before exit and is not expected to survive it.

Reports contain kernel/toolchain, mount information, source and fixture fingerprint, image ID, core-suite logs, per-seed external ACK ledgers, test outcomes, and container limits. Database failure copies are preserved when applicable. Raw reports/logs live under ignored `validation-results/`; selected measurements are recorded in [the validation summary](validation/2026-10-23.md).

## CI

`.github/workflows/storage-and-fuzz.yml` configures native `ubuntu-24.04` x86-64 and `ubuntu-24.04-arm` ARM64 jobs, each using the restricted managed-volume runner. It uploads results even on failure and bounds job duration. Runner labels follow [GitHub's hosted-runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).

The project is hosted at [goudam-jm/yali-db](https://github.com/goudam-jm/yali-db). Native ARM64 and x86-64 storage jobs have passed; see the [hosted results](validation/2026-10-23.md#hosted-validation-follow-up). The ordinary CI workflow additionally tests all core features and the declared minimum Rust version. Hosted runners record their own mount options; successful process recovery does not certify their storage for power-loss durability.

## Additional filesystems and device failures

The harness also contains an explicit, nondefault loop-filesystem mode for freshly created file-backed ext4/XFS images. It requires root plus `--allow-loop-devices`, and is **not enabled in CI or executed by the default runner**. Automatic approval review rejected the privileged run in this session because its device/filesystem exposure was broader than the command itself constrained. No attempt was made to bypass that rejection; the restricted managed-volume run was used instead. The optional privileged mode remains unvalidated and requires a separate explicit approval/review before use.

These tests do not establish XFS support, disk-full behavior on every journaling filesystem, parent-directory survival under power loss, device cache correctness, or recovery after a kernel/VM hard reset. Deployment approval still requires validation on the intended filesystem/device/mount configuration, including a disposable VM or dedicated test device for sudden-reset experiments. See [power-loss testing](power-loss-testing.md) and the [production checklist](production-readiness.md).
