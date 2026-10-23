"""Bounded, sanitizer-enabled campaigns with logs, stats, corpus minimization and artifacts."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import time

ROOT = Path(__file__).resolve().parents[1]
NIGHTLY = "nightly-2026-10-22"


def digest():
    value = hashlib.sha256()
    files = [ROOT / "Cargo.lock", ROOT / "fuzz/Cargo.lock", ROOT / "yalidb-core/Cargo.toml", ROOT / "fuzz/Cargo.toml"]
    files += sorted((ROOT / "yalidb-core/src").rglob("*.rs")) + sorted((ROOT / "fuzz/fuzz_targets").glob("*.rs"))
    for path in files:
        value.update(str(path.relative_to(ROOT)).encode() + b"\0" + path.read_bytes())
    return value.hexdigest()


def corpus_stats(path):
    files = [p for p in path.iterdir() if p.is_file()]
    return {"files": len(files), "bytes": sum(p.stat().st_size for p in files)}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--seconds", type=int, default=300)
    parser.add_argument("--targets", nargs="+", choices=["decode", "recovery", "state"], default=["decode", "recovery", "state"])
    parser.add_argument("--seed", type=int, default=20261023)
    parser.add_argument("--output", type=Path, default=ROOT / "validation-results/fuzz")
    parser.add_argument("--parallel", type=int, default=3)
    args = parser.parse_args()
    if not 1 <= args.seconds <= 86400 or not 1 <= args.parallel <= 3:
        raise SystemExit("seconds must be 1..86400 and parallel 1..3")
    output = args.output.resolve(); output.mkdir(parents=True, exist_ok=True)
    subprocess.run(["python3", str(ROOT / "scripts/seed-fuzz.py")], check=True)
    report = {"status": "running", "source_sha256": digest(), "machine": platform.machine(), "platform": platform.platform(), "nightly": NIGHTLY, "cargo_fuzz": subprocess.check_output(["cargo", "fuzz", "--version"], text=True).strip(), "rust": subprocess.check_output(["rustc", f"+{NIGHTLY}", "--version"], text=True).strip(), "sanitizer": "address", "build_std": True, "seconds_per_target": args.seconds, "seed": args.seed, "targets": []}
    (output / "results.json").write_text(json.dumps(report, indent=2) + "\n")

    def campaign(target):
        directory = output / target; directory.mkdir(exist_ok=True)
        artifacts = directory / "artifacts"; artifacts.mkdir(exist_ok=True)
        base = ["cargo", f"+{NIGHTLY}", "fuzz"]
        command = base + ["run", "--build-std", "--fuzz-dir", str(ROOT / "fuzz"), target, str(ROOT / "fuzz/corpus" / target), "--", f"-max_total_time={args.seconds}", "-max_len=131072", "-rss_limit_mb=1024", "-timeout=10", f"-seed={args.seed}", "-print_final_stats=1", f"-artifact_prefix={artifacts}/"]
        started = time.monotonic()
        with (directory / "fuzz.log").open("w") as log:
            result = subprocess.run(command, cwd=ROOT, stdout=log, stderr=subprocess.STDOUT, timeout=args.seconds + 900)
        text = (directory / "fuzz.log").read_text(errors="replace")
        stats = dict(re.findall(r"stat::(\w+):\s+(\d+)", text))
        done = re.findall(r"Done (\d+) runs in (\d+) second", text)
        coverage = re.findall(r"cov:\s*(\d+)", text)
        row = {"target": target, "status": "passed" if result.returncode == 0 and done else "failed", "exit_code": result.returncode, "wall_seconds": round(time.monotonic() - started, 3), "stats": {key: int(value) for key, value in stats.items()}, "executions": int(done[-1][0]) if done else None, "fuzz_seconds": int(done[-1][1]) if done else None, "coverage_counters": int(coverage[-1]) if coverage else None, "corpus_before_minimization": corpus_stats(ROOT / "fuzz/corpus" / target), "command": command}
        if row["status"] == "passed":
            with (directory / "minimize.log").open("w") as log:
                minimized = subprocess.run(base + ["cmin", "--build-std", "--fuzz-dir", str(ROOT / "fuzz"), target, "--", "-rss_limit_mb=1024", "-timeout=10"], cwd=ROOT, stdout=log, stderr=subprocess.STDOUT, timeout=600)
            row["minimization_exit_code"] = minimized.returncode
            if minimized.returncode != 0:
                row["status"] = "failed"
        row["corpus_after_minimization"] = corpus_stats(ROOT / "fuzz/corpus" / target)
        shutil.copytree(ROOT / "fuzz/corpus" / target, directory / "corpus", dirs_exist_ok=True)
        (directory / "results.json").write_text(json.dumps(row, indent=2) + "\n")
        print(json.dumps({key: row.get(key) for key in ("target", "status", "executions", "fuzz_seconds", "coverage_counters")}), flush=True)
        return row

    try:
        with ThreadPoolExecutor(max_workers=args.parallel) as executor:
            report["targets"] = list(executor.map(campaign, args.targets))
        report["status"] = "passed" if all(row["status"] == "passed" for row in report["targets"]) else "failed"
    except BaseException as error:
        report["status"] = "failed"; report["error"] = str(error)
        raise
    finally:
        (output / "results.json").write_text(json.dumps(report, indent=2) + "\n")
    if report["status"] != "passed":
        raise SystemExit("A fuzz target/minimization failed; preserve artifacts and reproduce before changing seeds.")


if __name__ == "__main__":
    main()
