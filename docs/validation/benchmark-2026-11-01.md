# Embedded benchmark evidence — 2026-11-01

**Result: initial bounded baseline passed; production performance approval remains open.** This is development evidence for the owner-selected 10,000-key, 1 KiB-value, single-writer profile. See the [methodology](../benchmarking.md) and [machine-readable measurements](benchmark-local-arm64-2026-11-01.json).

## Environment and verification

Native ARM64 Linux container, kernel 7.0.14-linuxkit, Rust 1.99.0, Python 3.11.2, SQLite 3.40.1. The fresh Docker volume was ext4 mounted `rw,relatime,discard`; the container used a read-only root, no network/capabilities, 2 CPU quota and 1 GiB memory limit. CPU model was not reported by this guest. This is not a bare-metal SSD measurement.

Source fingerprint: `2cc5d8a274eee0a86be554a183d26b5377e66cabbd1de6f6f076a3e0fd551775`. Three repetitions per engine alternated order and used seeds 42, 43 and 44. Across both engines, **66,000 durable writes and 6,000 warm reads completed**, with six fresh-process recoveries verifying **60,000 records**, all versions and sequence 11,000 per store. The kernel/page cache remained alive. Final-source raw TSVs and stderr are retained locally under ignored `validation-results/benchmark-reviewed-local-2026-11-01/run/`.

## Observed ranges across three repetitions

YaliDB: warm-get p99 **1.917–5.417 µs**; durable-overwrite p99 **296.792–498.125 µs**; fresh-process recovery **8.987–10.574 ms**; maximum observed full-rebuild pause **28.475 ms**; incremental instrumented process RSS **172.442–172.442 bytes/key**.

SQLite: warm-get p99 **15.500–35.209 µs**; durable-overwrite p99 **116.166–197.625 µs**; fresh-process recovery **0.223–0.282 ms**; maximum observed full-rebuild pause **131.706 ms**; incremental instrumented process RSS **258.867–258.867 bytes/key**.

These are ranges of per-repetition p99s, not a pooled p99. The JSON contains p50/p95/p99, maxima, phase throughput, per-stage RSS/high-water RSS and process I/O counters, plus sampled visible database-file peaks. Only three recovery/maintenance observations exist per engine; no recovery tail percentile or worst-case bound is claimed.

## Interpretation and remaining gates

The local samples show different tradeoffs: YaliDB has lower measured warm-read latency and process RSS growth; SQLite has lower durable-overwrite latency and faster startup in this profile. They use different client runtimes, so this does not establish a universal storage-engine winner or language advantage. SQLite implements the global version counter with an additional metadata-table update in each transaction. Both use the same synthetic bytes and access schedule.

Warm reads stayed below the PRD’s 1 ms hypothesis in these samples. That is not a production acceptance test: no workload SLO, contention/offered-load budget, cold-cache working set or target device has been approved. No performance optimization was made from these measurements.

SQLite’s full-rebuild observation includes checkpoint, VACUUM and another checkpoint; it is not the same algorithm as YaliDB compaction. RSS includes driver/latency-buffer overhead and excludes kernel page-cache memory. Directory sampling can miss peaks and unlinked temporary files. Process write-byte accounting is not physical-device write amplification. The full release measurement/budget gate remains open.

Validation: three Python tests cover nearest-rank statistics, output counts, workload vectors and atomic SQLite value/version/counter rollback; one Rust known-answer test checks the same generator vector. Formatting, strict Clippy on stable/MSRV and workflow lint passed during development. The initial read-only-container run exposed SQLite VACUUM needing a private writable temporary directory; both engines now receive a per-repetition TMPDIR, and SQLite receives the same SQLITE_TMPDIR.

The native ARM64/x86-64 [benchmark workflow](../../.github/workflows/benchmarks.yml) repeats this profile and uploads complete reports for 14 days. Its checks assert correctness and valid measurements, not a timing threshold on shared runners. Native results must be read with their own mount/CPU/compiler metadata; they must not be substituted for these local observations.

## Native hosted measurements

The [benchmark workflow run](https://github.com/goudam-jm/yali-db/actions/runs/41284603691) passed on native x86-64 and ARM64 for implementation commit `a5527f548976493477ec732308b071bed88a2b3e`. The [run record](benchmark-hosted-run-2026-11-01.json) and full [x86-64](benchmark-hosted-amd64-2026-11-01.json) and [ARM64](benchmark-hosted-arm64-2026-11-01.json) reports preserve all measurements. Both source fingerprints match the local final-source report; subsequent evidence-only documentation changes do not change that fingerprint.

Both hosts ran kernel 7.0.0-1017-azure, Rust 1.99.0, Python 3.12.3 and SQLite 3.45.1. x86-64 reported an Intel Xeon Platinum 8370C; ARM64 CPU model was not reported. Each repeated the complete workload three times per engine, checking 66,000 writes, 6,000 reads and 60,000 recovered records. Hosted totals are **132,000 writes, 12,000 reads and twelve verified fresh-process recoveries**.

Both hosts mounted ext4 with `rw,relatime,discard,journal_async_commit,nobarrier,errors=remount-ro,commit=30,data=writeback`. These are shared-runner development measurements. The configured API flush requests do not turn those mounts into a power-loss-qualified storage stack; do not copy those settings into a production recommendation.

x86-64 observed per-repetition warm-get p99 ranges of 3.188–3.407 µs for YaliDB and 27.269–70.451 µs for SQLite. Durable-overwrite p99 ranges were 348.427–600.574 µs and 257.506–543.598 µs respectively. Maximum observed full-rebuild pauses were 43.452 ms and 198.257 ms. Incremental instrumented process RSS was 169.984 bytes/key for YaliDB and 233.062 bytes/key for SQLite.

ARM64 observed warm-get p99 ranges of 3.376–3.512 µs for YaliDB and 39.288–42.320 µs for SQLite. Durable-overwrite p99 ranges were 342.848–407.432 µs and 282.264–317.193 µs respectively. Maximum observed full-rebuild pauses were 79.735 ms and 461.055 ms. Incremental instrumented process RSS was 169.984 bytes/key for YaliDB and 226.918–227.328 bytes/key for SQLite.

These independent runner environments are not a controlled ARM-versus-x86 comparison. The complete reports contain throughput, p50/p95/p99, process I/O accounting and sampled file-space metrics. Raw TSVs/stderr are downloaded under ignored `validation-results/benchmark-hosted-41284603691/` and retained by GitHub for 14 days. No timing-based CI approval or production SLO is inferred.

## Final verification follow-up

Review found that the initial drivers read the global sequence before maintenance and printed that cached value afterward. Row values/versions were checked after maintenance, but the counter needed a fresh read too. Both drivers now recheck final sequence and key count. A SQLite regression resets only the counter while preserving all rows and requires verification to fail. These checks happen after the timed intervals and resource snapshots; no engine or workload behavior changed.

The [corrected local report](benchmark-final-verifier-arm64-2026-11-01.json) records source `d16254b9ac9058a6047638cbd492a89b1eca1b5c75961ed4e734b80f3a02787e`: all six repetitions and 60,000 final records passed verification again. Four Python tests plus the Rust known-answer test passed. Earlier local/native measurement files above remain explicitly tied to their original source; they are not relabeled as measurements from this verification revision. [PR #2 checks](https://github.com/goudam-jm/yali-db/pull/2/checks) rerun native benchmarks and regressions for the corrected revision and retain their raw reports as artifacts.
