"""Statistics, workload identity and SQLite's versioned transaction contract."""
import tempfile
import sqlite3
from pathlib import Path
import unittest
from run import connect, indices, parse, percentile, put


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


if __name__ == '__main__': unittest.main()
