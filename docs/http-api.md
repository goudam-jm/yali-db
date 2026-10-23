# HTTP evaluation API v1

**Experimental:** the adapter uses Hyper/Tokio with bounded connection and storage admission, header/body/write deadlines, idle and lifetime limits, and graceful shutdown. See the [resource contract](http-hardening.md) and [OpenAPI schema](openapi.json). Loopback binding remains mandatory; protocol/security review and production measurements remain outstanding. No CORS, TLS endpoint, public bind, or automatic write retry is supplied.

All routes require exactly one `Authorization: Bearer TOKEN` header. Set `YALIDB_TOKEN` before starting the service; it must contain at least 32 bytes and no control characters. Use `Content-Type: application/json` and an explicit `Content-Length` on operation requests; transfer-encoded/chunked requests are rejected with 400 (or by the HTTP parser before dispatch). JSON has strict fields; unknown/duplicate fields are rejected by the typed decoder.

Keys, values, prefixes and continuation keys use unpadded base64url. Versions and returned absolute expiry milliseconds are decimal strings (or null for no expiry) to avoid JavaScript precision loss. TTL input is a positive integer number of milliseconds fitting `u64`; JavaScript callers must stay within their exact-integer range. TTL absence/null means permanent. Missing/expired get returns a normal success with `found: false`.

## Operations

`POST /v1/get`:

```json
{"key":"aw"}
```

Returns `{"found":false}`, or:

```json
{"found":true,"entry":{"value":"dg","version":"1","expires_at_ms":null}}
```

`POST /v1/put`:

```json
{"key":"aw","value":"dg","ttl_ms":60000,"condition":{"kind":"absent"}}
```

Returns `{"version":"1","expires_at_ms":"ABSOLUTE_UTC_MILLISECONDS"}` with actual numeric timestamp text in place of the descriptive label. Omit `condition` or use `{"kind":"any"}` for unconditional replacement. For CAS use `{"kind":"version","version":"1"}`. Versions must be canonical unsigned decimal strings without leading zeros. The engine permits a zero expected version as a never-matching condition.

`POST /v1/delete`:

```json
{"key":"aw","condition":{"kind":"version","version":"1"}}
```

Returns `{"deleted":true}` on removal; unconditional absent deletion returns false. Delete accepts `any`/`version`, not `absent`.

`POST /v1/scan`:

```json
{"prefix":"","after_key":null,"limit":10}
```

Returns `entries`, each containing a base64url `key` and an `entry` object with the same value/version/expiry fields as get, plus `next_key` (base64url or null). Pass it as the next request's `after_key`, retaining the same prefix. Empty prefix matches all live keys; empty continuation key is invalid. Limits are 1–100 entries and 1 MiB returned key+value bytes per page. Output JSON can be larger because of base64 and metadata. Each page samples time once; pages do not form a consistent snapshot across mutations.

`GET /health/live` returns `{"live":true}` while the request loop is responsive. `GET /health/ready` returns `{"ready":true}` only for a healthy opened owner; unhealthy storage returns 503. Health checks also require the token. They use cached worker health without blocking on the storage mutex; connection admission and runtime responsiveness still apply.

## Failure and retry contract

- 400: malformed input, bad encoding/length/condition, invalid TTL or clock/deadline error.
- 401: missing/incorrect authorization.
- 404/405: unknown route/unsupported method.
- 411/413/415: required content length missing/body too large/wrong content type.
- 408/417: body deadline exceeded/unsupported Expect handshake.
- 429: bounded storage queue is full; the operation was not enqueued.
- 409: condition conflict; re-read and deliberately merge or retry with the new version.
- 507: configured capacity exhausted; no successful user mutation for that request.
- 503: unavailable storage/sequence exhaustion/owner failure; preserve the response and investigate. `mutation_outcome_may_be_unknown` is true.

Application error responses contain a stable `error` code and no user keys, values, filesystem paths, or tokens. Readiness failures have their dedicated health body. Parser-level errors can close without JSON. Connection-cap rejection closes the socket without an HTTP response. Connection loss or timeout after sending a mutation has an unknown outcome even if no JSON response arrived. A put is acknowledged after the core's successful sync. Retrying an unconditional put can overwrite a newer value or extend its TTL. CAS prevents stale overwrite but does not identify which client committed an ambiguous response. There is no persisted idempotency-key mechanism.

For single-host applications in different containers, loopback addresses refer to each container's network namespace. The current adapter requires colocating the caller or explicitly arranging a shared network namespace; remote/private-network binding is not implemented. Expand the deployment contract only after the resource envelope, authorization and TLS/proxy design are reviewed.
