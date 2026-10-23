"""Independent, deterministic Python/zlib v1 seeds. Does not use the Rust encoder.

Run to refresh named seeds; grown fuzz/corpus data is never overwritten.
"""
import shutil
import struct
import zlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def checked(data):
    return data + struct.pack("<I", zlib.crc32(data))


def header(sequence=0, count=0):
    return checked(b"YALIL001" + struct.pack("<QQ", sequence, count))


def frame(key=b"key", value=b"value", sequence=1, expiry=0, delete=False):
    body = checked(struct.pack("<BQQII", int(delete), sequence, expiry, len(key), len(value)) + key + value)
    return checked(b"YLR1" + struct.pack("<I", len(body))) + body


def main():
    seeds = {
        "decode": {
            "manifest": checked(b"YALIM001" + struct.pack("<Q", 1)),
            "manifest-zero": checked(b"YALIM001" + struct.pack("<Q", 0)),
            "header": header(), "frame": frame(), "empty-value": frame(value=b""),
            "tombstone": frame(value=b"", delete=True),
            "ttl": frame(expiry=1234567),
            "max-frame": frame(key=b"k" * 1024, value=b"v" * 65536),
            "partial-envelope": frame()[:11], "partial-body": frame()[:-1],
        },
        "recovery": {
            "empty": header(), "one-put": header() + frame(),
            "delete": header() + frame() + frame(value=b"", delete=True, sequence=2),
            "snapshot": header(9, 1) + frame(sequence=7) + frame(key=b"next", sequence=10),
            "empty-snapshot": header(9, 0), "max-put": header() + frame(value=b"v" * 65536),
            "torn-tail": header() + frame() + frame(sequence=2)[:-3],
            "incomplete-snapshot": header(9, 2) + frame(sequence=7),
            "sequence-gap": header() + frame(sequence=3),
        },
        "state": {"empty-ops": b"\0", "mixed": bytes(range(256)), "hot-key": bytes([0, 0, 0, 7] * 128)},
    }
    for target, cases in seeds.items():
        directory = ROOT / "fuzz/seeds" / target
        corpus = ROOT / "fuzz/corpus" / target
        directory.mkdir(parents=True, exist_ok=True)
        corpus.mkdir(parents=True, exist_ok=True)
        for name, data in cases.items():
            seed = directory / f"{name}.bin"
            seed.write_bytes(data)
            if not (corpus / seed.name).exists():
                shutil.copy2(seed, corpus / seed.name)
    print("Independent seed fixtures prepared; grown corpus preserved.")


if __name__ == "__main__":
    main()
