# 01 — Rust foundations for YaliDB

## Outcome

Become comfortable enough to build a synchronous, single-owner, in-memory byte-key store. You do not need to finish the entire Rust Book first. Alternate a small reading assignment with code you write yourself.

Use the [Rust Book](https://doc.rust-lang.org/book/) as the primary tutorial and the [standard library documentation](https://doc.rust-lang.org/std/) as a lookup reference. Tutorial examples teach the language; the contracts in the next guide define YaliDB behavior.

## Step 1 — Check your tools

On this Mac, inspection on 2026-10-18 found `rustup`, Rust 1.99.0, Cargo 1.99.0, and the stable Apple Silicon toolchain. The installed component list did not include rustfmt or Clippy. There is no need to reinstall Rust just to begin.

Run these yourself to confirm the current environment:

```sh
rustup show active-toolchain
rustc --version
cargo --version
rustup component list --installed
```

The tools have different jobs:

- `rustup` installs/selects compiler toolchains and optional components.
- `rustc` compiles Rust source.
- `cargo` builds packages, runs tests, and manages dependencies.
- `rustfmt` formats source; `cargo fmt` invokes it for a package.
- `clippy` checks common mistakes and questionable patterns.

Install the two missing development components for your selected toolchain:

```sh
rustup component add rustfmt clippy
```

On a different machine without Rust, follow the official [installation guide](https://doc.rust-lang.org/book/ch01-01-installation.html). On macOS, a missing linker can require Apple's command-line tools; run `xcode-select --install` if the linker is missing. An editor with rust-analyzer is useful for inline diagnostics and navigation, but terminal checks remain the shared verification path.

**Checkpoint:** explain why having Cargo available is different from having an editor extension available.

## Step 2 — Create a library package

The project now contains `yalidb-core` (implementation started on 2026-10-23). To work on that package, enter its directory and run `cargo check`/`cargo test`; skip initialization. The commands below describe the original blank-project exercise only. For an independent reimplementation, create a differently named scratch package outside the current workspace.

Original blank-project setup, only when the package does not already exist:

```sh
cd /Users/goudammuralitharan/Documents/Ideas/YaliDB
cargo new --lib yalidb-core --vcs none
cd yalidb-core
cargo check
cargo test
```

Run `cargo new` once. If the directory already exists, inspect it rather than rerunning initialization over your work. Keeping a library subdirectory preserves the PRD, spec, and learning documents at the project root; a workspace can be added when another package is needed.

Read [Hello, Cargo!](https://doc.rust-lang.org/book/ch01-03-hello-cargo.html) and [packages and crates](https://doc.rust-lang.org/book/ch07-01-packages-and-crates.html).

Inspect:

- `Cargo.toml`: package metadata, Rust edition, and dependencies. Keep Cargo's generated edition. This milestone needs only the standard library.
- `src/lib.rs`: library root and initial generated example/test. Replace the example as your own implementation takes shape.
- `Cargo.lock`: resolved dependency versions; it is not the package manifest.
- `target/`: generated build output; keep it out of version control.

A library exposes functionality to callers; it does not need a `main` function. Run examples later with Cargo's example support rather than turning the engine into a terminal app.

Use this loop throughout:

```sh
cargo check
cargo test
cargo fmt
cargo clippy --all-targets -- -D warnings
```

`cargo check` quickly checks ordinary build targets without producing the final binary; `cargo test` also compiles and runs tests. `cargo fmt` changes formatting. At a checkpoint, use `cargo fmt --check` to verify formatting without changing files. Warnings-as-errors may be inconvenient during an unfinished step; resolve them before calling the checkpoint complete.

**Checkpoint:** the generated test passes, and you can locate the file that contains it. This proves setup works, not that the database works.

## Step 3 — Learn syntax and data modeling

Read Book [chapter 3](https://doc.rust-lang.org/book/ch03-00-common-programming-concepts.html), [structs](https://doc.rust-lang.org/book/ch05-00-structs.html), and [enums and matching](https://doc.rust-lang.org/book/ch06-00-enums.html).

Focus on immutable bindings by default, `mut`, function parameters/returns, expression values, `impl` blocks, `pub`, and exhaustive `match` expressions. A semicolon can turn a returned expression into a statement; read the compiler's expected/found types when that happens.

For YaliDB:

- A `struct` models a record with named fields.
- An `enum` models mutually exclusive states, such as unconditional write versus version-conditional write.
- `Option<T>` distinguishes missing from present. A present empty value is not missing.
- `Result<T, E>` distinguishes a valid operation result from a failure such as an invalid key.
- `&self` borrows the store for reading; `&mut self` borrows it exclusively for mutation.

**Exercise:** model a toy preference with a byte value and optional version, construct two instances, and inspect them using a derived `Debug` implementation. Then model three write-condition alternatives with an enum. Do not build database methods yet.

**Explain:** why is an error string a poorer internal representation of a condition than a typed enum?

## Step 4 — Ownership, borrowing, and byte slices

Read [ownership](https://doc.rust-lang.org/book/ch04-01-what-is-ownership.html), [references and borrowing](https://doc.rust-lang.org/book/ch04-02-references-and-borrowing.html), and [slices](https://doc.rust-lang.org/book/ch04-03-slices.html). Revisit these as compiler errors arise.

The key API choices are:

- `Vec<u8>` owns a growable byte buffer. The map will own stored keys and records.
- `&[u8]` borrows bytes owned elsewhere. A lookup can inspect caller bytes without owning or copying them.
- `b"theme"` is a byte-string literal; `.to_vec()` creates an owned vector from those bytes.
- Moving a vector transfers ownership of its buffer; cloning copies its contents into a separate allocation.
- `String` requires valid UTF-8; arbitrary database bytes do not. Converting every key/value to text would weaken the contract.

Use owned return values for the first database API. A read copies the selected value so the caller can keep it while subsequently mutating or dropping the store. This has a measurable allocation cost; accept that simple contract first. Do not solve early borrowing problems by adding reference-counted buffers everywhere.

**Exercises, separate from the engine:**

1. Move a vector into a helper function, then try to use the original binding. Read the error before changing anything.
2. Change that helper to borrow a slice and observe that the original stays usable.
3. Clone the vector, mutate the clone, and prove the original bytes did not change.
4. Hold a borrowed map value while attempting a mutation and using the borrow afterward. Explain why Rust rejects this. Then return an owned copy instead.

**Checkpoint:** explain who owns the bytes before insertion, after insertion, and after an owned get result is returned. You do not need explicit lifetime parameters for this first API.

## Step 5 — Collections and errors

Read [vectors](https://doc.rust-lang.org/book/ch08-01-vectors.html), [hash maps](https://doc.rust-lang.org/book/ch08-03-hash-maps.html), and [recoverable errors](https://doc.rust-lang.org/book/ch09-02-recoverable-errors-with-result.html).

Keep the [HashMap API reference](https://doc.rust-lang.org/std/collections/struct.HashMap.html) nearby. Explore `insert`, `get`, `remove`, `iter`, and `retain`. Start with `HashMap<Vec<u8>, Record>`. A byte slice can be used for borrowed lookup into vector keys; avoid allocating a vector merely to look up a key.

Hash-map iteration order is unspecified. A prefix scan requiring sorted results must explicitly order them. Expected constant-time map lookup describes the map work; hashing the key and copying the value still cost time proportional to their bytes.

Use named error variants for invalid sizes, conflicts, time overflow, and version exhaustion. Read about `?` for propagating failures. A missing key is a normal successful result, not an exceptional crash. `unwrap`/`expect` are useful in tests where failure should fail the test; avoid them on public operation inputs.

**Exercise:** make a small map of owned byte keys and values; look up using a slice, overwrite a key, and remove it twice. Print or assert what each map method returns. Next, write a length-validation helper returning a typed error. This is practice for your own engine code.

## Step 6 — Tests, traits, and deterministic time

Read [writing tests](https://doc.rust-lang.org/book/ch11-01-writing-tests.html) and [test organization](https://doc.rust-lang.org/book/ch11-03-test-organization.html). Use unit tests beside internals and integration tests in `tests/` for the public API. Assertions should describe visible behavior rather than duplicate every private implementation detail.

Read the introductory [traits section](https://doc.rust-lang.org/book/ch10-02-traits.html) when you reach TTL. You only need a small clock abstraction initially: a trait defines the operation that both a real clock and a fake clock can supply. Generics allow a store to use a chosen clock type without depending on real time in tests. Avoid reading the entire advanced-traits material now.

Look up [`Duration`](https://doc.rust-lang.org/std/time/struct.Duration.html), [`SystemTime`](https://doc.rust-lang.org/std/time/struct.SystemTime.html), and `checked_add`/checked integer conversion as needed. A duration is an elapsed amount; an absolute timestamp identifies a point in time. These are different concepts even when both are represented as milliseconds.

**Exercise:** write a helper receiving a current timestamp and optional positive TTL in milliseconds and returning an optional checked deadline. Test absence, zero, normal addition, and overflow without sleeping. This helper is an exercise, not the whole clock design.

## What can wait

Borrow-checker fluency grows with practice. For milestone one you do not need async/Tokio, networking, serialization crates, macros beyond ordinary derives/tests, unsafe code, memory mapping, custom allocators, lock-free collections, or consensus. Shared-thread execution comes after the single-owner API; versions and CAS can be learned through sequential stale-client simulations first.

Proceed to [the in-memory KV build guide](02-in-memory-kv.md) once setup works and you can explain owned bytes, borrowed slices, `Option`, and `Result`. Read the remaining topics just before their corresponding implementation step.
