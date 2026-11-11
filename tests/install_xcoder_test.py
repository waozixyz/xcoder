"""Exercise atomic package cutover with a real, private running executable."""
import os
from pathlib import Path
import runpy
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
publish = runpy.run_path(str(ROOT / 'scripts/install-xcoder.py'))['publish_package_entry']


class PackageCutoverTest(unittest.TestCase):
    def setUp(self):
        parent = ROOT / 'build/install-tests'
        parent.mkdir(parents=True, exist_ok=True)
        self.directory = tempfile.TemporaryDirectory(dir=parent)
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name).resolve()

    def test_running_executable_keeps_its_inode_and_can_still_be_executed(self):
        entry = self.root / 'codex'
        compatibility = self.root / 'codex-upstream'
        native = self.root / 'xcoder'
        shutil.copy2('/usr/bin/sleep', entry)
        shutil.copy2('/usr/bin/true', native)
        inode = entry.stat().st_ino
        process = subprocess.Popen([str(entry), '60'])
        try:
            publish(entry, native, compatibility)
            self.assertEqual(compatibility.stat().st_ino, inode)
            self.assertEqual(Path(os.readlink(f'/proc/{process.pid}/exe')), compatibility)
            self.assertIsNone(process.poll())
            subprocess.run([str(entry)], check=True, timeout=5)
            subprocess.run([f'/proc/{process.pid}/exe', '0'], check=True, timeout=5)
        finally:
            process.terminate()
            process.wait(timeout=5)

    def test_existing_compatibility_entry_is_preserved(self):
        entry = self.root / 'codex'
        compatibility = self.root / 'codex-upstream'
        native = self.root / 'xcoder'
        entry.write_text('original')
        compatibility.write_text('preserve')
        native.write_text('native')
        with self.assertRaises(FileExistsError):
            publish(entry, native, compatibility)
        self.assertEqual(entry.read_text(), 'original')
        self.assertEqual(compatibility.read_text(), 'preserve')


if __name__ == '__main__':
    unittest.main()
