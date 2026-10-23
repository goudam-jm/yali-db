"""Host cleanup must own its logs; the recovery oracle must compare every field."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from run import recovery_matches

spec = importlib.util.spec_from_file_location('vm_host_runner', Path(__file__).resolve().parents[1] / 'run-vm-powercut.py')
host = importlib.util.module_from_spec(spec)
spec.loader.exec_module(host)


class ControllerTests(unittest.TestCase):
    def test_private_logs_preserve_siblings_on_success_and_guest_failure(self):
        for guest_fails, log_fails in ((False, False), (True, False), (False, True)):
            with self.subTest(guest_fails=guest_fails, log_fails=log_fails), tempfile.TemporaryDirectory() as folder:
                output = Path(folder) / 'run.one'
                sibling = Path(folder) / 'run.log'
                sibling.write_text('unrelated log')
                def docker_run(command, **kwargs):
                    if command[1] == 'run':
                        kwargs['stdout'].write('guest log\n'); kwargs['stdout'].flush()
                        self.assertEqual(list(output.iterdir()), [])
                        if guest_fails: raise subprocess.CalledProcessError(1, command)
                        (output / 'results.json').write_text(json.dumps({'status': 'passed', 'cases': [], 'machine': 'aarch64'}))
                original_copy = host.shutil.copyfileobj
                def save_log(source, target):
                    if log_fails: raise OSError('report filesystem full')
                    original_copy(source, target)
                argv = ['runner', '--image', 'test', '--platform', 'linux/arm64', '--output', str(output)]
                with patch('builtins.print'), patch.object(host.shutil, 'copyfileobj', side_effect=save_log), patch.object(sys, 'argv', argv), patch.object(host.os, 'geteuid', return_value=1001), patch.object(host.subprocess, 'run', side_effect=docker_run) as called, patch.object(host.subprocess, 'check_output', side_effect=['image-id', 'arm64']):
                    if log_fails:
                        with self.assertRaises(OSError): host.main()
                    elif guest_fails:
                        with self.assertRaises(subprocess.CalledProcessError): host.main()
                    else:
                        host.main()
                self.assertEqual(sibling.read_text(), 'unrelated log')
                if not log_fails: self.assertEqual((output / 'controller.log').read_text(), 'guest log\n')
                self.assertEqual(called.call_args_list[-1].args[0][1:3], ['volume', 'rm'])

    def test_recovery_requires_full_state_versions_and_allowed_sequence(self):
        committed = {'slot:1': [10, 1]}
        pending = {**committed, 'slot:2': [20, 2]}
        allowed = [(committed, 1), (pending, 2)]
        cases = [
            (committed, 1, True), (pending, 2, True),
            ({'slot:1': [99, 1]}, 1, False),
            ({'slot:1': [10, 99]}, 1, False),
            ({}, 1, False), (committed, 2, False), (pending, 1, False),
            ({**pending, 'invented': [30, 3]}, 3, False),
        ]
        for state, sequence, expected in cases:
            with self.subTest(state=state, sequence=sequence):
                self.assertEqual(recovery_matches({'status': 'recovered', 'state': state, 'sequence': sequence}, allowed), expected)
        self.assertFalse(recovery_matches({'status': 'unrecoverable'}, allowed))


if __name__ == '__main__': unittest.main()
