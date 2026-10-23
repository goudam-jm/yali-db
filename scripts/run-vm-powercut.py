"""Launch only a restricted container with a fresh, private VM-image volume."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import uuid


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--image', required=True)
    parser.add_argument('--platform', choices=['linux/arm64', 'linux/amd64'], required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--filesystems', nargs='+', choices=['ext4', 'xfs'], default=['ext4', 'xfs'])
    parser.add_argument('--cases', type=int, default=10)
    args = parser.parse_args()
    assert 1 <= args.cases <= 10
    assert os.geteuid() != 0, 'run as an ordinary user'
    output = args.output.resolve()
    assert not output.exists(), 'choose a new output directory to preserve evidence'
    output.mkdir(parents=True)
    docker = shutil.which('docker') or str(Path.home() / '.docker/bin/docker')
    volume = 'yalidb-vm-powercut-' + uuid.uuid4().hex
    subprocess.run([docker, 'volume', 'create', '--label', 'app=yalidb-vm-powercut', volume], check=True, capture_output=True)
    command = [docker, 'run', '--rm', '--name', volume, '--platform', args.platform, '--user', f'{os.getuid()}:{os.getgid()}', '--read-only', '--network', 'none', '--cap-drop', 'ALL', '--security-opt', 'no-new-privileges=true', '--memory', '2g', '--cpus', '2', '--pids-limit', '128', '--mount', f'type=volume,source={volume},target=/work', '--mount', f'type=bind,source={output},target=/results', '--env', f'GITHUB_SHA={os.environ.get("GITHUB_SHA", "")}', args.image, '--filesystems', *args.filesystems, '--cases', str(args.cases)]
    log_path = output.with_suffix('.log')
    try:
        with log_path.open('x') as log:
            subprocess.run(command, check=True, stdout=log, stderr=subprocess.STDOUT, timeout=3600)
        report_path = output / 'results.json'
        result = json.loads(report_path.read_text())
        result['container_image_id'] = subprocess.check_output([docker, 'image', 'inspect', '--format', '{{.Id}}', args.image], text=True).strip()
        result['docker_daemon_arch'] = subprocess.check_output([docker, 'version', '--format', '{{.Server.Arch}}'], text=True).strip()
        result['architecture_emulation'] = args.platform.rsplit('/', 1)[1] != result['docker_daemon_arch']
        result['container_limits'] = {'capabilities': [], 'network': 'none', 'read_only_root': True, 'uid': os.getuid(), 'gid': os.getgid(), 'memory': '2g', 'cpus': 2, 'pids': 128}
        report_path.write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps({'status': result['status'], 'cases': len(result['cases']), 'machine': result['machine']}))
    finally:
        subprocess.run([docker, 'rm', '--force', volume], capture_output=True)
        subprocess.run([docker, 'volume', 'rm', volume], check=True, capture_output=True)
        if log_path.exists(): shutil.move(log_path, output / 'controller.log')


if __name__ == '__main__': main()
