"""Safely run Linux checks with fresh Docker volumes; NO privileged/device access.

The image is built separately. This runner always drops all capabilities and uses the host UID.
"""
import argparse
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import uuid

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--platform", choices=["linux/arm64", "linux/amd64"], required=True)
    parser.add_argument("--image", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--seeds", type=int, default=32)
    args = parser.parse_args()
    if not 1 <= args.seeds <= 4096:
        raise SystemExit("seeds must be 1..4096")
    docker = shutil.which("docker") or str(Path.home() / ".docker/bin/docker")
    output = args.output.resolve(); output.mkdir(parents=True, exist_ok=True)
    if (output / "results.json").exists():
        raise SystemExit("choose a new report directory to preserve previous evidence")
    volume = "yalidb-validation-" + uuid.uuid4().hex
    subprocess.run([docker, "volume", "create", "--label", "app=yalidb-validation", volume], check=True, capture_output=True)
    environment = f"host={platform.system()}/{platform.machine()};container={args.platform};Docker-managed volume"
    container=volume
    command = [docker, "run", "--rm", "--name", container, "--platform", args.platform, "--read-only", "--network", "none", "--cap-drop", "ALL", "--security-opt", "no-new-privileges=true", "--user", f"{os.getuid()}:{os.getgid()}", "--memory", "2g", "--cpus", "2", "--pids-limit", "256", "--mount", f"type=volume,source={volume},target=/work", "--tmpfs", "/scratch:rw,size=64m,mode=1777", "--mount", f"type=bind,source={output},target=/results", "--env", f"YALIDB_EXECUTION_ENVIRONMENT={environment}", "--env", f"GITHUB_SHA={os.environ.get('GITHUB_SHA','')}", args.image, "--mode", "managed", "--seeds", str(args.seeds)]
    try:
        with (output / "container.log").open("w") as log:
            subprocess.run(command, check=True, stdout=log, stderr=subprocess.STDOUT, timeout=1800)
            subprocess.run(command + ["--verify-only"], check=True, stdout=log, stderr=subprocess.STDOUT, timeout=300)
        result = json.loads((output / "results.json").read_text())
        result["container_image_id"] = subprocess.check_output([docker, "image", "inspect", "--format", "{{.Id}}", args.image], text=True).strip()
        result["docker_daemon_arch"]=subprocess.check_output([docker,"version","--format","{{.Server.Arch}}"],text=True).strip()
        result["architecture_emulation"]=args.platform.rsplit("/",1)[1] != result["docker_daemon_arch"]
        result["container_limits"]={"capabilities":[],"read_only_root":True,"network":"none","uid":os.getuid(),"gid":os.getgid(),"memory":"2g","cpus":2,"pids":256}
        (output / "results.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps({"status": result["status"], "machine": result["machine"], "filesystems": [(f["filesystem"], f["status"]) for f in result["filesystems"]], "container_restart_verification": result.get("container_restart_verification")}), flush=True)
    finally:
        subprocess.run([docker,"rm","--force",container],capture_output=True)
        # Only the fresh UUID volume created above is removed. Never use prune or a supplied volume name.
        subprocess.run([docker, "volume", "rm", volume], check=True, capture_output=True)


if __name__ == "__main__":
    main()
