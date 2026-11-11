"""Installation certification must survive failed and concurrent test runs."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('xcoder_validation', ROOT / 'scripts/test-xcoder.py')
validation = importlib.util.module_from_spec(spec)
spec.loader.exec_module(validation)


class ValidationTest(unittest.TestCase):
    def run_suite(self, failure=False, changed=False):
        parent = ROOT / 'build/validation-tests'
        parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=parent) as temporary:
            root = Path(temporary)
            (root / 'build/bin').mkdir(parents=True)
            for name in ['xcoder', 'codex-code-mode-host']:
                (root / 'build/bin' / name).write_bytes(b'test executable')
            receipt = root / 'backup/xcoder-validation.json'
            receipt.parent.mkdir()
            receipt.write_text('{"local_tests_passed":true}')

            def command(args, **kwargs):
                if args[-1] == 'tests/native_cli_test.py':
                    self.assertFalse(receipt.exists(), 'old success survived into the new test run')
                    if failure:
                        raise subprocess.CalledProcessError(1, args)
                    if changed:
                        (root / 'build/bin/xcoder').write_bytes(b'concurrent replacement')
                return subprocess.CompletedProcess(args, 0)

            with patch.object(validation, 'ROOT', root), patch.object(validation, 'RECEIPT', receipt), \
                 patch.object(validation.subprocess, 'run', side_effect=command), \
                 patch.object(sys, 'argv', ['test-xcoder.py', '--cargo', 'cargo']):
                if failure or changed:
                    with self.assertRaises(subprocess.CalledProcessError if failure else RuntimeError):
                        validation.main()
                    self.assertFalse(receipt.exists(), 'failed suite certified an install')
                else:
                    validation.main()
                    saved = json.loads(receipt.read_text())
                    self.assertTrue(saved['local_tests_passed'])
                    self.assertEqual({key: saved[key] for key in validation.fingerprints()},
                                     validation.fingerprints())
                    self.assertEqual(receipt.stat().st_mode & 0o777, 0o600)

    def test_success_certifies_the_exact_executables(self):
        self.run_suite()

    def test_failed_rerun_removes_previous_certificate(self):
        self.run_suite(failure=True)

    def test_binary_replaced_during_tests_is_not_certified(self):
        self.run_suite(changed=True)


if __name__ == '__main__':
    unittest.main()
