"""Bounded Linux embedded measurements; SQLite comes from Python's standard library."""
import argparse
from array import array
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time

ROOT = Path(__file__).resolve().parents[2]
MASK = (1 << 64) - 1


def indices(seed, count, keys):
    for _ in range(count):
        seed ^= seed << 13; seed &= MASK
        seed ^= seed >> 7
        seed ^= seed << 17; seed &= MASK
        yield seed % keys


def resource(stage):
    status = dict(row.split(':', 1) for row in Path('/proc/self/status').read_text().splitlines() if ':' in row)
    accounting = dict(row.split(':', 1) for row in Path('/proc/self/io').read_text().splitlines())
    for field, name in [('VmRSS', 'rss_bytes'), ('VmHWM', 'peak_rss_bytes')]:
        print(f'STAT\t{stage}\t{name}\t{int(status[field].split()[0]) * 1024}')
    print(f'STAT\t{stage}\tprocess_write_bytes\t{int(accounting["write_bytes"])}')


def connect(path):
    connection = sqlite3.connect(path, isolation_level=None)
    assert connection.execute('PRAGMA journal_mode=WAL').fetchone()[0] == 'wal'
    connection.execute('PRAGMA synchronous=FULL')
    connection.execute('PRAGMA wal_autocheckpoint=1000')
    connection.execute('PRAGMA cache_size=-2048')
    connection.execute('PRAGMA mmap_size=0')
    for pragma, expected in [('synchronous', 2), ('wal_autocheckpoint', 1000), ('cache_size', -2048), ('mmap_size', 0)]:
        assert connection.execute(f'PRAGMA {pragma}').fetchone()[0] == expected
    return connection


def put(connection, key, value, sequence):
    connection.execute('BEGIN IMMEDIATE')
    try:
        connection.execute('INSERT INTO kv VALUES (?, ?, ?) ON CONFLICT(k) DO UPDATE SET v=excluded.v, version=excluded.version', (key, value, sequence))
        connection.execute('UPDATE metadata SET sequence=?', (sequence,))
        connection.execute('COMMIT')
    except BaseException:
        connection.execute('ROLLBACK')
        raise


def samples(phase, times, elapsed):
    for value in times: print(f'SAMPLE\t{phase}\t{value}')
    print(f'ELAPSED\t{phase}\t{elapsed}\t{len(times)}')


def sqlite_worker(args):
    path = args.database
    assert (not path.exists()) if args.worker == 'load' else path.is_file()
    resource('runtime')
    started = time.perf_counter_ns()
    connection = connect(path)
    if args.worker == 'load':
        connection.executescript('CREATE TABLE kv(k BLOB PRIMARY KEY, v BLOB NOT NULL, version INTEGER NOT NULL) WITHOUT ROWID; CREATE TABLE metadata(sequence INTEGER NOT NULL); INSERT INTO metadata VALUES (0);')
    sequence = connection.execute('SELECT sequence FROM metadata').fetchone()[0]
    print(f'DURATION\topen\t{time.perf_counter_ns() - started}')
    resource('opened')
    if args.worker == 'load':
        assert sequence == 0
        times = array('Q')
        phase = time.perf_counter_ns()
        for index in range(args.keys):
            key, value = index.to_bytes(8, 'big'), bytes([index % 251]) * args.value_bytes
            started = time.perf_counter_ns()
            put(connection, key, value, index + 1)
            times.append(time.perf_counter_ns() - started)
        elapsed = time.perf_counter_ns() - phase
        resource('populated'); samples('populate', times, elapsed)
        times = array('Q')
        phase = time.perf_counter_ns()
        for index in indices(args.seed, args.operations, args.keys):
            key = index.to_bytes(8, 'big')
            started = time.perf_counter_ns()
            row = connection.execute('SELECT v, version FROM kv WHERE k=?', (key,)).fetchone()
            times.append(time.perf_counter_ns() - started)
            assert row == (bytes([index % 251]) * args.value_bytes, index + 1)
        samples('warm_get', times, time.perf_counter_ns() - phase)
        times = array('Q')
        phase = time.perf_counter_ns()
        for operation, index in enumerate(indices(args.seed, args.operations, args.keys)):
            key, value = index.to_bytes(8, 'big'), bytes([(index + 1) % 251]) * args.value_bytes
            started = time.perf_counter_ns()
            put(connection, key, value, args.keys + operation + 1)
            times.append(time.perf_counter_ns() - started)
        elapsed = time.perf_counter_ns() - phase
        resource('updated'); samples('overwrite', times, elapsed)
        sys.stdout.flush()
        # Bypass sqlite3_close so recovery measures an uncheckpointed process exit.
        os._exit(0)
    assert sequence == args.keys + args.operations
    started = time.perf_counter_ns()
    assert connection.execute('PRAGMA wal_checkpoint(TRUNCATE)').fetchone()[0] == 0
    connection.execute('VACUUM')
    assert connection.execute('PRAGMA wal_checkpoint(TRUNCATE)').fetchone()[0] == 0
    print(f'DURATION\tmaintenance\t{time.perf_counter_ns() - started}')
    resource('maintained')
    updated = {index: args.keys + operation + 1 for operation, index in enumerate(indices(args.seed, args.operations, args.keys))}
    assert connection.execute('SELECT count(*) FROM kv').fetchone()[0] == args.keys
    for index in range(args.keys):
        row = connection.execute('SELECT v, version FROM kv WHERE k=?', (index.to_bytes(8, 'big'),)).fetchone()
        assert row == (bytes([(index + (index in updated)) % 251]) * args.value_bytes, updated.get(index, index + 1))
    print(f'VERIFIED\t{args.keys}\t{sequence}')
    connection.close()


def percentile(values, fraction):
    if not values or not 0 < fraction <= 1: raise ValueError('nonempty values and quantile in (0,1] required')
    return sorted(values)[math.ceil(fraction * len(values)) - 1]


def parse(text):
    result = {'samples_ns': {}, 'elapsed_ns': {}, 'resources': {}, 'durations_ns': {}}
    for line in text.splitlines():
        row = line.split('\t')
        if row[0] == 'SAMPLE': result['samples_ns'].setdefault(row[1], []).append(int(row[2]))
        elif row[0] == 'ELAPSED': result['elapsed_ns'][row[1]] = {'nanoseconds': int(row[2]), 'count': int(row[3])}
        elif row[0] == 'STAT': result['resources'].setdefault(row[1], {})[row[2]] = int(row[3])
        elif row[0] == 'DURATION': result['durations_ns'][row[1]] = int(row[2])
        elif row[0] == 'VERIFIED': result['verified'] = [int(row[1]), int(row[2])]
        else: raise ValueError(f'unknown worker output: {row[0]}')
    for phase, elapsed in result['elapsed_ns'].items():
        assert elapsed['count'] == len(result['samples_ns'][phase]) and elapsed['nanoseconds'] > 0
    return result


def disk_size(directory):
    logical, allocated = 0, 0
    for path in directory.rglob('*'):
        try:
            info = path.stat()
            if path.is_file(): logical += info.st_size; allocated += info.st_blocks * 512
        except FileNotFoundError:
            pass
    return {'logical_bytes': logical, 'allocated_bytes': allocated}


def worker(command, directory, output):
    stop = threading.Event()
    peak = disk_size(directory)
    errors = []
    def sample():
        try:
            while not stop.wait(0.005):
                current = disk_size(directory)
                for key, value in current.items(): peak[key] = max(peak[key], value)
        except BaseException as error: errors.append(error)
    thread = threading.Thread(target=sample, daemon=True); thread.start()
    try:
        completed = subprocess.run(command, env={**os.environ, 'TMPDIR': str(directory), 'SQLITE_TMPDIR': str(directory)}, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=900)
        output.with_suffix('.tsv').write_text(completed.stdout)
        output.with_suffix('.stderr').write_text(completed.stderr)
        assert completed.returncode == 0, f'worker failed: inspect {output.with_suffix(".stderr")}'
        result = parse(completed.stdout)
    finally:
        stop.set(); thread.join(5)
    assert not thread.is_alive() and not errors, errors
    current = disk_size(directory)
    for key, value in current.items(): peak[key] = max(peak[key], value)
    result['sampled_peak_database_files'] = peak
    result['final_disk'] = current
    return result


def source_digest():
    paths = [ROOT / 'Cargo.toml', ROOT / 'Cargo.lock', ROOT / 'yalidb-core/Cargo.toml', ROOT / 'yalidb-core/examples/benchmark.rs']
    paths += sorted((ROOT / 'yalidb-core/src').rglob('*.rs')) + sorted(Path(__file__).resolve().parent.glob('*.py'))
    digest = hashlib.sha256()
    for path in paths: digest.update(str(path.relative_to(ROOT)).encode() + b'\0' + path.read_bytes())
    return digest.hexdigest()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--output', type=Path)
    parser.add_argument('--work-dir', type=Path, help='existing parent for a new private temporary data directory')
    parser.add_argument('--keys', type=int, default=10_000)
    parser.add_argument('--value-bytes', type=int, default=1024)
    parser.add_argument('--operations', type=int, default=1000)
    parser.add_argument('--repetitions', type=int, default=3)
    parser.add_argument('--seed', type=int, default=42)
    parser.add_argument('--worker', choices=['load', 'recover'], help=argparse.SUPPRESS)
    parser.add_argument('--database', type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    assert platform.system() == 'Linux', 'process RSS/I/O accounting requires Linux'
    assert 1 <= args.keys <= 100_000 and 1 <= args.value_bytes <= 65536 and 1 <= args.operations <= 100_000
    assert 1 <= args.repetitions <= 10 and 0 < args.seed < MASK - 10
    assert (args.keys + args.operations) * (args.value_bytes + 49) < 60 * 1024 * 1024, 'workload exceeds the default-log benchmark budget'
    if args.worker:
        assert args.database is not None
        sqlite_worker(args); return
    assert args.output is not None, 'output directory required'
    args.output = args.output.resolve()
    assert not args.output.exists(), 'choose a new output directory'
    args.output.mkdir(parents=True)
    work = Path(tempfile.mkdtemp(prefix='yalidb-benchmark-', dir=args.work_dir or args.output))
    mount = json.loads(subprocess.check_output(['findmnt', '-J', '--target', str(work), '-o', 'FSTYPE,OPTIONS'], text=True))
    report = {'status': 'running', 'recorded_at_utc': time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()), 'source_sha256': source_digest(), 'ci_sha': os.environ.get('GITHUB_SHA'), 'kernel': platform.release(), 'machine': platform.machine(), 'cpu_count': os.cpu_count(), 'cpu_model': next((row.split(':', 1)[1].strip() for row in Path('/proc/cpuinfo').read_text().splitlines() if row.startswith(('model name', 'Model', 'Processor')) and ':' in row), 'not reported'), 'cgroup_cpu_max': Path('/sys/fs/cgroup/cpu.max').read_text().strip() if Path('/sys/fs/cgroup/cpu.max').exists() else None, 'cgroup_memory_max': Path('/sys/fs/cgroup/memory.max').read_text().strip() if Path('/sys/fs/cgroup/memory.max').exists() else None, 'mount': mount, 'rust': subprocess.check_output(['rustc', '--version'], text=True).strip(), 'disk_sampling': {'interval_ms': 5, 'scope': 'visible database-directory files; excludes unlinked temporary files', 'bound': 'observed lower bound'}, 'sqlite': sqlite3.sqlite_version, 'python': platform.python_version(), 'profile': {'keys': args.keys, 'key_bytes': 8, 'value_bytes': args.value_bytes, 'operations_per_phase': args.operations, 'repetitions': args.repetitions, 'seed': args.seed, 'concurrency': 1, 'arrival_model': 'closed-loop'}, 'sqlite_config': {'journal_mode': 'WAL', 'synchronous': 'FULL', 'wal_autocheckpoint_pages': 1000, 'cache_size_kib': 2048, 'mmap_size': 0, 'global_version_counter': True}, 'runs': []}
    try:
        for repetition in range(args.repetitions):
            # Alternate order to avoid always giving one engine the warmer host environment.
            engines = ('yalidb', 'sqlite') if repetition % 2 == 0 else ('sqlite', 'yalidb')
            for engine in engines:
                directory = work / f'{repetition}-{engine}'; directory.mkdir()
                path = directory / ('db' if engine == 'yalidb' else 'db.sqlite3')
                common = ['--keys', str(args.keys), '--value-bytes', str(args.value_bytes), '--operations', str(args.operations), '--seed', str(args.seed + repetition)]
                def command(mode):
                    if engine == 'yalidb': return [str(ROOT / 'target/release/examples/benchmark'), mode, str(path), str(args.keys), str(args.value_bytes), str(args.operations), str(args.seed + repetition)]
                    return [sys.executable, str(Path(__file__)), '--worker', mode, '--database', str(path), *common]
                loaded = worker(command('load'), directory, args.output / f'{repetition}-{engine}-load')
                recovered = worker(command('recover'), directory, args.output / f'{repetition}-{engine}-recover')
                assert recovered['verified'] == [args.keys, args.keys + args.operations]
                for phase, count in [('populate', args.keys), ('warm_get', args.operations), ('overwrite', args.operations)]:
                    assert len(loaded['samples_ns'][phase]) == count
                phases = {phase: {'count': len(values), **{f'p{q}_ns': percentile(values, q / 100) for q in (50, 95, 99)}, 'max_ns': max(values), 'throughput_ops_s': len(values) * 1e9 / loaded['elapsed_ns'][phase]['nanoseconds']} for phase, values in loaded.pop('samples_ns').items()}
                delta = loaded['resources']['populated']['rss_bytes'] - loaded['resources']['opened']['rss_bytes']
                written = loaded['resources']['updated']['process_write_bytes'] - loaded['resources']['runtime']['process_write_bytes'] + recovered['resources']['maintained']['process_write_bytes'] - recovered['resources']['runtime']['process_write_bytes']
                row = {'engine': engine, 'maintenance_operation': 'Db::compact' if engine == 'yalidb' else 'wal_checkpoint(TRUNCATE); VACUUM; wal_checkpoint(TRUNCATE)', 'repetition': repetition, 'phases': phases, 'load': loaded, 'recovery': recovered, 'incremental_process_rss_bytes_per_key': delta / args.keys, 'process_write_bytes_per_logical_payload_byte': written / ((args.keys + args.operations) * (8 + args.value_bytes))}
                report['runs'].append(row)
                (args.output / 'results.json').write_text(json.dumps(report, indent=2) + '\n')
                print(json.dumps({'engine': engine, 'repetition': repetition, 'verified': recovered['verified']}), flush=True)
        report['status'] = 'passed'
        shutil.rmtree(work)
    except BaseException as error:
        report['status'] = 'failed'; report['error'] = str(error)
        if work.parent != args.output:
            try: shutil.copytree(work, args.output / 'failure-data')
            except OSError as copy_error: report['failure_copy_error'] = str(copy_error)
        raise
    finally:
        (args.output / 'results.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__': main()
