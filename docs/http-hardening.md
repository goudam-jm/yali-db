# HTTP resource and shutdown contract

The server now uses Hyper HTTP/1 on Tokio's single-thread asynchronous I/O runtime. One dedicated ordinary thread owns storage dispatch. Hyper handles framing; the application does not implement a custom HTTP parser. The storage core still has no runtime/network dependency.

## Admission and memory bounds

Defaults: at most **64 connection tasks**, **16 queued storage requests**, and **one executing storage request**. A new accepted socket without a connection permit is immediately closed without spawning a task or returning an HTTP status. Kernel listen-backlog behavior is controlled by the OS; the limit bounds application-owned active connection tasks, not every TCP handshake in the kernel.

Once a request has passed authentication/framing/body limits, it is admitted with nonblocking queue insertion. A full queue returns 429 without enqueuing or mutating. A closed queue returns 503. There is no unbounded blocking pool or thread per socket. Each connection has at most one active HTTP/1 service request; keep-alive is supported without HTTP/2, upgrades, or concurrent multiplexing.

Headers are capped at 32 fields and a 16 KiB Hyper read buffer. Request bodies are capped at 96 KiB, both by declared length and a bounded streaming collector. A response contains at most the core's 100-entry/1 MiB raw scan page, plus JSON/base64 expansion. Slow readers can retain bounded responses on admitted connections; peak memory is therefore proportional to configured connection count, queue capacity, and response size, not just idle RSS. No fixed production RSS claim is made yet.

## Deadlines and lifetime

- Header and keep-alive idle-header read deadline: 5 seconds.
- Entire body collection: 5 seconds, not reset by each incoming byte. Timeout returns 408 and closes the connection.
- Write/flush window: 5 seconds across progress writes until a flush completes. Progress bytes do not reset that timer. A blocked flush is timed too. Hyper streams an already materialized, bounded response; a connection lifetime independently bounds the whole exchange.
- Storage admission-to-response wait: 10 seconds. A timed-out or disconnected caller closes its reply channel. A queued job whose reply is closed or whose deadline has expired is skipped before dispatch. An operation already executing is allowed to finish; cancellation cannot safely undo a write/fsync. Timeout returns 503 with an unknown-mutation-outcome flag when a response can still be delivered.
- Connection lifetime: 60 seconds, including busy keep-alive connections. At most 100 requests per connection; the hundredth response requests `Connection: close`.
- Shutdown drain: 30 seconds total.

These limits can be changed with validated environment variables: `YALIDB_MAX_CONNECTIONS` (1–4,096), `YALIDB_QUEUE_CAPACITY` (1–1,024), `YALIDB_HEADER_TIMEOUT_MS`, `YALIDB_BODY_TIMEOUT_MS`, `YALIDB_WRITE_TIMEOUT_MS`, `YALIDB_REQUEST_TIMEOUT_MS`, `YALIDB_SHUTDOWN_TIMEOUT_MS` (each 50–60,000), and `YALIDB_CONNECTION_LIFETIME_MS` (100–3,600,000). Increasing limits increases the resource envelope; it does not establish capacity or performance. Defaults require no configuration.

## Framing and authentication

Every route, including health, requires the bearer token. Only a loopback bind is accepted. Require origin-form targets and one Host field, reject transfer encoding/chunked requests, ambiguous content lengths, Expect handshakes, duplicate authorization/content-type fields, unsupported methods, and oversized headers/bodies. Application errors close their connection so an unread rejected body cannot become a subsequent request. Hyper can reject malformed framing before application dispatch, including by closing without a JSON body; those failures are not a stable JSON error API.

No public bind, TLS termination, browser token distribution, tenant authorization, or security-sensitive TTL behavior is added. Applications still own authentication and per-user authorization. A protocol/security review remains required before production approval.

## Shutdown and health

SIGINT/SIGTERM stops accepting connections, marks readiness false, and asks Hyper to stop new requests on each connection while draining in-flight requests. Existing body/storage/write deadlines continue to apply. After connection tasks finish, the sender is dropped and the storage thread drains eligible work and releases the database. A global drain deadline aborts outstanding connection tasks; storage I/O already running is not unsafely canceled. Exceeding that deadline exits with failure and requires reopening to resolve ambiguous outcomes.

Readiness uses a cached worker-health flag and receiver state instead of synchronously acquiring the database mutex from the I/O runtime. Liveness does not wait for storage. Both still depend on admission to a connection slot. An indefinitely stalled kernel write/fsync can consume the single storage worker; HTTP calls time out, but the runtime remains responsive and cannot claim storage recovered.

## Evidence

`cargo test -p yalidb-server` covers queue saturation, deadline ambiguity, skipped expired/canceled work, and dribbling-write/flush deadlines. `scripts/http-hardening.py` tests real slow headers/bodies, framing attacks and size/count limits, keep-alive idle expiry, connection saturation/recovery, connection lifetime/request caps, and an in-flight write during graceful shutdown followed by reopen. `scripts/http-interop.py` exercises a shared record through Node and Python with CAS. `docs/openapi.json` describes the application wire schema.

These tests are bounded local correctness evidence. They do not replace independent security review, prolonged overload/RSS measurements, broader interoperability testing, or validation of OS/device behavior. See the production checklist for remaining gates. Configuration and timeout APIs are based on the [Hyper server builder](https://docs.rs/hyper/latest/hyper/server/conn/http1/struct.Builder.html) and [Hyper shutdown model](https://hyper.rs/guides/1/server/graceful-shutdown/).
