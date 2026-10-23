# Contributing

YaliDB is experimental. Start by reading the technical spec, format, and production-readiness checklist. Keep changes small and describe the operation/failure invariant they protect. A performance change must preserve the durability contract and include a reproducible measurement rather than an unsupported speed claim.

Run formatting, strict Clippy, tests, and documentation checks listed in the README. Storage changes need recovery/corruption/failure-path coverage; format changes need a compatibility story and independent golden fixtures. Keep unsafe code forbidden in project source. Do not add async/network dependencies to the core.

Use synthetic test data and temporary directories, and never commit credentials, production data, generated target files, or private database directories. Contributions are provided under the project's MIT OR Apache-2.0 licensing terms. Publication, supported release policy, and a community security contact will be established before a production release.
