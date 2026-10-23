# YaliDB coverage-guided fuzzing

Three libFuzzer targets use the actual engine with an opt-in, undocumented `fuzzing` feature:

- **decode:** raw manifest/header/frame parsing, plus checksum-aware copies so payload/metadata mutations can reach validation beyond CRC checks. Original bytes are always tested too; production checks are never disabled.
- **recovery:** the same replay function used by `Db::open`, with raw and checksum-aware logs, plus structured valid mutation histories checked against a reference map. Filesystem publication/sync ordering is covered separately by the crash-image and Linux harnesses.
- **state:** byte ownership, puts, CAS conflicts, absent-key semantics, missing deletes, global versions, reads and paginated scans compared with an independent ordered map. This target currently uses permanent records; deterministic TTL tests remain in the ordinary suite. It does not establish exhaustive concurrent correctness.

Inputs are capped at 128 KiB. Each target also enforces that bound independently. Structured histories are bounded in operation count and key cardinality; no fuzz target accesses production directories or user data. The shared replay code retains the same on-disk bounds, corruption checks and snapshot/sequence rules as normal startup. Only the normal owner truncates/synchronizes files.

## Setup and run

```sh
rustup toolchain install nightly-2026-10-22 --profile minimal --no-self-update
rustup component add rust-src --toolchain nightly-2026-10-22
cargo install cargo-fuzz --version 0.13.2 --locked
cargo fetch --manifest-path fuzz/Cargo.toml --locked
python3 scripts/run-fuzz.py --seconds 300 --output validation-results/fuzz-run
```

The runner uses the pinned nightly, AddressSanitizer, `--build-std`, debug/overflow checks from cargo-fuzz defaults, a 1 GiB per-target RSS cap, a 10-second per-input timeout, a fixed recorded seed and a finite time budget. By default the three targets run concurrently; `--parallel 1` runs them serially. The reported fuzz duration excludes compilation and can exceed the budget slightly because libFuzzer checks elapsed time at intervals.

Named seeds under `seeds/` were independently encoded with Python `struct`/`zlib`, including maximum-size values, snapshots, tombstones, TTL metadata and malformed/truncated histories. `scripts/seed-fuzz.py` prepares seeds and adds missing inputs to `corpus/` without erasing a grown corpus. Corpus growth and artifacts are ignored by Git; preserve them through CI cache/artifact storage. The separate fuzz lockfile resolves the production CRC dependency to the same version as the workspace lockfile.

The runner stores commands, source fingerprint, toolchain, logs, execution counts, libFuzzer coverage counters, memory statistics, findings and corpus-size measurements. After a successful campaign it runs `cargo fuzz cmin` and copies the minimized corpus into the report. Coverage counters are **not a percentage of all code paths**; sanitizer RSS includes instrumentation/corpus overhead and is not the database's production footprint.

If a campaign finds a crash, timeout or sanitizer issue, retain its artifact and source/toolchain identity. Reproduce with `cargo +nightly-2026-10-22 fuzz run --build-std TARGET ARTIFACT_PATH`, minimize with `cargo ... fuzz tmin`, fix the issue, and add the minimized input as a normal regression fixture/test. Never delete an inconvenient corpus input or weaken an assertion merely to make a run pass. No crash was observed in the recorded campaign, so no discovered-crash regression is claimed; independent compatibility fixtures are included in ordinary tests.

## CI budgets

The storage/fuzz workflow runs a 60-second campaign per target on push/PR, saves/restores grown corpora and uploads logs/findings/minimized corpora on completion. Manual `workflow_dispatch` defaults to 1,800 seconds per target, with a hard maximum of 1,800 in that workflow and a job timeout. No recurring paid schedule is enabled implicitly. Hosted jobs run in [goudam-jm/yali-db](https://github.com/goudam-jm/yali-db/actions/workflows/storage-and-fuzz.yml); see the recorded evidence below.

The bounded local campaign is initial evidence, not an overnight soak or proof that no bugs remain. Longer native Linux sanitizer campaigns, additional seeds, TTL/concurrency histories, filesystem error schedules and independent review remain production gates. See the [recorded results](../docs/validation/2026-10-23.md), [Rust Fuzz Book](https://rust-fuzz.github.io/book/cargo-fuzz/guide.html), and [tutorial](https://rust-fuzz.github.io/book/cargo-fuzz/tutorial.html).
