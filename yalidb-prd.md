# YaliDB — Product Requirements Document

**Version:** 1.0 (Draft)
**Date:** 2026-10-18
**Status:** Draft — pending owner review
**Author:** Product

> **The name.** *Yali* (யாளி) — the mythical lion-like guardian carved on Tamil temple doorways, standing watch over what lies within. YaliDB stands guard over your application's state.

---

## 1. Executive summary

Every application has a category of data that is small, per-user, constantly churning, and quietly miserable wherever it lives today: stream resume cursors, UI filters, favorites, app settings. Teams stash it in browser storage (fragile, device-bound), in their primary database (workload mismatch), or stand up Redis (operational overkill for the job).

**YaliDB is a very lightweight embedded key-value store in Rust for exactly this data.** It is not a better database — it is a *smaller* one for a *narrower* job: durable per-user state with expiry, near-zero operations, persistence included. You link it into your app, point it at a file, and it remembers. Nothing to deploy, nothing to configure, nothing to babysit.

**v1.0 target user:** the solo developer or small team running a single-box app (VPS, Fly.io machine, Raspberry Pi-class device) who needs server-side user state without operating infrastructure.

---

## 2. Problem statement

**P1 — Browser storage is fragile and device-bound.** `localStorage`/`sessionStorage` get evicted, don't sync across devices, and die with the browser profile. A "resume from last event" cursor stored there breaks the moment the user switches devices. Pain: broken resume experiences, lost UI state.

**P2 — Primary databases are the wrong shape for this workload.** Session state is high-churn tiny writes/reads with constant expiry — it competes with business data for connections, cache, and attention. Teams know it doesn't belong there but put it there anyway for lack of a better home. Pain: noisy-neighbor contention, schema churn for trivia.

**P3 — Redis is operational overkill for small teams.** A separate server to deploy, secure, monitor, back up, and pay for — all to hold 5-minute OTP codes and filter objects. For teams without a platform function, "run another server" is the most expensive sentence in the project. Pain: ops burden wildly disproportionate to the job.

**P4 — Embedded alternatives drag unneeded machinery.** SQLite is superb but it's a full SQL engine — query parser, planner, B-tree pages — for workloads that only ever do `get(key)` / `put(key, value, ttl)`. Pain: complexity and footprint you never use.

---

## 3. Target users

| Persona | Profile | Job to be done |
|---|---|---|
| **Indie hacker** (primary) | Solo dev, one VPS or Fly.io machine, deploys a single binary | "Persist my users' state without running anything else" |
| **Small SaaS team** (primary) | 2–10 engineers, no platform team, single-box or few-box deploy | "Give our user-state BFF a persistence layer with zero ops" |
| **Edge/IoT developer** | Raspberry Pi-class devices, kiosks, gateways | "Durable local state that sips resources" |
| **Prototyper** | MVP, hackathon, weekend project | "User state in 10 lines, no infra" |

**Explicitly not a target:** enterprise platform teams needing shared, highly-available state across fleets. They are well served by Redis/MongoDB and should stay there (see §7 and §10).

---

## 4. Vision & positioning

**Positioning statement:** *For developers who need durable per-user state without infrastructure, YaliDB is the embedded key-value store that remembers — unlike browser storage (fragile), unlike primary databases (wrong workload), unlike Redis (operational overkill).*

**Design principles:**
1. **Narrow beats broad.** Do one job (durable KV + TTL) completely; refuse all others.
2. **Boring durability.** Crash recovery is the normal path, not a special mode. Conservative fsync defaults; never trade safety for benchmarks.
3. **Zero config.** `open(path)` is the entire operations manual.
4. **Embed first.** The library is the product; the sidecar is a convenience.

---

## 5. Goals & non-goals

**Goals:**
- **G-01:** A Rust developer persists per-user state in under 10 lines of code with zero configuration.
- **G-02:** Zero committed-write loss on process crash (verified by kill -9 test suite).
- **G-03:** "Fast enough" performance: embedded p99 reads well under 1ms on local SSD; competitive with SQLite on the session-state workload (proven by published benchmarks, not claimed).
- **G-04:** Tiny footprint: single-file database, small binary increment, modest idle RAM.
- **G-05:** v1.0 published on crates.io with docs, examples for all four use cases, and an honest README (including the architectural boundary in §10).

**Non-goals (v1.x):**
- Replication, clustering, or any multi-node behavior.
- SQL, secondary indexes, reverse queries ("all users who favorited X").
- A managed cloud offering; enterprise compliance certifications.
- Serving as shared session infrastructure for load-balanced fleets (see §10 — this is a documented constraint, not a roadmap item).

---

## 6. Validated use cases

**UC-01 — Durable stream cursor (lead use case).** *Actor:* any app with live data streams. *Flow:* on each (or every Nth) event, `put("user:{id}:stream:{sid}", event_id, ttl)`; on reconnect, `get(...)` and resume from the cursor. *Why YaliDB:* tiny value, constant writes, read-once-per-session — the exact churn profile primary DBs hate and browser storage can't be trusted with. (This is the Kafka consumer-offset pattern, for web apps.)

**UC-02 — Per-user UI state.** *Actor:* data-heavy web apps (tables, dashboards). *Flow:* `put("user:{id}:page:{page}:filters", json_blob)` on change; `get(...)` on page load. *Why YaliDB:* cross-device filters with none of the ceremony; localStorage can't follow the user.

**UC-03 — Favorited URLs.** *Actor:* content apps. *Flow:* per-user favorite list under `user:{id}:favorites`. *Why YaliDB:* pure key lookups, written rarely, must persist. (Constraint: reverse lookups are out of scope — documented.)

**UC-04 — App configurations.** *Actor:* any app with per-user settings. *Flow:* settings JSON per user, read at login, written rarely. *Why YaliDB:* tidy offload from the primary DB; the mildest win of the four, but real.

---

## 7. Competitive landscape

| Alternative | Best for | Why it's wrong for this job |
|---|---|---|
| **Redis** | High-throughput shared caching, rich data structures, pub/sub | Separate server to operate; persistence (RDB/AOF) is a bolted-on trade-off; overkill for KV+TTL |
| **SQLite** | Embedded relational data, complex queries | Full SQL engine (parser, planner, B-trees) for get/put workloads; single-writer limits |
| **Memcached** | Pure ephemeral caching | No persistence at all — a restart wipes everything |
| **etcd** | Strongly-consistent config/service discovery | Consensus machinery for data that doesn't need consensus; heavy for session state |
| **Postgres JSONB** | "It's already there" | Workload mismatch (churn vs business data); session expiry via polling/cron is clumsy |
| **MongoDB** | Document queries, aggregations | Document DB for key lookups is a cannon for a mosquito; heavy footprint |

**Honest bottom line:** every incumbent wins on maturity, ecosystem, and battle-testing. YaliDB wins on operations (nothing to run), latency (in-process calls), fit (API shaped exactly like the job), and footprint. For this use case, the columns YaliDB wins are the ones that decide adoption.

---

## 8. Functional requirements

| ID | Priority | Requirement | Acceptance |
|---|---|---|---|
| FR-01 | P0 | Embedded library API: `open(path)`, `get`, `put` (with optional TTL), `delete`, `scan_prefix` | All four use cases implementable in ≤ 10 lines each |
| FR-02 | P0 | Byte-oriented keys and values (caller serializes) | Round-trip arbitrary bytes incl. empty values |
| FR-03 | P0 | Per-record TTL with lazy expiry on read | Expired keys invisible to readers; never resurrected |
| FR-04 | P0 | Crash-safe persistence: append-only log, fsync discipline, replay recovery, torn-tail truncation, CRC32 per record | kill -9 at any point → recovery with zero committed-write loss, 5/5 runs |
| FR-05 | P0 | Log rotation + compaction (merge live records, drop dead/expired) | Disk usage bounded under sustained churn |
| FR-06 | P1 | HTTP sidecar mode on localhost (get/put/delete) | Any language can use it; documented API |
| FR-07 | P1 | Key-naming conventions documented (`user:{id}:{thing}`) with examples per use case | In the guide, not just the API docs |
| FR-08 | P2 | Batch put/get operations | — |
| FR-09 | P2 | Metrics/logging hooks (op counts, compaction events) | — |

---

## 9. Non-functional requirements

- **NFR-01 Performance (targets, to be validated by benchmarks):** embedded p99 `get` well under 1ms on local SSD; competitive with SQLite on the session-state workload. Published benchmark harness vs SQLite ships in v1.0 — claims require numbers.
- **NFR-02 Footprint:** single-file database; small binary increment; idle RAM in the low megabytes.
- **NFR-03 Durability:** zero committed-write loss on process crash; torn writes detected and truncated, never silently accepted. Fuzz the record decoder.
- **NFR-04 Safety:** 100% safe Rust in the storage core (no `unsafe`); `cargo clippy -D warnings` and `cargo fmt --check` clean on every commit.
- **NFR-05 Compatibility:** stable on-disk format v1; any format change ships with a migration story, never silent breakage.
- **NFR-06 Operability:** zero configuration knobs required; human-readable startup/recovery logs; corruption produces a clear error naming the file and offset, not a panic.

---

## 10. Deployment & architectural boundary

**Supported deployments:**
1. **Embedded library (primary):** linked into the app; state is a file next to the app. Nothing to deploy.
2. **HTTP sidecar (secondary):** tiny binary next to the app on localhost, for non-Rust services.
3. **Single container (supported, not pitched):** one container with a volume mount, for Kubernetes-native teams.

**Documented architectural boundary (not a bug, not a roadmap item):** an embedded store is *durable local state*, not *shared infrastructure*. One server's embedded store is visible only to that server — using it as the session tier behind a load balancer implies server affinity (sticky sessions). Teams needing "any server serves any user" need a shared store with replication, which is explicitly out of scope. The reference architecture is the **dedicated user-state BFF**: a small service (embedding YaliDB) whose API is shaped around frontend needs (`GET /me/ui-state`, `POST /me/stream-cursor`), sitting beside the main API on the same box.

---

## 11. Developer experience

- **5-minute quickstart:** install → 10-line example → persistent state. The quickstart is tested in CI so it never rots.
- **Examples for all four use cases** (§6), copy-pasteable.
- **Error messages that suggest fixes** ("database file locked — is another process using this path?").
- **Docs on docs.rs** with a plain-language architecture page (no DB background assumed).

---

## 12. Roadmap

**Milestone 1 — Core engine (build phases 1–4):** in-memory KV + TTL → append-only log + recovery → CRC + torn-write handling → rotation + compaction. Exit: kill -9 suite green, compaction bounds disk.

**Milestone 2 — v1.0:** HTTP sidecar, SQLite benchmark shootout with honest write-up, docs + examples, open-source hygiene (README, MIT OR Apache-2.0, CI, crates.io).

**Milestone 3 — v1.x:** batch operations, metrics hooks, broader platform testing (ARM/x86), community-driven hardening.

**Explicitly not on the roadmap:** clustering/replication, secondary indexes, SQL. The non-goals in §5 are load-bearing — the PRD is the guardrail against scope creep.

---

## 13. Success metrics

- **Adoption (targets):** 500 GitHub stars within 6 months of v1.0; 1,000 crates.io downloads/month; ≥ 3 community-reported production uses.
- **Technical:** benchmark parity-or-better vs SQLite on the session workload; zero data-loss issues filed in the first 6 months.
- **Community:** issues responded to within 48h; ≥ 2 external contributors by month 6.

---

## 14. Risks & mitigations

| Risk | Mitigation |
|---|---|
| **"Why not SQLite?"** — the question every evaluator asks | Answer with positioning (§4) *and* published benchmarks, not adjectives |
| Data-loss bug destroys trust irreversibly | Fuzzing, kill -9 suite, conservative fsync defaults; durability is P0 over performance |
| Scope creep toward distributed features | This PRD's non-goals (§5) are the guardrail; any replication proposal needs a new PRD |
| Single-maintainer bus factor | Open-source hygiene from day one (docs, CI, examples); Apache-2.0/MIT invites contribution |
| The "boring" positioning undersells | "Boring durability" is the feature — market it as such |

---

## 15. Open questions

1. Exact performance targets — validate against SQLite in Milestone 1 benchmarks before committing to numbers in marketing?
2. Should `scan_prefix` support pagination for large per-user namespaces?
3. Sidecar auth: localhost-only binding, or token auth for shared-host setups?
4. At-rest encryption: P2 roadmap item or permanent non-goal?

---

## Appendix A — Glossary (plain terms)

- **Append-only log:** a file you only ever add to the end of; never modify the middle. Fast to write, trivial to recover.
- **Keydir (in-memory index):** a hash map from each key to exactly where its latest value lives in the log files — so reads need one disk seek.
- **Tombstone:** a special log record meaning "this key was deleted" — needed because you can't erase from an append-only log.
- **Compaction (merge):** rewriting the log to keep only live records, dropping deleted/expired/overwritten ones — keeps disk usage bounded.
- **CRC32:** a small checksum stored with each record; on recovery, a mismatched checksum means "this write was torn by a crash" → truncate it.
- **fsync:** telling the OS "actually put this on the disk now, not just in your buffers" — the difference between *claimed* durable and *actually* durable.
- **Torn write:** a crash mid-write leaving a half-written record at the log's tail — detected by CRC, handled by truncation.
- **Write amplification:** extra disk writes caused by internal work (like compaction) beyond the user's own writes — the tax every storage engine pays; the design goal is to keep it low.
