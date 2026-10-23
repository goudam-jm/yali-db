# 02 — Build the in-memory KV store

## Contract and boundaries

You will implement this yourself in `yalidb-core`. This guide gives behavior and a suggested sequence rather than finished method bodies. Use [the foundations guide](01-rust-foundations.md) for Rust references and [the spec](../../yalidb-tech-spec.md) for the broader design.

**Reference implementation added 2026-10-23:** following the owner's request to begin implementation, `yalidb-core` now exists. These instructions remain independent learning exercises; use a scratch crate outside the workspace if you want to implement them without reading the reference. The reference additionally includes synchronization and disk storage; those are outside this exercise's completion criteria.

The final milestone-one store owns byte keys and values, validates inputs, returns owned entries, versions successful mutations, supports conditional writes and TTL, and provides bounded sorted prefix scans. It is synchronous and has one owner. All state disappears when the store is dropped or the process exits. Do not call it persistent or crash-safe.

Thread sharing and concurrent-history checks are explicitly deferred to the shared-handles milestone. Sequential CAS tests establish the conditional-write semantics now; they do not prove a future concurrent implementation is correct. No disk paths, locks, append logs, server, or external crates are needed here.

## Step 1 — Write down the invariants

Create a short design note in your package, in your own words, describing:

1. Each key has at most one current record.
2. Missing and present-with-empty-value are different states.
3. Only successful state-changing mutations consume a version; versions never wrap or repeat within a store instance.
4. Invalid input, a failed condition, or arithmetic overflow changes neither data nor the mutation sequence.
5. Expired records are logically absent everywhere, including condition checks and scans.
6. Callers cannot mutate stored bytes through a returned value.

An invariant is something that must remain true after every operation, not merely a happy-path example. Later steps refine these rules; keep this note aligned with your tests.

**Stop and explain:** how could a get implementation accidentally make invariant 6 false in a language without Rust's borrowing rules?

## Step 2 — Implement the smallest byte-KV API

Start with a private map, a constructor, and three operations. Suggested temporary signatures:

```rust
pub fn new() -> Self;
pub fn put(&mut self, key: Vec<u8>, value: Vec<u8>);
pub fn get(&self, key: &[u8]) -> Option<Vec<u8>>;
pub fn delete(&mut self, key: &[u8]) -> bool;
```

These are signature sketches, not a standalone compilable file. You choose the store's type name and write its struct, `impl`, and method bodies. Keep the map private. Accept owned buffers on put and borrowed keys on lookup/removal. Clone only the returned value on get. These signatures will evolve below; early refactoring is intentional.

Write tests for:

- Get on an empty store returns missing.
- Put then get returns exactly the stored bytes.
- Overwrite changes that key without changing another key.
- Present empty value differs from a missing key.
- Keys/values containing zero and invalid UTF-8 bytes round-trip.
- Delete present returns true; delete again returns false.
- Mutating an owned get result does not change the stored value.
- Two distinct byte buffers with equal contents refer to the same key.

Keep tests small enough that a failure identifies one behavior. `b"theme".to_vec()` is useful test input; include actual non-text bytes too.

**Hint if stuck:** inspect the map method's return type. You often already have an `Option` that can be transformed or checked, rather than needing a separate existence lookup.

**Checkpoint:** explain the difference between cloning a value for an owned read result and cloning the entire map to bypass a borrow error.

## Step 3 — Add limits and explicit errors

Introduce named constants for a nonempty key of at most 1,024 bytes and a value of at most 65,536 bytes. These are byte counts. Empty values remain valid. Change public mutation results to `Result`; change get to `Result<Option<...>, Error>` so invalid keys are distinguishable from valid missing keys.

Validate nonempty/size-limited keys consistently for get, put, and delete. Only put needs value validation. The empty-prefix scan rule later is deliberately different from empty-key validation.

Use an error enum with meaningful variants, derive the traits needed for assertions, and implement `Display` and `std::error::Error` as a follow-up within this step. Never put user values in error messages. You do not need an error-handling dependency.

Test each exact boundary and one byte beyond it. Test that an invalid replacement preserves the prior value. Perform validation before a mutation becomes visible.

**Checkpoint:** why is returning `None` for an invalid oversized key ambiguous?

## Step 4 — Introduce records and versions

Replace map values with a private record holding owned value bytes and a `u64` version. Return a public owned entry from get with its value and version. Keep the internal map private; do not expose mutable record references.

Start the global mutation sequence at zero and assign version one to the first successful put. Every successful put consumes a new sequence number, even if the bytes are identical. A delete of a present record also consumes a sequence number, even though it leaves no record behind. A delete of a missing key returns false without consuming one. Do not reset the sequence when the map becomes empty.

Use checked increment. Compute everything that could fail before replacing/removing data or advancing the sequence. At exhaustion, state-changing mutations return `VersionExhausted`; reads and nonmutating outcomes still work.

Test put/overwrite/delete/recreate and operations on different keys. The recreated key must receive a newer version. Use a private unit-test setup to put the sequence near `u64::MAX`; do not add a production public setter solely for this test. Verify that overflow leaves the data and sequence unchanged.

**Checkpoint:** why is a per-key counter that resets on deletion unsafe for a stale client?

## Step 5 — Conditional writes and deletes

Add a condition enum with `Any`, `Absent`, and `Version(u64)` for put. Delete supports unconditional or version matching; choose a separate delete condition type so unsupported combinations cannot be constructed.

The behavior is:

- `Any` put inserts or replaces.
- `Absent` put succeeds only when the key is logically absent.
- `Version(v)` put/delete succeeds only when a live record with that version exists.
- Unconditional delete of an absent key succeeds as a no-op and returns false.
- Version-conditional delete of an absent key returns a conflict.
- A conflict preserves both data and sequence.

Validate input first. Evaluate the condition next. Allocate the new sequence only after the condition succeeds and only if a state change will happen. The TTL step adds checked deadline construction before any mutation is committed.

Simulate two clients sequentially: both read version V; client A successfully replaces it; client B attempts a write using V and gets a conflict; B reads again and deliberately merges or retries. Test this sequence, not just isolated enum branches.

Also test create-if-absent on present/missing keys and deletion followed by recreation and a stale conditional write. The latter catches version reuse errors.

**Checkpoint:** CAS protects against a stale version; it does not decide how two favorites lists should be merged. Explain which responsibility belongs to the application.

## Step 6 — Add deterministic TTL

Now introduce a clock abstraction. Suggested shape: a clock supplies a fallible absolute timestamp in UTC milliseconds as `u64`. The store owns a chosen clock type. A real clock converts `SystemTime` relative to the Unix epoch with checked conversion; a fake clock supplies controlled values. A clock error changes no data or sequence.

You can first pass the current time to a private helper to understand the logic, then introduce a `Clock` trait and a generic store. For tests, supply a small scripted clock returning known readings on successive calls, or a shared fake clock handle. If using shared interior mutability feels premature, use scripted readings; do not add thread synchronization just to test a single-owner store.

Add optional `expires_at_ms` to records and public entries. Let put take `Option<u64>` TTL milliseconds for this milestone. This unsigned type cannot express negative TTL; reject zero and use checked deadline addition. `None` means permanent. A later public API can adopt `Duration` while keeping the same semantics.

For each valid operation, sample the clock once and use that same effective time for the entire operation. Clamp it to the last effective time observed by this store instance so a backward clock reading cannot revive an expired record within that instance. Advancing this clock-observation floor is allowed even when a condition conflicts; it is not a data mutation or a version allocation. Validate input before sampling time.

A record is expired when `now >= expires_at_ms`. Treat it as missing in get, scan, put conditions, and delete conditions. Replacing a TTL record without TTL clears its deadline. A successful replacement with TTL starts a new deadline based on that operation's time.

Keep get logically read-only with respect to records. If its signature remains `&self`, use narrowly scoped interior mutability for the clock floor after learning `Cell`, or move to `&mut self` for this single-owner milestone and document the choice. Prefer the latter if it keeps the reasoning simple; the future shared API will revisit synchronization. Do not silently consult wall time inside a test helper.

Lazy expiry hides entries; it need not remove them. Add an explicit `purge_expired` maintenance method, sampling time once, that removes expired records without consuming user mutation versions. This is memory cleanup of already-invisible state. Physical removal also prevents those specific old records from becoming visible later; this milestone still has no restart behavior.

Tests must use controlled time, never `sleep`:

- Visible just before deadline; absent at the exact deadline and afterward.
- Permanent entry stays visible as time advances.
- Zero TTL and overflowing deadline return errors without replacing an existing value or consuming a version.
- `Absent` put succeeds over an expired record; old-version CAS fails against it.
- Deleting expired records behaves like deleting missing records.
- Replacement without TTL becomes permanent; replacement with TTL gets a new deadline.
- A backward clock reading does not revive a previously expired entry in this instance.
- Purge preserves live/permanent records and removes expired records.

**Checkpoint:** distinguish logical absence, physical removal, and durability. None of them implies the others automatically.

## Step 7 — Prefix scans with bounded output

Add a scan method accepting a byte prefix, an optional exclusive `after_key`, and a page limit. Return owned keys and entries, ordered lexicographically by bytes, plus an optional continuation key.

Rules for this milestone:

- Empty prefix matches every live key; oversized prefixes are invalid.
- `after_key`, when supplied, must be a valid nonempty key, but need not still exist or match the prefix. Include matching keys strictly greater than it.
- Accept limits 1 through 100; reject zero and larger limits.
- Sample one time per page and exclude expired entries.
- Limit the sum of returned key and value bytes to 1 MiB; metadata/wire overhead is excluded from this definition. Stop before exceeding either count or byte budget.
- Return `next_key` equal to the last emitted key only if another matching live key remains. Otherwise return no continuation.

Since one maximum-size entry fits within the byte budget, you can always make progress when eligible keys remain. Request the next page with the same prefix and returned continuation key. A mutable store between pages can cause newly inserted earlier keys to be missed; there is no snapshot promise.

For the first correct version, gather matching borrowed candidates, sort by key, then select the page and clone only the returned items. This bounds output but uses temporary memory proportional to matching keys. State that limitation. Before marking the full milestone complete, revise it to retain at most `limit + 1` smallest eligible candidate references during the map traversal, then sort/select them. Count/byte stopping may leave more than one retained candidate unreturned; any remaining candidate proves a next page exists. A bounded sorted vector is adequate at a 100-entry limit; a heap is optional. Inspect all map entries, but avoid a temporary vector containing the entire database.

Test ordering with adversarial insertion order, binary prefixes, empty prefix, no matches, exclusive continuation, missing continuation key, expired matches, exact final page, count limit, and byte limit. Verify concatenating pages over an unchanged store returns every matching live entry exactly once. Add/delete keys between two pages and document the expected weak consistency.

**Checkpoint:** scans are linear in map traversal plus candidate selection work. Explain why a hash map does not provide an efficient sorted range query.

## Step 8 — Make the library understandable to another person

Split files only where it improves navigation. A reasonable eventual layout is `lib.rs` for exports, `store.rs` for operations, `error.rs` for errors, and `clock.rs` for time. Keeping the early implementation in one file is fine. Learn `mod`, `pub`, and `pub(crate)` when extracting modules.

Write rustdoc on public types/methods covering missing values, limits, owned copies, TTL boundaries, version behavior, and lack of persistence. Add a small `examples/in_memory.rs` yourself showing user-state put/get and a conditional update. Avoid putting real user identifiers in sample output.

Run from the package directory:

```sh
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo doc --no-deps
cargo run --example in_memory
```

The example command assumes you have created the example file. Do not treat formatting or lint success as proof of database semantics.

## Definition of done

- All operation contracts above have behavior tests, including failed-operation nonmutation and exact TTL boundaries.
- Tests use deterministic clocks and no network/disk resources.
- Returned entries own their bytes; the map and records stay private.
- Version allocation survives delete/recreate within the instance and never wraps.
- Scan output and temporary candidate storage are bounded as described.
- Invalid user input returns errors; no input-dependent `panic!`, `unwrap`, or `expect` in the operation path.
- Documentation calls this an experimental in-memory implementation and states the memory cost: all stored values live in RAM here, unlike the later disk-backed keydir design.
- You can explain a failed stale update, an expiry decision, and each method's ownership choices without reading its body line by line.

When these pass, pause for a code/design review before adding persistence. The next milestone introduces a record codec and append log, not HTTP or replication. Save your own brief notes on what Rust made clear, what was difficult, and which invariants your tests actually establish.
