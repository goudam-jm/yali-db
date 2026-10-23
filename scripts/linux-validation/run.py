"""Disposable-container harness; restricted managed mode is the default.

Optional loop mode requires separate approval and only formats fresh, private file-backed devices.
Results/ACK ledgers live outside the filesystems under test. Clean remounts are NOT power cuts.
"""
import argparse
import errno
import hashlib
import json
import os
from pathlib import Path
import platform
import random
import selectors
import shutil
import subprocess
import tempfile
import time
import traceback

APP = Path("/app")
PROBE = APP / "target/release/examples/storage_probe"


def run(command, **kwargs):
    return subprocess.run([str(x) for x in command], check=True, text=True, capture_output=True, timeout=kwargs.pop("timeout", 600), **kwargs)


def line(process, timeout=10):
    with selectors.DefaultSelector() as selector:
        selector.register(process.stdout, selectors.EVENT_READ)
        if not selector.select(timeout):
            raise TimeoutError("probe failed to emit a line before deadline")
        output = process.stdout.readline()
    if not output:
        raise AssertionError(f"probe exited early: {process.stderr.read().decode()}")
    return output.decode().strip().split()


def send(process):
    process.stdin.write(b"GO\n")
    process.stdin.flush()


def apply(state, sequence, operation, key, value):
    state = dict(state)
    if operation == "P":
        sequence += 1
        state[key] = [int(value), sequence]
    elif key in state:
        sequence += 1
        del state[key]
    return state, sequence


def stress(mount, output, seeds):
    results = []
    for seed in range(1, seeds + 1):
        database = mount / f"seed-{seed}"
        process = subprocess.Popen([str(PROBE), "worker", str(database), str(seed)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
        state, sequence = {}, 0
        ledger = output / f"ack-{seed}.jsonl"
        try:
            with ledger.open("w") as journal:
                for _ in range(16 + seed % 32):
                    send(process)
                    begin = line(process)
                    assert begin[0] == "BEGIN", begin
                    ack = line(process)
                    assert ack[0] == "ACK" and ack[1] == begin[1], ack
                    state, sequence = apply(state, sequence, ack[3], ack[4], ack[5])
                    assert sequence == int(ack[2])
                    journal.write(json.dumps({"ack": ack, "expected_sequence": sequence}) + "\n")
                    journal.flush()
                    os.fsync(journal.fileno())
                # At most ONE operation can be ahead of the external acknowledged ledger.
                send(process)
                pending = line(process)
                assert pending[0] == "BEGIN"
                candidate, candidate_sequence = apply(state, sequence, pending[2], pending[3], pending[4])
                time.sleep(random.Random(seed).choice([0, 0.0005, 0.003]))
                process.kill()
                process.wait(timeout=10)
                assert process.returncode < 0
            dumped = run([PROBE, "dump", database]).stdout.splitlines()
            recovered_sequence = int(dumped[0].split()[1])
            recovered = {row.split()[1]: [int(row.split()[2]), int(row.split()[3])] for row in dumped[1:]}
            assert (recovered, recovered_sequence) in [(state, sequence), (candidate, candidate_sequence)], f"seed {seed}: acknowledged prefix lost"
            results.append({"seed": seed, "acknowledged_operations": 16 + seed % 32, "acknowledged_sequence": sequence, "recovered_sequence": recovered_sequence, "recovered_state": recovered, "status": "passed"})
        except BaseException:
            if database.exists():
                shutil.copytree(database, output / f"failure-seed-{seed}", dirs_exist_ok=True)
            raise
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=10)
            process.stdin.close(); process.stdout.close(); process.stderr.close()
    return results


def locking(mount):
    database = mount / "locking"
    process = subprocess.Popen([str(PROBE), "hold", str(database)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
    try:
        assert line(process) == ["HELD"]
        run([PROBE, "locked", database])
        process.stdin.write(b"release\n"); process.stdin.flush()
        process.wait(timeout=10)
        assert process.returncode == 0
        run([PROBE, "hold", database], input="release\n")
    finally:
        if process.poll() is None:
            process.kill(); process.wait(timeout=10)
        process.stdin.close(); process.stdout.close(); process.stderr.close()


def disk_full(mount):
    database = mount / "enospc"
    run([PROBE, "init-enospc", database])
    reserve, filler, probe = (mount / name for name in ("reserve", "filler", "short-write"))
    reserve.write_bytes(b"x" * 131072)
    with reserve.open("rb") as file:
        os.fsync(file.fileno())
    block = b"f" * (1024 * 1024)
    filled = 0
    with filler.open("wb", buffering=0) as file:
        while True:
            try:
                count = file.write(block)
                assert count is not None and count > 0
                filled += count
            except OSError as error:
                assert error.errno == errno.ENOSPC, error
                break
        os.fsync(file.fileno())
    reserve.unlink()
    # POSIX permits a successful short write before the following write returns ENOSPC.
    with probe.open("wb", buffering=0) as file:
        written = file.write(block)
        assert written is not None and 0 < written < len(block), f"expected real short write, got {written}"
        try:
            file.write(block)
            raise AssertionError("expected ENOSPC after the short write")
        except OSError as error:
            assert error.errno == errno.ENOSPC
        os.fsync(file.fileno())
    result = run([PROBE, "expect-enospc", database]).stdout.strip().splitlines()
    assert result[-1].startswith("ENOSPC_OWNER_POISONED ")
    confirmed = int(result[-1].split()[1])
    assert len(result) == confirmed + 1
    filler.unlink(); probe.unlink()
    result = run([PROBE, "verify-enospc", database, confirmed]).stdout.strip()
    assert result == "ACKNOWLEDGED_ANCHOR_PRESERVED"
    return {"filled_bytes": filled, "short_write_bytes": written, "confirmed_puts": confirmed, "status": "passed"}


def source_digest():
    digest = hashlib.sha256()
    paths = [APP / "Cargo.toml", APP / "Cargo.lock", APP / "yalidb-core/Cargo.toml"] + sorted((APP / "yalidb-core").rglob("*.rs")) + sorted((APP / "yalidb-core/tests/fixtures").glob("*.bin")) + sorted((APP / "scripts/linux-validation").glob("*"))
    for path in paths:
        digest.update(str(path.relative_to(APP)).encode() + b"\0" + path.read_bytes())
    return digest.hexdigest()


def core_suite(mount,output):
    import re
    artifacts=[json.loads(line) for line in (APP/"core-test-artifacts.jsonl").read_text().splitlines()]
    binaries=sorted({row["executable"] for row in artifacts if row.get("reason")=="compiler-artifact" and row.get("profile",{}).get("test") and row.get("executable")})
    assert binaries,"no test binaries found"
    passed=0
    with (output/"core-tests.log").open("w") as log:
        for binary in binaries:
            suite=subprocess.run([binary,"--nocapture"],env={**os.environ,"TMPDIR":str(mount)},text=True,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=600)
            log.write(suite.stdout);log.flush()
            assert suite.returncode==0,"core suite failed; inspect core-tests.log"
            match=re.search(r"test result: ok\. (\d+) passed",suite.stdout)
            assert match,suite.stdout
            passed+=int(match.group(1))
    return passed


def managed(args,output,result):
    work=Path("/work")
    if args.verify_only:
        prior=json.loads((output/"results.json").read_text())
        result.update(prior)
        assert prior["status"]=="passed"
        assert (work/".yalidb-validation-owned").read_text()==source_digest()
        for row in prior["filesystems"][0]["stress"]:
            dumped=run([PROBE,"dump",work/f"seed-{row['seed']}"]).stdout.splitlines()
            sequence=int(dumped[0].split()[1])
            state={line.split()[1]:[int(line.split()[2]),int(line.split()[3])] for line in dumped[1:]}
            if sequence!=row["recovered_sequence"] or state!=row["recovered_state"]:
                shutil.copytree(work/f"seed-{row['seed']}",output/f"restart-failure-{row['seed']}",dirs_exist_ok=True)
                raise AssertionError(f"container restart changed seed {row['seed']}")
        result.update(prior)
        result["container_restart_verification"]="passed"
        return
    assert work.is_dir() and not any(work.iterdir()),"use a fresh empty Docker volume, never existing data"
    (work/".yalidb-validation-owned").write_text(source_digest())
    mount=json.loads(run(["findmnt","-J","--target",work,"-o","SOURCE,FSTYPE,OPTIONS"]).stdout)
    record={"filesystem":mount["filesystems"][0]["fstype"],"mount":mount,"status":"running"}
    result["filesystems"].append(record)
    record["core_tests_passed"]=core_suite(work,output)
    record["core_suite"]="passed"
    locking(work);record["cross_process_lock"]="passed"
    record["stress"]=stress(work,output,args.seeds)
    scratch=Path("/scratch")
    fs_type=json.loads(run(["findmnt","-J","--target",scratch,"-o","FSTYPE"]).stdout)["filesystems"][0]["fstype"]
    assert fs_type=="tmpfs","ENOSPC test requires a size-limited disposable tmpfs, not a host disk"
    record["enospc_filesystem"]="tmpfs"
    record["enospc"]=disk_full(scratch)
    record["status"]="passed"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", default="/results")
    parser.add_argument("--mode", choices=["managed", "loop"], default="managed")
    parser.add_argument("--allow-loop-devices", action="store_true")
    parser.add_argument("--verify-only", action="store_true")
    parser.add_argument("--seeds", type=int, default=32)
    parser.add_argument("--filesystems", nargs="+", choices=["ext4", "xfs"], default=["ext4", "xfs"])
    args = parser.parse_args()
    if platform.system() != "Linux" or not Path("/.dockerenv").exists():
        raise SystemExit("Run only in the provided disposable container; no host-device mode is supported.")
    if args.mode == "loop" and (os.geteuid() != 0 or not args.allow_loop_devices):
        raise SystemExit("Loop mode requires explicit --allow-loop-devices and separate privileged-run approval.")
    if not 1 <= args.seeds <= 4096:
        raise SystemExit("seeds must be 1..4096")
    output = Path(args.output).resolve(); output.mkdir(parents=True, exist_ok=True)
    result = {"status": "running", "kernel": platform.release(), "machine": platform.machine(), "rust": run(["rustc", "--version"]).stdout.strip(), "source_sha256": source_digest(), "ci_sha": os.environ.get("GITHUB_SHA") or None, "scope": "Docker-managed Linux volume plus tmpfs ENOSPC; NOT OS power cut" if args.mode=="managed" else "disposable loop filesystems; NOT OS power cut", "execution_environment": os.environ.get("YALIDB_EXECUTION_ENVIRONMENT","CI/Linux"), "mode": args.mode, "filesystems": []}
    started = time.monotonic()
    try:
        if args.mode=="managed":
            managed(args,output,result)
            result["status"]="passed"
            return
        with tempfile.TemporaryDirectory(prefix="yalidb-linux-") as directory:
            root = Path(directory)
            for filesystem in args.filesystems:
                image = root / f"{filesystem}.img"
                with image.open("wb") as file:
                    file.truncate(512 * 1024 * 1024)
                mount = root / filesystem; mount.mkdir()
                loop = run(["losetup", "--find", "--show", image]).stdout.strip()
                assert loop.startswith("/dev/loop")
                mounted = False
                record = {"filesystem": filesystem, "status": "running"}
                result["filesystems"].append(record)
                fs_output = output / filesystem; fs_output.mkdir(exist_ok=True)
                try:
                    run([f"mkfs.{filesystem}", "-f" if filesystem == "xfs" else "-F", loop])
                    run(["mount", "-t", filesystem, loop, mount]); mounted = True
                    record["mount"] = run(["findmnt", "-J", "--target", mount, "-o", "SOURCE,FSTYPE,OPTIONS"]).stdout.strip()
                    record["core_tests_passed"]=core_suite(mount,fs_output)
                    record["core_suite"]="passed"
                    locking(mount); record["cross_process_lock"] = "passed"
                    record["stress"] = stress(mount, fs_output, args.seeds)
                    record["enospc"] = disk_full(mount)
                    run(["umount", mount]); mounted = False
                    run(["mount", "-t", filesystem, loop, mount]); mounted = True
                    run([PROBE, "verify-enospc", mount / "enospc", record["enospc"]["confirmed_puts"]])
                    record["clean_remount"] = "passed"; record["status"] = "passed"
                finally:
                    if mounted:
                        run(["umount", mount])
                    run(["losetup", "--detach", loop])
                    image.unlink(missing_ok=True)
        result["status"] = "passed"
    except BaseException as error:
        result["status"] = "failed"; result["error"] = str(error)
        (output / "failure.log").write_text(traceback.format_exc())
        raise
    finally:
        elapsed=round(time.monotonic()-started,3)
        if args.verify_only:
            result["verification_elapsed_seconds"]=elapsed
            result["elapsed_seconds"]=round(result.get("validation_elapsed_seconds",0)+elapsed,3)
        else:
            result["validation_elapsed_seconds"]=elapsed
            result["elapsed_seconds"]=elapsed
        (output / "results.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps({"status": result["status"], "machine": result["machine"], "filesystems": [(f["filesystem"], f["status"]) for f in result["filesystems"]]}), flush=True)


if __name__ == "__main__":
    main()
