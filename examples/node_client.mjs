// Node.js evaluation client. An aborted/lost mutation response has an unknown outcome.
const token = process.env.YALIDB_TOKEN;
if (!token) throw new Error("YALIDB_TOKEN is required");
const port = Number(process.env.YALIDB_PORT ?? 7419);
if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error("Invalid YALIDB_PORT");
const key = Buffer.from("example:cross-language").toString("base64url");

async function operation(route, payload) {
  const response = await fetch(`http://127.0.0.1:${port}/v1/${route}`, {
    method: "POST",
    headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
    body: JSON.stringify(payload),
    signal: AbortSignal.timeout(5000),
  });
  const result = await response.json();
  if (!response.ok) throw new Error(`HTTP ${response.status}: ${result.error}`);
  return result;
}

if (process.argv[2] === "read") {
  console.log(JSON.stringify(await operation("get", { key })));
} else {
  const prior = await operation("get", { key });
  const condition = prior.found
    ? { kind: "version", version: prior.entry.version }
    : { kind: "absent" };
  const receipt = await operation("put", {
    key, value: Buffer.from("node").toString("base64url"), condition,
  });
  console.log(JSON.stringify(receipt));
}
