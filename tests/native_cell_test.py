"""Real Zi2c compilation, isolated native execution and callback framing."""
import json
import os
from pathlib import Path
import shutil
import socket
import struct
import subprocess
import tempfile
import threading
import unittest

ROOT = Path(__file__).resolve().parents[1]
ZIRAN = Path(os.environ['XCODER_ZIRAN_TOOLCHAIN']).resolve()


def base():
    command = ['bwrap', '--unshare-all', '--die-with-parent', '--new-session',
               '--cap-drop', 'ALL', '--clearenv', '--ro-bind', '/usr', '/usr',
               '--ro-bind', '/bin', '/bin', '--ro-bind', '/lib', '/lib',
               '--proc', '/proc', '--dev', '/dev', '--tmpfs', '/tmp', '--dir', '/etc',
               '--setenv', 'PATH', '/usr/bin:/bin', '--setenv', 'HOME', '/tmp', '--setenv', 'TMPDIR', '/tmp']
    for item in ['/lib64', '/etc/ld.so.cache', '/etc/alternatives']:
        if Path(item).exists():
            command.extend(['--ro-bind', item, item])
    return command


def exact(stream, size):
    chunks = bytearray()
    while len(chunks) < size:
        part = stream.recv(size - len(chunks))
        if not part:
            raise EOFError('incomplete native frame')
        chunks.extend(part)
    return bytes(chunks)


class NativeCellTest(unittest.TestCase):
    def setUp(self):
        parent = ROOT / 'build/native-tests'
        parent.mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=parent)
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name)
        (self.path / 'api').mkdir()
        shutil.copyfile(ROOT / 'native/runtime.zi', self.path / 'api/runtime.zi')

    def compile(self, source):
        (self.path / 'main.zi').write_text(source)
        command = base()
        for part in ['build/bin', 'std', 'include']:
            command.extend(['--ro-bind', str(ZIRAN / part), '/toolchain/' + part])
        command += ['--bind', str(self.path), '/cell',
                            '--chdir', '/cell', '--', '/toolchain/build/bin/ziran', 'build', '--target=c',
                            '--exe', '--entry', 'main:main', '--root', '/cell', '--module-path',
                            'xcoder=/cell/api', '-o', '/cell/out', '/cell/main.zi']
        result = subprocess.run(command, capture_output=True, text=True, timeout=60)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def run_cell(self, source, expected):
        self.compile(source)
        listener = socket.socket(socket.AF_UNIX)
        self.addCleanup(listener.close)
        listener.bind(str(self.path / 'rpc'))
        listener.listen()
        received = []
        errors = []

        def server():
            try:
                for expected_item in expected:
                    stream, _ = listener.accept()
                    with stream:
                        length, = struct.unpack('<I', exact(stream, 4))
                        self.assertLessEqual(length, 16777216)
                        item = json.loads(exact(stream, length))
                        received.append(item)
                        self.assertEqual(item['op'], expected_item)
                        response = json.dumps({'ok': True, 'value': 42}).encode()
                        # Exercise partial reply reads at both header and payload.
                        wire = struct.pack('<I', len(response)) + response
                        for byte in wire:
                            stream.sendall(bytes([byte]))
            except BaseException as error:
                errors.append(error)

        thread = threading.Thread(target=server, daemon=True)
        thread.start()
        command = base() + ['--ro-bind', str(self.path / 'out/main'), '/cell', '--bind',
                            str(self.path / 'rpc'), '/rpc', '--chdir', '/tmp', '--', '/cell']
        env = {key: value for key, value in os.environ.items() if key not in
               ['DISPLAY', 'WAYLAND_DISPLAY', 'XAUTHORITY', 'DBUS_SESSION_BUS_ADDRESS']}
        result = subprocess.run(command, env=env, capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        thread.join(timeout=5)
        self.assertFalse(thread.is_alive(), 'native callback server still waiting')
        if errors:
            raise errors[0]
        return received

    def test_real_tools_and_partial_frames(self):
        received = self.run_cell('''using X :: #import "xcoder/runtime"
main :: () -> s32 {
    reply := Call("fixture", "{\\"value\\":42}")
    defer Release(*reply)
    if !reply.ok { return 4 }
    Text(Body(*reply))
    return 0
}
''', ['call', 'text'])
        self.assertEqual(json.loads(received[0]['value']), {'value': 42})
        self.assertEqual(json.loads(received[1]['value']), {'ok': True, 'value': 42})

    def test_owner_files_and_other_cells_are_absent(self):
        received = self.run_cell('''using X :: #import "xcoder/runtime"
F :: #import "std/file_tree_linux"
main :: () -> s32 {
    if F.IsFile("/etc/passwd") || F.IsDirectory("/mnt/storage/Projects") || F.IsDirectory("/toolchain") { return 5 }
    Text("isolated")
    return 0
}
''', ['text'])
        self.assertEqual(received[0]['value'], 'isolated')

    def test_replies_are_freed_in_a_loop(self):
        source = '''using X :: #import "xcoder/runtime"
main :: () -> s32 {
    for i: 0..99 { reply := Call("fixture"); if !reply.ok { return 4 }; Release(*reply) }
    return 0
}
'''
        self.assertEqual(len(self.run_cell(source, ['call'] * 100)), 100)

    def test_batch_calls_release_each_reply(self):
        source = '''using X :: #import "xcoder/runtime"
main :: () -> s32 {
    calls: [4]BatchCall
    for i: 0..3 { calls[i].name = "fixture"; calls[i].input = "{}" }
    CallBatch(calls[:])
    for i: 0..3 { Text(Body(*calls[i].reply)); Release(*calls[i].reply) }
    return 0
}
'''
        messages = self.run_cell(source, ['call'] * 4 + ['text'] * 4)
        self.assertEqual([json.loads(item['value'])['value'] for item in messages[4:]], [42] * 4)


if __name__ == '__main__':
    unittest.main()
