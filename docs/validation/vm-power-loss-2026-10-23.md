# Disposable VM power-loss evidence — 2026-10-23

**Local result: passed; physical-device production approval remains open.** The [harness guide](../vm-power-loss-testing.md) defines the virtual power boundary, reproduction commands, oracle and limitations.

## Local ARM64 campaign

The [machine-readable report](vm-powercut-arm64-2026-10-23.json) records QEMU 7.2.22, Debian guest kernel 6.1.0-54-arm64, Rust 1.99.0, source fingerprint `a4d2920f374558257a5d7d035a229d071461944f45c1f677951650f9e0770ff1`, boot artifacts' hashes and container image ID. QEMU TCG ran inside a restricted native ARM64 container on Docker's Linux 7.0.14-linuxkit VM. No host device or privileged container was used.

- **20 normal cut/recovery cases passed:** ten ext4 and ten XFS, covering acknowledgment, device-write, before-flush and persisted-before-reply boundaries during append and compaction workloads.
- Normal cases checked **204 externally recorded acknowledged API calls**, full maps, versions and sequence high-water marks. A known pending operation was the only permitted alternative state.
- The controller discarded **90,112 bytes of volatile device cache** across normal cases, in addition to killing each guest's RAM. Other cuts occurred after a completed flush and had no pending device-cache bytes.
- **Two negative controls detected the intended durability violation**, one per filesystem, after the virtual disk falsely acknowledged flushes. Infrastructure failures were not accepted as successful detection.
- Each of the 22 cases formatted its own 512 MiB image and used a separate fresh guest for recovery: **44 guest boots** total. The complete run took 81.446 seconds; this is a harness duration, not a DB performance benchmark.

Guest ext4 mounted `rw,noatime` with default ordered-data journaling/barriers. XFS mounted `rw,noatime,attr2,inode64,logbufs=8,logbsize=32k,noquota`. The host runner's filesystem is a distinct outer layer; it remained powered.

Five independent virtual-disk tests passed, including wire-level FUA, lost unflushed writes, lying flushes and refusal to overwrite an existing image. All three GitHub Actions workflows passed actionlint. Raw serial/block traces, ACK ledgers and checksummed pre-recovery disk archives are under ignored `validation-results/vm-powercut-arm64-final/`.

During harness development, guest boot exposed a missing CRC32C module; formatting exposed QEMU's larger NBD request size; XFS exposed background log initialization after sync. Those setup issues were corrected before the recorded successful campaign. No database engine code was changed or recovery assertion relaxed. A pre-workload baseline checkpoint is explicit in every report; there is no controller flush after the database opens.

## Remaining limits

This is actual guest-kernel/filesystem recovery following abrupt virtual RAM/device-cache loss. The volatile disk backend commits flushes under a lock; cuts do not interrupt a backing-file fsync or explore arbitrary device-sector reordering/tearing. The outer host and physical storage stay powered, and host fsync correctness remains assumed.

Actual deployment hardware/controller/VM-storage reset tests, parent-volume loss, journaling-filesystem ENOSPC, broader histories and independent storage review remain release gates. The prior [trace-model suite](../power-loss-testing.md) explores additional sampled reorderings; neither suite certifies physical device behavior. See the [production-readiness checklist](../production-readiness.md).
