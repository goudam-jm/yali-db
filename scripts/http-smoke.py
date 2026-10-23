"""Real HTTP/auth/restart checks using only Python's standard library.

Run `cargo build -p yalidb-server --locked` first. No credentials or user data are printed.
"""
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / "target/debug/yalidb-server"
TOKEN = secrets.token_hex(32)
OPENER = urllib.request.build_opener(urllib.request.ProxyHandler({}))


def call(port, route, payload=None, token=TOKEN, method=None):
    data = None if payload is None else json.dumps(payload).encode()
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}{route}", data=data,
        headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"},
        method=method,
    )
    try:
        response = OPENER.open(request, timeout=3)
    except urllib.error.HTTPError as response:
        return response.code, json.load(response)
    with response:
        return response.status, json.load(response)


def start(directory, overrides=None):
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    process = subprocess.Popen(
        [str(BINARY), directory, f"127.0.0.1:{port}"],
        env={**os.environ, "YALIDB_TOKEN": TOKEN, **(overrides or {})},
        stdout=subprocess.DEVNULL, stderr=subprocess.PIPE,
    )
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise AssertionError(process.stderr.read().decode())
        try:
            if call(port, "/health/ready")[0] == 200:
                return process, port
        except (OSError, urllib.error.URLError):
            time.sleep(0.02)
    process.kill()
    process.wait(timeout=5)
    raise AssertionError("service readiness timeout")


def stop(process):
    process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)
    process.stderr.close()
    assert process.returncode == 0, f"service did not drain cleanly: {process.returncode}"


def main():
    with tempfile.TemporaryDirectory(prefix="yalidb-http-") as directory:
        database = str(Path(directory) / "db")
        process, port = start(database)
        try:
            assert call(port, "/health/ready", token="wrong")[0] == 401
            assert call(port, "/v1/get", {"key": "aw"}) == (200, {"found": False})
            status, receipt = call(port, "/v1/put", {"key": "aw", "value": "dg", "condition": {"kind": "absent"}})
            assert status == 200 and receipt["version"] == "1"
            assert call(port, "/v1/put", {"key": "aw", "value": "eA", "condition": {"kind": "version", "version": "99"}})[0] == 409
            assert call(port, "/v1/get", {"key": "aw"})[1]["entry"]["value"] == "dg"
            assert call(port, "/v1/get", {"key": "%%%"})[0] == 400
            assert call(port, "/v1/put", {"key": "aw", "value": "dg", "ttl_ms": 0})[0] == 400
            assert call(port, "/v1/scan", {"prefix": "", "limit": 1})[1]["entries"][0]["key"] == "aw"
            assert call(port, "/v1/get", {"key": "aw", "extra": True})[0] == 400
            assert call(port, "/v1/get", {"key": "aw"}, method="GET")[0] == 405
            assert call(port, "/missing", {})[0] == 404
            assert call(port, "/v1/put", {"key": "aw", "value": "eA" * 100000})[0] == 413
        finally:
            stop(process)
        process, port = start(database)
        try:
            assert call(port, "/v1/get", {"key": "aw"})[1]["entry"]["version"] == "1"
            assert call(port, "/v1/delete", {"key": "aw", "condition": {"kind": "version", "version": "1"}}) == (200, {"deleted": True})
            assert call(port, "/v1/get", {"key": "aw"}) == (200, {"found": False})
        finally:
            stop(process)
    print("HTTP smoke checks passed (auth, API contracts, limits, CAS, and restart).")


if __name__ == "__main__":
    main()
