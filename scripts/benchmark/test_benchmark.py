"""Statistics, workload identity and SQLite's versioned transaction contract."""
import tempfile
import os
import subprocess
import sys
import sqlite3
from pathlib import Path
import unittest
from types import SimpleNamespace
from unittest.mock import patch
import run as benchmark
from run import connect, indices, parse, percentile, put, verify_sqlite


class BenchmarkTests(unittest.TestCase):
    def test_nearest_rank_and_worker_count_validation(self):
        values = list(range(1, 101))
        self.assertEqual([percentile(values, q) for q in (0.5, 0.95, 0.99)], [50, 95, 99])
        self.assertEqual(percentile([7], 0.99), 7)
        with self.assertRaises(ValueError): percentile([], 0.99)
        with self.assertRaises(AssertionError): parse('SAMPLE\tput\t10\nELAPSED\tput\t100\t2\n')
        with self.assertRaises(ValueError): parse('unexpected\toutput\n')

    def test_workload_sequence_matches_unsigned_xorshift64(self):
        self.assertEqual(list(indices(42, 5, 10_000)), [5674, 5471, 954, 9736, 4162])

    def test_sqlite_settings_and_atomic_global_versions_survive_reopen(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / 'test.sqlite3'
            db = connect(path)
            db.executescript('CREATE TABLE kv(k BLOB PRIMARY KEY, v BLOB NOT NULL, version INTEGER NOT NULL) WITHOUT ROWID; CREATE TABLE metadata(sequence INTEGER NOT NULL); INSERT INTO metadata VALUES (0);')
            put(db, b'key', b'initial', 1)
            put(db, b'key', b'updated', 2)
            db.execute("CREATE TRIGGER refuse_metadata BEFORE UPDATE ON metadata BEGIN SELECT RAISE(ABORT, 'injected failure'); END")
            with self.assertRaisesRegex(sqlite3.IntegrityError, "injected failure"): put(db, b'key', b'forbidden', 3)
            db.close()
            db = connect(path)
            self.assertEqual(db.execute('SELECT v, version FROM kv').fetchall(), [(b'updated', 2)])
            self.assertEqual(db.execute('SELECT sequence FROM metadata').fetchone(), (2,))
            db.close()

    def test_final_verifier_rejects_counter_loss_with_intact_rows(self):
        with tempfile.TemporaryDirectory() as folder:
            db = connect(Path(folder) / 'test.sqlite3')
            db.executescript('CREATE TABLE kv(k BLOB PRIMARY KEY, v BLOB NOT NULL, version INTEGER NOT NULL) WITHOUT ROWID; CREATE TABLE metadata(sequence INTEGER NOT NULL); INSERT INTO metadata VALUES (0);')
            args = SimpleNamespace(keys=3, operations=1, seed=42, value_bytes=8)
            try:
                for index in range(args.keys):
                    put(db, index.to_bytes(8, 'big'), bytes([index]) * 8, index + 1)
                index = next(indices(args.seed, 1, args.keys))
                put(db, index.to_bytes(8, 'big'), bytes([index + 1]) * 8, 4)
                self.assertEqual(verify_sqlite(db, args), 4)
                db.execute('UPDATE metadata SET sequence=0')
                with self.assertRaises(AssertionError): verify_sqlite(db, args)
            finally:
                db.close()

    def test_optimized_execution_is_rejected_before_creating_output(self):
        with tempfile.TemporaryDirectory() as folder:
            for flags, optimized_env in ((['-O'], False), ([], True)):
                output = Path(folder) / ('environment' if optimized_env else 'flag')
                env = dict(os.environ)
                if optimized_env: env['PYTHONOPTIMIZE'] = '1'
                command = [sys.executable, *flags, str(Path(benchmark.__file__).resolve()), '--keys', '1', '--operations', '1', '--repetitions', '1', '--output', str(output)]
                result = subprocess.run(command, env=env, text=True, capture_output=True, timeout=10)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('benchmark requires assertions', result.stderr)
                self.assertFalse(output.exists())

    def test_original_error_survives_sqlite_automatic_rollback(self):
        with tempfile.TemporaryDirectory() as folder:
            db = connect(Path(folder) / 'test.sqlite3')
            try:
                db.executescript('CREATE TABLE kv(k BLOB PRIMARY KEY, v BLOB NOT NULL, version INTEGER NOT NULL) WITHOUT ROWID; CREATE TABLE metadata(sequence INTEGER NOT NULL); INSERT INTO metadata VALUES (0);')
                put(db, b'key', b'good', 1)
                db.execute("CREATE TRIGGER refuse_metadata BEFORE UPDATE ON metadata BEGIN SELECT RAISE(ROLLBACK, 'original failure'); END")
                with self.assertRaisesRegex(sqlite3.IntegrityError, 'original failure'):
                    put(db, b'key', b'bad', 2)
                self.assertFalse(db.in_transaction)
                self.assertEqual(db.execute('SELECT v, version FROM kv').fetchall(), [(b'good', 1)])
                self.assertEqual(db.execute('SELECT sequence FROM metadata').fetchone(), (1,))
            finally:
                db.close()

    def test_timeout_preserves_partial_output_and_original_exception(self):
        with tempfile.TemporaryDirectory() as folder:
            directory = Path(folder) / 'data'; directory.mkdir()
            output = Path(folder) / 'worker'
            for stdout, stderr, expected in ((b'partial\xff', b'error detail', 'partial\ufffd'), ('text output', None, 'text output')):
                error = subprocess.TimeoutExpired(['worker'], 900, output=stdout, stderr=stderr)
                with patch.object(benchmark.subprocess, 'run', side_effect=error):
                    with self.assertRaises(subprocess.TimeoutExpired) as raised:
                        benchmark.worker(['worker'], directory, output)
                self.assertIs(raised.exception, error)
                self.assertEqual(output.with_suffix('.tsv').read_text(), expected)
                self.assertEqual(output.with_suffix('.stderr').read_text(), 'error detail' if stderr else '')


if __name__ == '__main__': unittest.main()
