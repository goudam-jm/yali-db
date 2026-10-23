"""Adversarial loopback socket checks. Build yalidb-server first; no external services.

Deadlines use generous assertion margins; these are correctness tests, not latency benchmarks.
"""
import http.client
import json
from pathlib import Path
import runpy
import signal
import socket
import tempfile
import time

helpers = runpy.run_path(str(Path(__file__).with_name("http-smoke.py")))
start, stop, call, token = (helpers[k] for k in ("start", "stop", "call", "TOKEN"))


def connect(port):
    sock = socket.create_connection(("127.0.0.1", port), timeout=2)
    sock.settimeout(2)
    return sock


def headers(port, route="/v1/put", length=100):
    return (
        f"POST {route} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n"
        f"Authorization: Bearer {token}\r\nContent-Type: application/json\r\n"
        f"Content-Length: {length}\r\n\r\n"
    ).encode()


def receive(sock):
    output = bytearray()
    try:
        while True:
            data = sock.recv(65536)
            if not data:
                break
            output.extend(data)
    except ConnectionResetError:
        pass
    return bytes(output)


def slow_headers(port):
    with connect(port) as sock:
        began = time.monotonic()
        sock.sendall(b"POST /v1/put HTTP/1.1\r\n")
        for _ in range(8):
            time.sleep(0.05)
            try:
                sock.sendall(b"X")
            except OSError:
                break
        receive(sock)
        assert time.monotonic() - began < 1.5, "header trickle extended the deadline"


def slow_body(port):
    with connect(port) as sock:
        began = time.monotonic()
        sock.sendall(headers(port, length=1000))
        sock.sendall(b"{")
        for _ in range(8):
            time.sleep(0.05)
            try:
                sock.sendall(b" ")
            except OSError:
                break
        response = receive(sock)
        assert b" 408 " in response, response
        assert time.monotonic() - began < 1.5, "body trickle extended the deadline"


def malformed_framing(port):
    requests = [
        b"POST /v1/put HTTP/1.1\r\nHost: x\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n{}",
        headers(port).replace(b"Content-Length: 100", b"Transfer-Encoding: chunked\r\nContent-Length: 100"),
        headers(port).replace(b"Content-Length: 100", b"Content-Length: -1"),
        headers(port).replace(b"Content-Length: 100", b"Transfer-Encoding: chunked"),
        headers(port).replace(b"\r\n\r\n", b"\r\nExpect: 100-continue\r\n\r\n"),
        headers(port).replace(b"\r\n\r\n", b"\r\nX-Large: " + b"x" * 20000 + b"\r\n\r\n"),
        headers(port).replace(b"\r\n\r\n", b"\r\n" + b"X-Many: x\r\n" * 40 + b"\r\n"),
        headers(port).replace(f"Authorization: Bearer {token}\r\n".encode(), f"Authorization: Bearer {token}\r\nAuthorization: Bearer {token}\r\n".encode()),
    ]
    for data in requests:
        with connect(port) as sock:
            try:
                sock.sendall(data)
            except ConnectionResetError:
                pass
            response = receive(sock)
            assert not response or response.startswith((b"HTTP/1.1 4", b"HTTP/1.1 5")), response
            assert b" 200 " not in response, response
    assert call(port, "/v1/get", {"key": "aw"}) == (200, {"found": False})


def idle_connection_expires(port):
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
    conn.request("GET", "/health/live", headers={"Authorization": f"Bearer {token}"})
    reply = conn.getresponse()
    assert reply.status == 200
    reply.read()
    sock = conn.sock
    time.sleep(0.5)
    assert receive(sock) == b""
    conn.close()


def connection_cap(database):
    process, port = start(database, {"YALIDB_MAX_CONNECTIONS": "2", "YALIDB_HEADER_TIMEOUT_MS": "3000"})
    held = []
    try:
        for _ in range(2):
            sock = connect(port)
            sock.sendall(b"GET /health/live HTTP/1.1\r\n")
            held.append(sock)
        time.sleep(0.1)
        with connect(port) as refused:
            began = time.monotonic()
            try:
                refused.sendall(b"GET /health/live HTTP/1.1\r\nHost: x\r\n\r\n")
            except ConnectionResetError:
                pass
            assert receive(refused) == b""
            assert time.monotonic() - began < 1
        for sock in held:
            sock.close()
        held.clear()
        deadline = time.monotonic() + 2
        while True:
            try:
                assert call(port, "/health/ready")[0] == 200
                break
            except (OSError, http.client.HTTPException):
                if time.monotonic() > deadline:
                    raise
                time.sleep(0.02)
    finally:
        for sock in held:
            sock.close()
        stop(process)


def request_count_cap(port):
    conn = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
    try:
        for n in range(100):
            conn.request("GET", "/health/live", headers={"Authorization": f"Bearer {token}"})
            reply = conn.getresponse()
            assert reply.status == 200
            assert (reply.getheader("Connection") == "close") == (n == 99)
            reply.read()
    finally:
        conn.close()


def lifetime_cap(database):
    process, port = start(database, {"YALIDB_CONNECTION_LIFETIME_MS": "500", "YALIDB_HEADER_TIMEOUT_MS": "300"})
    try:
        with connect(port) as sock:
            began = time.monotonic()
            closed = False
            for _ in range(20):
                try:
                    sock.sendall(f"GET /health/live HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\n\r\n".encode())
                    reply = http.client.HTTPResponse(sock)
                    reply.begin()
                    assert reply.status == 200
                    reply.read()
                    reply.close()
                except (OSError, http.client.HTTPException):
                    closed = True
                    break
                time.sleep(0.075)
            assert closed, "active keep-alive bypassed the connection lifetime cap"
            assert time.monotonic() - began < 1.5
    finally:
        stop(process)


def graceful_inflight_write(database):
    process, port = start(database, {"YALIDB_SHUTDOWN_TIMEOUT_MS": "2000"})
    body = json.dumps({"key": "ZHJhaW4", "value": "dg"}).encode()
    try:
        with connect(port) as sock:
            sock.sendall(headers(port, length=len(body)) + body[:1])
            time.sleep(0.1)
            process.send_signal(signal.SIGTERM)
            time.sleep(0.05)
            sock.sendall(body[1:])
            reply = http.client.HTTPResponse(sock)
            reply.begin()
            assert reply.status == 200
            reply.read()
            reply.close()
        process.wait(timeout=3)
        assert process.returncode == 0
    finally:
        if process.poll() is None:
            stop(process)
        else:
            process.stderr.close()
    process, port = start(database)
    try:
        assert call(port, "/v1/get", {"key": "ZHJhaW4"})[1]["entry"]["value"] == "dg"
    finally:
        stop(process)


with tempfile.TemporaryDirectory(prefix="yalidb-harden-") as root:
    process, port = start(str(Path(root) / "deadlines"), {
        "YALIDB_HEADER_TIMEOUT_MS": "200", "YALIDB_BODY_TIMEOUT_MS": "200",
    })
    try:
        slow_headers(port)
        slow_body(port)
        malformed_framing(port)
        idle_connection_expires(port)
        assert call(port, "/health/ready")[0] == 200
    finally:
        stop(process)
    process, port = start(str(Path(root) / "request-count"))
    try:
        request_count_cap(port)
    finally:
        stop(process)
    connection_cap(str(Path(root) / "connections"))
    lifetime_cap(str(Path(root) / "lifetime"))
    graceful_inflight_write(str(Path(root) / "shutdown"))
print("HTTP hardening checks passed (slow clients, framing, idle/lifetime/count caps, saturation, and shutdown).")
