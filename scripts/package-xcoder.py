#!/usr/bin/env python3
"""Publish build outputs atomically without replacing a running binary's inode."""
from pathlib import Path
import runpy
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[1]
copy = runpy.run_path(str(ROOT / 'scripts/install-xcoder.py'))['atomic_copy']
destination = ROOT / 'build/bin'
destination.mkdir(parents=True, exist_ok=True)
for name in ['xcoder', 'codex-code-mode-host']:
    # Keep the original release output for symbolication. Package only the
    # executable sections; temporary files stay beside the stable output.
    with tempfile.TemporaryDirectory(dir=destination, prefix='.package-') as directory:
        staged = Path(directory) / name
        copy(ROOT / 'codex-rs/target/release' / name, staged)
        subprocess.run(['strip', '--strip-unneeded', str(staged)], check=True)
        copy(staged, destination / name)
