#!/usr/bin/env python3
"""Run the complete local suite and certify only the binaries it tested."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
RECEIPT = ROOT / 'backup/xcoder-validation.json'


def fingerprints():
    values = {}
    for key, name in [('binary_sha256', 'xcoder'), ('host_sha256', 'codex-code-mode-host')]:
        with (ROOT / 'build/bin' / name).open('rb') as stream:
            values[key] = hashlib.file_digest(stream, 'sha256').hexdigest()
    return values


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cargo', required=True)
    parser.add_argument('--ui-features', default='')
    parser.add_argument('--existing-binaries', action='store_true',
                        help='Test the existing packaged executables without rebuilding products; all local test suites still run')
    args = parser.parse_args()
    subprocess.run(['git', 'check-ignore', '-q', str(RECEIPT)], cwd=ROOT, check=True)
    RECEIPT.parent.mkdir(exist_ok=True, mode=0o700)
    # A failed rerun must never leave an earlier success available to install.
    RECEIPT.unlink(missing_ok=True)
    env = {key: value for key, value in os.environ.items() if key not in
           ['DISPLAY', 'WAYLAND_DISPLAY', 'XAUTHORITY', 'DBUS_SESSION_BUS_ADDRESS',
            'XCODER_TEST_HOST', 'XCODER_TEST_CLI']}
    env['CARGO'] = args.cargo
    targets = ['native-test'] if args.existing_binaries else ['native-test', 'all']
    subprocess.run(['make', *targets, 'UI_FEATURES=' + args.ui_features],
                   cwd=ROOT, env=env, check=True)
    tested = fingerprints()
    runtime = ROOT / 'build/native-runtime'
    runtime.mkdir(exist_ok=True, mode=0o700)
    runtime.chmod(0o700)
    env['XCODER_RUNTIME_DIR'] = str(runtime)
    commands = [
        [args.cargo, 'test', '--manifest-path', 'codex-rs/Cargo.toml', '--release',
         '-p', package, '--lib']
        for package in ['codex-code-mode-runtime', 'codex-code-mode-protocol']
    ]
    commands += [[sys.executable, 'tests/' + name]
                 for name in ['validation_test.py', 'native_host_test.py', 'native_cli_test.py', 'install_xcoder_test.py']]
    for command in commands:
        subprocess.run(command, cwd=ROOT, env=env, check=True)
    if tested != fingerprints():
        raise RuntimeError('Packaged binaries changed during local tests; rerun make test')
    receipt = dict(local_tests_passed=True, tested_at=int(time.time()), **tested)
    descriptor, temporary = tempfile.mkstemp(dir=RECEIPT.parent, prefix='.xcoder-validation-')
    try:
        with os.fdopen(descriptor, 'w') as stream:
            json.dump(receipt, stream, indent=2)
            stream.write('\n')
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, RECEIPT)
    finally:
        Path(temporary).unlink(missing_ok=True)
    print('Local suite passed; installation receipt saved for the tested binaries.', flush=True)


if __name__ == '__main__':
    main()
