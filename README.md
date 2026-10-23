# YaliDB

An experimental Rust byte-key store for small, durable per-user application state. Custom storage is part of the project's learning and open-source goals.

**Status: experimental, not production approved.** The engine is implemented; the [production release checklist](docs/production-readiness.md) records the evidence still required. Do not use it as the only copy of irreplaceable data.

## Implemented

- Owned byte values, TTL, global mutation versions, compare-and-swap and bounded prefix scans.
- Volatile mode and disk-backed mode with a keydir containing offsets rather than resident values.
- Checksummed append log, flush-before-success, process ownership lock, recovery, and generation compaction.
- Cloneable handles with serialized operations, resource limits, and basic owner diagnostics.
- An authenticated, loopback-only HTTP adapter for evaluation from other languages.

No replication, automatic failover, cross-key transactions, encryption, or online file-copy backup. HTTP limits/deadlines and simulated power-loss tests are implemented; independent review, platform validation and production measurements remain required.

## Run

Requires Rust 1.89 or newer. Linux is the intended first deployment target; macOS development is supported by local tests, with power-loss durability validation outstanding. Windows is unsupported. Use a trusted local filesystem and an existing parent directory.

```sh
cargo test --workspace --locked
cargo run -p yalidb-core --example user_state -- /tmp/yali-demo.yalidb
```

Run the example twice to exercise reopening. For your own code:

```rust
use yalidb_core::{Condition, Db};

fn main() -> yalidb_core::Result<()> {
    let db = Db::open("/tmp/my-state.yalidb")?;
    let receipt = db.put(b"user:demo:theme", b"dark", None, Condition::Any)?;
    let entry = db.get(b"user:demo:theme")?;
    assert_eq!(entry.map(|e| e.version), Some(receipt.version));
    Ok(())
}
```

`Db::memory()` uses the same operation semantics with volatile values. All handles cloned from a database share its mutex and file lock. Drop **every** clone to release ownership. A successful disk-backed mutation has already synchronized; shutdown is not the commit boundary.

## HTTP evaluation

The service only binds loopback and requires `YALIDB_TOKEN` containing at least 32 bytes. Generate and set a local token without committing it. Then run:

```sh
cargo run -p yalidb-server -- /tmp/yali-http.yalidb 127.0.0.1:7419
```

See the [wire API](docs/http-api.md) and [Python example](examples/python_client.py). Applications own user authentication/authorization and must not expose the generic database token to web/mobile clients. Multiple services can share this one owner; separate local stores per application replica do not share state.

## Design and operations

- [Technical spec](yalidb-tech-spec.md)
- [On-disk format](docs/storage-format.md)
- [Operations and recovery](docs/operations.md)
- [Production readiness](docs/production-readiness.md)
- [Linux storage validation](docs/linux-validation.md)
- [Disposable VM power-loss validation](docs/vm-power-loss-testing.md)
- [Coverage-guided fuzzing](fuzz/README.md)
- [Measured validation results](docs/validation/2026-10-23.md)
- [Rust learning path](docs/learning/README.md)

The learning guides remain exercises you can implement independently; the workspace now includes a reference implementation. No benchmark advantage or production reliability claim is implied by passing tests.

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo doc --workspace --no-deps --locked
cargo build -p yalidb-server --locked
python3 scripts/http-smoke.py
python3 scripts/http-hardening.py
python3 scripts/http-interop.py
```

The network tests start temporary loopback services, exercise slow clients and graceful shutdown, and verify shared state through Python and Node.js (Node 24 used for validation). See [HTTP resource limits](docs/http-hardening.md), [OpenAPI](docs/openapi.json), and [power-loss testing](docs/power-loss-testing.md). See [contribution guidance](CONTRIBUTING.md) and [security reporting](SECURITY.md). Licensed under either MIT or Apache-2.0, at your option.
