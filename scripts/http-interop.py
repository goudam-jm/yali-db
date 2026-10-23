"""Python and Node share a real owner and conditionally update the same record."""
import base64
import json
import os
from pathlib import Path
import runpy
import subprocess
import tempfile

helpers = runpy.run_path(str(Path(__file__).with_name("http-smoke.py")))
start, stop, call, token, root = (helpers[k] for k in ("start", "stop", "call", "TOKEN", "ROOT"))
with tempfile.TemporaryDirectory(prefix="yalidb-interop-") as directory:
    process, port = start(str(Path(directory) / "db"))
    try:
        env = {**os.environ, "YALIDB_TOKEN": token, "YALIDB_PORT": str(port)}
        command = ["node", str(root / "examples/node_client.mjs")]
        result = subprocess.run(command, env=env, capture_output=True, text=True, timeout=10, check=True)
        receipt = json.loads(result.stdout)
        key = base64.urlsafe_b64encode(b"example:cross-language").rstrip(b"=").decode()
        status, current = call(port, "/v1/get", {"key": key})
        assert status == 200 and current["entry"]["version"] == receipt["version"]
        assert current["entry"]["value"] == "bm9kZQ"
        status, updated = call(port, "/v1/put", {
            "key": key, "value": "cHl0aG9u",
            "condition": {"kind": "version", "version": receipt["version"]},
        })
        assert status == 200 and int(updated["version"]) > int(receipt["version"])
        result = subprocess.run(command + ["read"], env=env, capture_output=True, text=True, timeout=10, check=True)
        read = json.loads(result.stdout)
        assert read["entry"]["value"] == "cHl0aG9u"
        assert read["entry"]["version"] == updated["version"]
        assert call(port, "/v1/delete", {"key": key, "condition": {"kind": "version", "version": receipt["version"]}})[0] == 409
    finally:
        stop(process)
print("Cross-language HTTP checks passed (Node write, Python CAS update, Node read, stale-version conflict).")
