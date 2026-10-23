# Security status

This project has no production-approved release or published security-support window yet. The HTTP adapter has connection/queue bounds, transport deadlines, strict framing policy and graceful shutdown, but independent security review and prolonged overload testing remain required. It remains intended for trusted loopback evaluation. Do not expose it publicly, use its token in browser/mobile clients, or rely on TTL for security decisions.

Do not post credentials, user data, exploit details against real deployments, or private database contents in public reports. Until a repository security contact/private advisory channel is configured, coordinate private disclosure directly with the project owner rather than assuming public issues are a private channel. Establish that channel before distributing production builds.

Local storage requires a trusted private directory and cooperating processes. Checksums protect against accidental corruption, not malicious modification. There is no database encryption or tenant authorization inside the engine; applications and deployment controls provide those boundaries.
