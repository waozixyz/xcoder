#!/usr/bin/env python3
"""Run project builds with low CPU/I/O priority and a shared RAM/swap budget."""
import fcntl
import os
from pathlib import Path
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
UNIT = 'xcoder-compilation.scope'


def call(*arguments):
    return subprocess.run(['busctl', '--user', 'call', 'org.freedesktop.systemd1',
                           '/org/freedesktop/systemd1', 'org.freedesktop.systemd1.Manager',
                           *arguments], capture_output=True, text=True)


def main():
    if len(sys.argv) < 2:
        raise SystemExit('Supply the build command and its arguments')
    os.setpriority(os.PRIO_PROCESS, 0, 19)
    subprocess.run(['ionice', '-c', '2', '-n', '7', '-p', str(os.getpid())], check=True)
    if os.environ.get('XCODER_BUILD_SCOPE') != '0' and ('/' + UNIT) not in Path('/proc/self/cgroup').read_text():
        (ROOT / 'build').mkdir(exist_ok=True)
        with (ROOT / 'build/resource-scope.lock').open('a') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            joined = call('AttachProcessesToUnit', 'ssau', UNIT, '', '1', str(os.getpid()))
            if joined.returncode:
                created = call('StartTransientUnit', 'ssa(sv)a(sa(sv))', UNIT, 'fail', '8',
                               'Description', 's', 'Xcoder background compilation',
                               'CPUWeight', 't', '1', 'IOWeight', 't', '1',
                               'MemoryHigh', 't', '1610612736', 'MemoryMax', 't', '2147483648',
                               'MemorySwapMax', 't', '68719476736',
                               'CollectMode', 's', 'inactive-or-failed',
                               'PIDs', 'au', '1', str(os.getpid()), '0')
                if created.returncode:
                    raise SystemExit('Cannot apply the Xcoder RAM/swap budget: ' + created.stderr.strip()
                                     + '\nA user systemd manager is required. Set XCODER_BUILD_SCOPE=0 only to explicitly omit the memory budget.')
            deadline = time.monotonic() + 10
            while ('/' + UNIT) not in Path('/proc/self/cgroup').read_text():
                if time.monotonic() >= deadline:
                    raise SystemExit('The user manager did not attach this build to its resource scope')
                time.sleep(0.01)
    print('Build resources: nice 19, best-effort I/O priority 7; RAM soft 1.5 GiB/hard 2 GiB, swap up to 64 GiB'
          if os.environ.get('XCODER_BUILD_SCOPE') != '0' else 'Build resources: nice 19, best-effort I/O priority 7; memory scope explicitly disabled',
          file=sys.stderr, flush=True)
    os.execvp(sys.argv[1], sys.argv[1:])


if __name__ == '__main__':
    main()
