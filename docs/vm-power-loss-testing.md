# Disposable Linux VM power-loss validation

This harness boots a **real Linux kernel** in QEMU, runs the actual YaliDB engine on a fresh ext4 or XFS filesystem, abruptly removes guest RAM and the virtual device's unflushed write cache, then boots an independent guest to recover the disk. It extends process-kill and trace-model evidence. It does **not** cut power to the host computer or certify a physical drive/controller.

## Isolation and scope

QEMU uses software emulation (TCG) inside a non-root, read-only container with no network, capabilities, host block devices, KVM, host shares, Docker socket, or device passthrough. Only a newly created private Docker volume for images and the chosen empty results directory are mounted. The host runner never accepts an existing disk or volume to format. It removes only its UUID-named container/volume. Do not run it with sudo or add privileges.

The guest root filesystem is an initramfs. Its **sole** block device `/dev/vda` is the private, 512 MiB, regular-file-backed export created by this run. Formatting commands execute inside that guest. This does not reuse the earlier privileged host-loop-device path. Memory, CPU and process counts are bounded; every boot, armed boundary and full campaign has a timeout.

The virtual disk is a test-only NBD server over a private Unix socket. Writes enter a bounded volatile sector map; reads see pending writes. Guest FLUSH and FUA persist through `pwrite` and `fsync` before acknowledgment. A cut discards pending sectors instead of letting QEMU termination drain its cache. Merely killing QEMU with an ordinary file-backed writeback disk would leave host page-cache contents alive; this backend makes the chosen volatile-device boundary explicit.

The backend's bounded protocol subset supports read/write/flush/FUA/disconnect and rejects unsupported commands. Independent tests verify overlapping writes, flush durability, FUA on the wire, lost unflushed data, boundary blocking and refusal to overwrite an existing image. It is a validation oracle, not a production NBD implementation.

## Run

Requires Docker and Python 3.9+ on the host. Choose the architecture of your Docker engine:

```sh
docker build --platform linux/arm64 -t yalidb-vm-powercut:arm64 -f scripts/vm-powercut/Dockerfile .
python3 scripts/run-vm-powercut.py --platform linux/arm64 --image yalidb-vm-powercut:arm64 --cases 10 --output validation-results/vm-powercut-arm64-run
```

For x86-64 use `linux/amd64` and a corresponding image tag. Container architecture and Docker daemon architecture are recorded separately. QEMU TCG emulates the guest CPU even when host/container architectures match. For a quick smoke test, use `--filesystems ext4 --cases 1`; the negative control still runs. Each output directory must be new.

```sh
python3 -m unittest discover -s scripts/vm-powercut -p 'test_*.py'
```

The protocol unit tests bind private Unix sockets. The image build downloads Debian kernel/filesystem packages and Rust dependencies; runtime networking is disabled. The Rust base image is digest-pinned; Debian security packages can change across builds. Reports record source fingerprints, QEMU/kernel/Rust versions, kernel/initrd hashes, image IDs and exact QEMU commands. Retain the built image when investigating a failure.

## Campaign and oracle

Each filesystem gets ten successful-durability scenarios plus one deliberately broken-disk negative control. Normal scenarios cut:

- Immediately after six acknowledged operations, with no additional mutation allowed.
- After a device write, before a flush, and after a flush has persisted but before its reply, during operation seven (pre-mutation compaction), eleven (post-mutation compaction), or seventeen (append).

These are device boundaries during those operations, not an exhaustive enumeration of every filesystem syscall or compaction publication step. The first matching device event wins; logs show the actual offset/length and boundary. Workload keys, puts/deletes and cut phases vary by recorded seed. Every database uses a separate newly formatted image.

Before the database opens, guest `sync` and an explicit backend checkpoint establish a durable empty filesystem baseline. XFS can continue background log initialization after `sync`, so the checkpoint records any additional cache bytes it persists. **No controller checkpoint occurs after database open.** A negative control only starts lying about flushes after this baseline exists.

A GO/BEGIN/ACK exchange allows one known pending mutation at most. The controller fsyncs its acknowledgment ledger outside the guest disk. At the cut it freezes that guest process, discards the device cache, and kills QEMU without requesting guest shutdown, unmount or sync. The only accepted recovered state is the complete acknowledged key/value/version map and sequence, or exactly the one known pending operation. The ACK-only case permits no extra operation. The ledger counts API calls, including missing deletes that do not advance sequence.

Before recovery, the harness saves a compressed immutable copy of the crash disk and its archive checksum. A fresh QEMU process boots a fresh kernel instance with the surviving disk bytes, mounts normally so filesystem journals can recover, and runs `Db::open` and a full map/version dump. Mount failure, database failure, acknowledged data loss or any forbidden state fails a normal case.

The negative control falsely acknowledges all guest flush/FUA requests after the baseline. It must accumulate unflushed bytes and report ignored barriers, and the same recovery oracle must detect a durability violation. Infrastructure errors/timeouts do not count as successful detection. This demonstrates that a green normal case depends on effective barriers, rather than surviving host cache accidentally hiding lost acknowledgments.

## Evidence and limits

Raw reports include serial consoles, block-operation traces, external ACK ledgers, pre-recovery disk archives, per-case outcomes and the aggregate report. Failure images are retained in the results directory; the disposable working volume is removed. GitHub Actions uploads artifacts with 14-day retention. Selected measurements belong in the [production checklist](production-readiness.md); retain/download artifacts before expiration when needed for review.

A successful run establishes recovery under this **specific virtual-device contract** and actual guest filesystem/kernel combination. The backend commits a flush under a lock; the controller cuts before/after that commit, not midway through backing-file fsync. FUA can persist earlier pending sectors as well as its own write. The campaign does not enumerate arbitrary device reordering, torn sectors, failures in acknowledged sectors, host/storage-server outage, controller firmware faults, parent-volume loss, journaling ENOSPC or every concurrent application history. The outer host remains powered, and the backend assumes its own fsync works. Earlier [trace-model tests](power-loss-testing.md) complement these boundaries; they do not certify hardware either.

Deployment approval still requires a dedicated disposable VM/device on the intended storage stack, documented cache/mount settings and actual host/drive-reset experiments as appropriate. Do not substitute hosted-runner ext4 mount options for the guest's recorded options or the production configuration.

Protocol and boot references: [NBD specification](https://github.com/NetworkBlockDevice/nbd/blob/master/doc/proto.md), [QEMU invocation/cache semantics](https://www.qemu.org/docs/master/system/invocation.html), [QEMU direct Linux boot](https://www.qemu.org/docs/master/system/linuxboot.html), and [ARM virt machine](https://www.qemu.org/docs/master/system/arm/virt).
