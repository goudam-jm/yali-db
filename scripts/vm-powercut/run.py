"""Abruptly remove a disposable guest and its volatile disk cache; reboot to verify."""
import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path
import platform
import selectors
import shutil
import signal
import subprocess
import tempfile
import time
import traceback

from volatile_disk import Disk

APP = Path('/app')
PLAN = [('ack', 6), ('write', 6), ('before-flush', 6), ('after-flush', 6),
        ('write', 10), ('before-flush', 10), ('after-flush', 10),
        ('write', 16), ('before-flush', 16), ('after-flush', 16)]


def digest():
    paths = [APP / 'Cargo.toml', APP / 'Cargo.lock', APP / 'yalidb-core/Cargo.toml', APP / 'scripts/run-vm-powercut.py']
    paths += sorted((APP / 'yalidb-core').rglob('*.rs'))
    paths += sorted((APP / 'scripts/vm-powercut').glob('*'))
    value = hashlib.sha256()
    for path in paths:
        if path.is_file(): value.update(str(path.relative_to(APP)).encode() + b'\0' + path.read_bytes())
    return value.hexdigest()


class Guest:
    def __init__(self, disk, directory, label):
        arch = platform.machine()
        emulator = 'qemu-system-aarch64' if arch == 'aarch64' else 'qemu-system-x86_64'
        machine = ['-machine', 'virt', '-cpu', 'cortex-a72'] if arch == 'aarch64' else ['-machine', 'q35', '-cpu', 'max']
        console = 'ttyAMA0' if arch == 'aarch64' else 'ttyS0'
        device = 'virtio-blk-device' if arch == 'aarch64' else 'virtio-blk-pci'
        block = {'driver': 'nbd', 'server': {'type': 'unix', 'path': disk.socket_path}, 'node-name': 'data', 'cache': {'direct': False, 'no-flush': False}}
        self.command = [emulator, *machine, '-accel', 'tcg', '-m', '384', '-smp', '1', '-nodefaults', '-no-user-config', '-nic', 'none', '-display', 'none', '-monitor', 'none', '-serial', 'stdio', '-no-reboot', '-kernel', '/app/guest/kernel', '-initrd', '/app/guest/initrd.gz', '-append', f'console={console} rdinit=/init panic=-1 loglevel=4', '-blockdev', json.dumps(block), '-device', f'{device},drive=data,write-cache=on']
        self.log = (directory / f'{label}-console.log').open('wb')
        self.process = subprocess.Popen(self.command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, bufsize=0)
        self.buffer = bytearray()

    def line(self, timeout=120):
        deadline = time.monotonic() + timeout
        while b'\n' not in self.buffer:
            remaining = deadline - time.monotonic()
            if remaining <= 0: raise TimeoutError('guest serial deadline; inspect console log')
            with selectors.DefaultSelector() as selector:
                selector.register(self.process.stdout, selectors.EVENT_READ)
                if not selector.select(remaining): raise TimeoutError('guest serial deadline; inspect console log')
                data = os.read(self.process.stdout.fileno(), 65536)
            if not data: raise AssertionError(f'guest exited {self.process.poll()}; inspect console log')
            self.log.write(data); self.log.flush(); self.buffer.extend(data)
            assert len(self.buffer) < 1024 * 1024, 'guest console line exceeds budget'
        line, _, self.buffer = self.buffer.partition(b'\n')
        return line.decode(errors='replace').strip()

    def until(self, prefix):
        deadline = time.monotonic() + 120
        lines = []
        while True:
            line = self.line(max(0.01, deadline - time.monotonic()))
            lines.append(line)
            if line.startswith(prefix): return lines
            if line in ('MOUNT_FAILED', 'DUMP_FAILED'): return lines
            if time.monotonic() >= deadline: raise TimeoutError(f'guest failed to emit {prefix}')

    def send(self, text):
        self.process.stdin.write((text + '\n').encode()); self.process.stdin.flush()

    def stop(self):
        if self.process.poll() is None:
            self.process.kill(); self.process.wait(timeout=10)
        self.log.close()
        self.process.stdin.close(); self.process.stdout.close()


def apply(state, sequence, operation, key, value):
    state = dict(state)
    if operation == 'P':
        sequence += 1; state[key] = [int(value), sequence]
    else:
        assert operation == 'D'
        if key in state: sequence += 1; del state[key]
    return state, sequence


def recover(image, socket_dir, directory, filesystem):
    with (directory / 'recovery-blocks.jsonl').open('w') as trace:
        disk = Disk(image, socket_dir / 'recover.sock', trace, create=False); disk.serve()
        guest = None
        try:
            guest = Guest(disk, directory, 'recovery')
            boot = guest.until('BOOT ')[-1]
            guest.send(f'RECOVER {filesystem}')
            output = guest.until('DUMP_DONE')
            if output[-1] in ('MOUNT_FAILED', 'DUMP_FAILED'):
                return {'status': 'unrecoverable', 'outcome': output[-1], 'boot': boot}
            metadata = [line for line in output if line.startswith('META ')]
            assert len(metadata) == 1
            state = {line.split()[1]: [int(line.split()[2]), int(line.split()[3])] for line in output if line.startswith('VALUE ')}
            return {'status': 'recovered', 'sequence': int(metadata[0].split()[1]), 'state': state, 'boot': boot, 'mount': [line for line in output if line.startswith('/dev/vda ')]}
        finally:
            if guest: guest.stop()
            disk.power_cut()


def scenario(root, output, filesystem, number, boundary, acknowledged, negative):
    label = f'{filesystem}-{number:02d}-' + ('negative-flush' if negative else boundary)
    directory = output / label; directory.mkdir()
    work = root / label; work.mkdir()
    image = work / 'fresh.raw'
    seed = 100 + number
    row = {'id': label, 'filesystem': filesystem, 'seed': seed, 'boundary': boundary, 'acknowledged_operations': acknowledged, 'negative_control': negative, 'status': 'running'}
    disk, guest = None, None
    try:
        with (directory / 'blocks.jsonl').open('w') as trace, (directory / 'ack.jsonl').open('w') as ledger:
            disk = Disk(image, work / 'disk.sock', trace); disk.serve()
            guest = Guest(disk, directory, 'workload')
            row['qemu_command'] = guest.command
            row['boot'] = guest.until('BOOT ')[-1]
            guest.send(f'FORMAT {filesystem}')
            initial = guest.until('READY')
            assert initial[-1] == 'READY', initial[-1]
            row['mount'] = [line for line in initial if line.startswith('/dev/vda ')]
            assert len(row['mount']) == 1
            assert 'nobarrier' not in row['mount'][0] and 'writeback' not in row['mount'][0]
            with disk.lock:
                # Establish a known durable filesystem baseline BEFORE any Db::open or mutation.
                # XFS may issue background log initialization after guest sync returns.
                row['baseline_checkpoint_bytes'] = len(disk.dirty) * 512
                disk.flush()
                disk.record(event='baseline-checkpoint', flushed_bytes=row['baseline_checkpoint_bytes'])
                if negative: disk.ignore_flush = True
            guest.send(f'RUN {seed}')
            state, sequence = {}, 0
            for step in range(1, acknowledged + 1):
                guest.send('GO')
                begin = guest.until('BEGIN ')[-1].split()
                ack = guest.until('ACK ')[-1].split()
                assert int(begin[1]) == step and ack[1] == begin[1]
                assert ack[3:] == begin[2:]
                state, sequence = apply(state, sequence, *ack[3:])
                assert sequence == int(ack[2])
                ledger.write(json.dumps({'begin': begin, 'ack': ack, 'state': state, 'sequence': sequence}) + '\n')
                ledger.flush(); os.fsync(ledger.fileno())
            row['acknowledged_state'], row['acknowledged_sequence'] = state, sequence
            allowed = [(state, sequence)]
            if boundary != 'ack' and not negative:
                disk.arm(boundary)
                guest.send('GO')
                pending = guest.until('BEGIN ')[-1].split()
                assert int(pending[1]) == acknowledged + 1
                row['pending'] = pending
                allowed.append(apply(state, sequence, *pending[2:]))
                if not disk.hit.wait(60): raise TimeoutError(f'guest never reached {boundary}')
            # Freeze this guest before removing device power, then SIGKILL it: no shutdown,
            # unmount, guest sync, or QEMU cache drain is requested at this boundary.
            os.kill(guest.process.pid, signal.SIGSTOP)
            row['device_power_cut'] = disk.power_cut(); disk = None
            guest.stop(); guest = None
            # Preserve immutable pre-recovery bytes for replay, including filesystem journals.
            with image.open('rb') as source, gzip.open(directory / 'pre-recovery.raw.gz', 'wb', compresslevel=1) as target:
                shutil.copyfileobj(source, target, 1024 * 1024)
            with (directory / 'pre-recovery.raw.gz').open('rb') as archive:
                row['pre_recovery_archive_sha256'] = hashlib.file_digest(archive, 'sha256').hexdigest()
            recovered = recover(image, work, directory, filesystem)
            row['recovery'] = recovered
            matches = recovered['status'] == 'recovered' and (recovered['state'], recovered['sequence']) in allowed
            if negative:
                assert row['device_power_cut']['counts']['ignored_flush'] > 0
                assert row['device_power_cut']['discarded_bytes'] > 0
                assert not matches, 'negative control failed: a lying disk escaped the oracle'
                row['status'] = 'negative-control-detected'
            else:
                assert matches, 'acknowledged state/version loss or forbidden recovery outcome'
                row['status'] = 'passed'
            return row
    except BaseException as error:
        row['status'] = 'failed'; row['error'] = str(error)
        (directory / 'failure.log').write_text(traceback.format_exc())
        raise
    finally:
        if guest: guest.stop()
        if disk:
            try: disk.power_cut()
            except BaseException as error: row['cleanup_error'] = str(error)
        if row['status'] == 'failed' and image.exists() and not (directory / 'pre-recovery.raw.gz').exists():
            with image.open('rb') as source, gzip.open(directory / 'failure.raw.gz', 'wb', compresslevel=1) as target:
                shutil.copyfileobj(source, target, 1024 * 1024)
        (directory / 'results.json').write_text(json.dumps(row, indent=2) + '\n')
        if row['status'] != 'failed': shutil.rmtree(work)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--output', type=Path, default=Path('/results'))
    parser.add_argument('--filesystems', nargs='+', choices=['ext4', 'xfs'], default=['ext4', 'xfs'])
    parser.add_argument('--cases', type=int, default=10)
    args = parser.parse_args()
    assert platform.system() == 'Linux' and Path('/.dockerenv').exists(), 'use the restricted harness container'
    assert 1 <= args.cases <= len(PLAN)
    assert os.geteuid() != 0, 'run as non-root; no devices or capabilities are required'
    output = args.output.resolve()
    assert output.is_dir() and not any(output.iterdir()), 'use a fresh empty results directory'
    work = Path('/work'); assert work.is_dir() and not any(work.iterdir()), 'use a fresh temporary work mount'
    report = {'status': 'running', 'source_sha256': digest(), 'ci_sha': os.environ.get('GITHUB_SHA') or None, 'host_kernel': platform.release(), 'machine': platform.machine(), 'qemu': subprocess.check_output(['qemu-system-aarch64' if platform.machine() == 'aarch64' else 'qemu-system-x86_64', '--version'], text=True).splitlines()[0], 'guest_kernel': (APP / 'guest/kernel-version').read_text().strip(), 'rust': subprocess.check_output(['rustc', '--version'], text=True).strip(), 'kernel_sha256': hashlib.sha256((APP / 'guest/kernel').read_bytes()).hexdigest(), 'initrd_sha256': hashlib.sha256((APP / 'guest/initrd.gz').read_bytes()).hexdigest(), 'scope': 'abrupt QEMU guest RAM loss plus discarded unflushed NBD cache; not physical host/drive power loss', 'cases': []}
    started = time.monotonic()
    try:
        with tempfile.TemporaryDirectory(prefix='yali-', dir=work) as folder:
            root = Path(folder)
            for filesystem in args.filesystems:
                for number, (boundary, count) in enumerate(PLAN[:args.cases], 1):
                    row = scenario(root, output, filesystem, number, boundary, count, False)
                    report['cases'].append(row)
                    print(json.dumps({k: row[k] for k in ('id', 'status', 'acknowledged_operations', 'device_power_cut')}), flush=True)
                    (output / 'results.json').write_text(json.dumps(report, indent=2) + '\n')
                row = scenario(root, output, filesystem, args.cases + 1, 'ack', 3, True)
                report['cases'].append(row)
                print(json.dumps({'id': row['id'], 'status': row['status']}), flush=True)
        report['status'] = 'passed'
    except BaseException as error:
        report['status'] = 'failed'; report['error'] = str(error)
        raise
    finally:
        report['elapsed_seconds'] = round(time.monotonic() - started, 3)
        (output / 'results.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__': main()
