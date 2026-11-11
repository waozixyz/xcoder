#!/usr/bin/env python3
"""Install a verified native fork, preserving Harmony's command and enrollment."""
import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def atomic_copy(source, target):
    fd, name = tempfile.mkstemp(dir=target.parent, prefix=target.name + '.new-')
    try:
        with os.fdopen(fd, 'wb') as output, source.open('rb') as data:
            shutil.copyfileobj(data, output)
            output.flush()
            os.fsync(output.fileno())
        os.chmod(name, 0o755)
        os.replace(name, target)
    finally:
        Path(name).unlink(missing_ok=True)


def publish_package_entry(entry, native, compatibility):
    """Keep the old executable's live dentry while switching new launches."""
    if compatibility.exists() or compatibility.is_symlink():
        raise FileExistsError(str(compatibility))
    libc = ctypes.CDLL(None, use_errno=True)
    exchange = libc.renameat2
    exchange.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
    exchange.restype = ctypes.c_int
    compatibility.symlink_to(native)
    # RENAME_EXCHANGE preserves the running old program at its compatibility
    # name while publishing the new symlink, with no missing-entry interval.
    if exchange(-100, os.fsencode(entry), -100, os.fsencode(compatibility), 2) != 0:
        error = ctypes.get_errno()
        compatibility.unlink()
        raise OSError(error, os.strerror(error), str(entry))


def install(prefix, before_cutover=None):
    binary = ROOT / 'build/bin/xcoder'
    host = ROOT / 'build/bin/codex-code-mode-host'
    metadata = json.loads(subprocess.check_output([str(binary), '--xcoder-runtime-info'], text=True))
    if metadata != dict(product='xcoder', tool_language='ziran', pipeline='zi2c', javascript_tool_runtime=False):
        raise ValueError('Not the native Xcoder build')
    # The validation receipt binds both executables to their actual local test.
    validation = json.loads((ROOT / 'backup/xcoder-validation.json').read_text())
    expected = dict(binary_sha256=digest(binary), host_sha256=digest(host))
    if not validation.get('local_tests_passed') or any(validation.get(key) != value for key, value in expected.items()):
        raise ValueError('Run the integrated local tests for these exact executables before installation')
    backup = ROOT / 'backup/xcoder-install'
    subprocess.run(['git', 'check-ignore', '-q', 'backup/xcoder-install'], cwd=ROOT, check=True)
    backup.mkdir(parents=True, exist_ok=True, mode=0o700)
    destination = prefix / 'lib/xcoder/bin'
    destination.mkdir(parents=True, exist_ok=True)
    managed = prefix / 'lib/harmony/agent-bin/codex'
    target = managed if managed.is_symlink() or managed.is_file() else prefix / 'bin/codex'
    target.parent.mkdir(parents=True, exist_ok=True)
    original = {'path': str(target), 'symlink': os.readlink(target) if target.is_symlink() else None}
    alias = prefix / 'bin/xcoder'
    alias.parent.mkdir(parents=True, exist_ok=True)
    original['xcoder_alias'] = {'path': str(alias), 'symlink': os.readlink(alias) if alias.is_symlink() else None}
    if alias.is_file() and not alias.is_symlink():
        saved = backup / ('xcoder-command-' + digest(alias))
        if not saved.exists():
            shutil.copy2(alias, saved)
        original['xcoder_alias']['saved'] = str(saved)
    original['previous_native'] = {}
    for name in ['codex', 'codex-code-mode-host', 'codex.xcoder.json']:
        previous = destination / name
        if previous.is_file():
            saved = backup / (name + '-' + digest(previous))
            if not saved.exists():
                shutil.copy2(previous, saved)
            original['previous_native'][name] = str(saved)
    package_entry = target.resolve() if target.exists() else None
    package_root = Path.home() / '.codex/packages/standalone'
    if package_entry and package_entry.name == 'codex' and package_entry.is_relative_to(package_root):
        # The desktop app also launches this registered package entry directly.
        # Preserve the running inode and original bytes before the atomic switch.
        saved = backup / ('package-codex-' + digest(package_entry))
        if not saved.exists():
            shutil.copy2(package_entry, saved)
        original['package_entry'] = str(package_entry)
        original['package_saved'] = str(saved)
        compatibility = package_entry.with_name('codex-upstream')
        if compatibility.exists() or compatibility.is_symlink():
            raise FileExistsError(str(compatibility))
        original['package_compatibility_entry'] = str(compatibility)
    if target.is_file() and not target.is_symlink():
        saved = backup / ('codex-' + digest(target))
        if not saved.exists():
            shutil.copy2(target, saved)
        original['saved'] = str(saved)
    receipt = backup / ('original-' + str(time.time_ns()) + '.json')
    receipt.write_text(json.dumps(original, indent=2) + '\n')
    receipt.chmod(0o600)
    atomic_copy(host, destination / 'codex-code-mode-host')
    atomic_copy(binary, destination / 'codex')
    native_receipt = dict(metadata, **expected)
    receipt_path = destination / 'codex.xcoder.json'
    fd, receipt_stage = tempfile.mkstemp(dir=destination, prefix='.native-receipt-')
    try:
        with os.fdopen(fd, 'w') as stream:
            json.dump(native_receipt, stream, indent=2)
            stream.write('\n')
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(receipt_stage, receipt_path)
    finally:
        Path(receipt_stage).unlink(missing_ok=True)
    for name in ['LICENSE', 'NOTICE']:
        shutil.copyfile(ROOT / name, destination.parent / name)
    # A coordinated local installer can publish its validated routing helper
    # after the native image is ready and immediately before launch paths move.
    if before_cutover is not None:
        before_cutover()
    if original.get('package_entry'):
        entry = Path(original['package_entry'])
        publish_package_entry(entry, destination / 'codex', Path(original['package_compatibility_entry']))
    link = target.with_name(target.name + '.xcoder-new')
    if link.exists() or link.is_symlink():
        raise FileExistsError(str(link))
    link.symlink_to(destination / 'codex')
    os.replace(link, target)
    alias_link = alias.with_name(alias.name + '.xcoder-new')
    if alias_link.exists() or alias_link.is_symlink():
        raise FileExistsError(str(alias_link))
    alias_link.symlink_to(destination / 'codex')
    os.replace(alias_link, alias)
    installed = json.loads(subprocess.check_output([str(target), '--xcoder-runtime-info'], text=True))
    if installed != metadata or digest(destination / 'codex') != expected['binary_sha256'] or digest(destination / 'codex-code-mode-host') != expected['host_sha256']:
        raise ValueError('Installed Xcoder verification failed; original target retained in ' + str(receipt))
    if original.get('package_entry'):
        registered = json.loads(subprocess.check_output([original['package_entry'], '--xcoder-runtime-info'], text=True))
        if registered != metadata:
            raise ValueError('Registered Codex package entry verification failed')
    print(json.dumps(dict(installed=str(target), binary=str(destination / 'codex'), recovery=str(receipt), **expected)))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--prefix', type=Path, default=Path.home() / '.local')
    install(parser.parse_args().prefix.resolve())
