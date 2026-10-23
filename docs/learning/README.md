# Build YaliDB yourself

This learning path accompanies the [technical spec](../../yalidb-tech-spec.md). You write the engine; these documents supply concepts, contracts, exercises, and review checkpoints. They intentionally contain no completed engine implementation.

Start here, in order:

1. [Rust setup and milestone-one concepts](01-rust-foundations.md).
2. [Build the in-memory KV store](02-in-memory-kv.md).

Work one checkpoint at a time. Read only the references needed for that checkpoint, implement it, test it, and explain the ownership and behavior decisions in your own words. Compiler errors are part of the exercises.

Milestone one is complete only after the in-memory semantics work: byte keys and values, validation, versions, conditional writes, deterministic TTL, and bounded prefix scans. The first three operations are an early checkpoint, not the whole milestone. Thread sharing, disk persistence, HTTP, and distributed services come later.

For feedback, share your current checkpoint, your code or diff, the behavior you expected, and any compiler/test output. Ask for a hint, an explanation, or a review. The default should be explanation and review before replacement code.

As of 2026-10-23, the workspace includes a reference implementation following the owner's request to begin implementation. The guides remain a self-directed exercise path. For independent practice, use a separate scratch package outside this workspace, or study/reimplement one operation at a time without copying the reference. Do not rerun crate initialization over the existing `yalidb-core` package. Tool-installation instructions are conditional; inspect your current environment first.
