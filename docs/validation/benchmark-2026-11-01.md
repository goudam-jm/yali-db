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
