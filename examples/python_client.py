"""Standard-library evaluation client. No automatic mutation retries."""
import base64
import json
import os
import urllib.error
import urllib.request


def encode(value: bytes) -> str:
    return base64.urlsafe_b64encode(value).rstrip(b"=").decode("ascii")


def operation(route: str, payload: dict) -> dict:
    token = os.environ["YALIDB_TOKEN"]
    request = urllib.request.Request(
        f"http://127.0.0.1:7419/v1/{route}",
        data=json.dumps(payload).encode(),
        headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"},
        method="POST",
    )
    # A timeout is an unknown write outcome. HTTP 409 requires application-level reconciliation.
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with opener.open(request, timeout=5) as response:
        return json.load(response)


if __name__ == "__main__":
    key = encode(b"user:demo:theme")
    current = operation("get", {"key": key})
    condition = (
        {"kind": "version", "version": current["entry"]["version"]}
        if current["found"]
        else {"kind": "absent"}
    )
    receipt = operation("put", {"key": key, "value": encode(b"dark"), "condition": condition})
    print("Stored version", receipt["version"])
