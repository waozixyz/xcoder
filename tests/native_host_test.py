"""Offline end-to-end tests against the built host executable and wire protocol."""
import json
import os
from pathlib import Path
import queue
import struct
import subprocess
import tempfile
import threading
import unittest

ROOT = Path(__file__).resolve().parents[1]
HOST = Path(os.environ.get('XCODER_TEST_HOST', ROOT / 'build/bin/codex-code-mode-host'))


class Peer:
    def __init__(self, directory):
        env = {key: value for key, value in os.environ.items() if key not in
               ['DISPLAY', 'WAYLAND_DISPLAY', 'XAUTHORITY', 'DBUS_SESSION_BUS_ADDRESS']}
        env['XCODER_RUNTIME_DIR'] = str(directory / 'runtime')
        self.process = subprocess.Popen([str(HOST), '--listen', 'stdio://'], stdin=subprocess.PIPE,
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env)
        self.messages = queue.Queue()
        self.reader = threading.Thread(target=self.read, daemon=True)
        self.reader.start()

    def read_exact(self, length):
        data = bytearray()
        while len(data) < length:
            part = self.process.stdout.read(length - len(data))
            if not part:
                raise EOFError('host closed before completing frame')
            data.extend(part)
        return data

    def read(self):
        try:
            while True:
                length, = struct.unpack('<I', self.read_exact(4))
                if length > 67108864:
                    raise ValueError('host returned oversized frame')
                self.messages.put(json.loads(self.read_exact(length)))
        except BaseException as error:
            self.messages.put(error)

    def send(self, message):
        data = json.dumps(message).encode()
        self.process.stdin.write(struct.pack('<I', len(data)) + data)
        self.process.stdin.flush()

    def receive(self):
        while True:
            message = self.messages.get(timeout=60)
            if isinstance(message, BaseException):
                raise message
            # Lifecycle notifications can race the next operation response.
            if message['type'] not in ['cell/closed', 'delegate/cancel']:
                return message

    def operation(self, ident, method, **fields):
        self.send(dict(type='operation/request', id=ident,
                       request=dict(method=method, sessionId='native-host-test', **fields)))

    def close(self):
        if not self.process.stdin.closed:
            self.process.stdin.close()
        try:
            self.process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=10)
        self.reader.join(timeout=5)
        self.process.stdout.close()
        self.process.stderr.close()


class NativeHostTest(unittest.TestCase):
    def setUp(self):
        directory = ROOT / 'build/native-host-tests'
        directory.mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=directory)
        self.addCleanup(self.temp.cleanup)
        self.peer = Peer(Path(self.temp.name))
        self.addCleanup(self.peer.close)
        self.peer.send(dict(type='connection/hello', supportedVersions=[1], requiredCapabilities=[],
                            optionalCapabilities=['bounded-output-v1', 'yield-observation',
                                                  'session-cell-execution-resource-limits']))
        self.assertEqual(self.peer.receive()['type'], 'connection/ready')
        self.peer.operation(1, 'session/open', outputTruncation='server',
                            cellExecutionLimits=dict(maxHeapSizeBytes=268435456, maxYieldTimeMs=30000))
        self.assertEqual(self.peer.receive()['result']['value']['type'], 'session/ready')

    def execute(self, body, ident=2, tools=()):
        definitions = [dict(name=name, tool_name=dict(name=name, namespace=None),
                            description='native fixture', kind=kind, input_schema=None,
                            output_schema=None) for name, kind in tools]
        source = 'using X :: #import "xcoder/runtime"\nmain :: () -> s32 {\n' + body + '\nreturn 0\n}\n'
        self.peer.operation(ident, 'session/execute', request=dict(tool_call_id='native-host-call',
                            enabled_tools=definitions, source=source, yield_time_ms=30000,
                            max_output_tokens=10000))
        reply = self.peer.receive()
        self.assertEqual(reply['result']['status'], 'ok', reply)
        return reply['result']['value']['cellId']

    def response(self, ident, initial=False):
        while True:
            reply = self.peer.receive()
            if reply['type'] in ['cell/closed', 'delegate/cancel']:
                continue
            self.assertEqual(reply.get('id'), ident, reply)
            self.assertEqual(reply['result']['status'], 'ok', reply)
            self.assertEqual(reply['type'], 'execute/initialResponse' if initial else 'operation/response')
            return reply['result']['value']

    def test_actual_host_tool_round_trip_and_timing(self):
        self.execute(r'r := Call("fixture", "{\"value\":7}"); defer Release(*r); Text(Body(*r))',
                     tools=[('fixture', 'function')])
        callback = self.peer.receive()
        self.assertEqual(callback['type'], 'delegate/request', callback)
        self.assertEqual(callback['request']['invocation']['input'], {'value': 7})
        self.peer.send(dict(type='delegate/response', id=callback['id'],
                            result=dict(status='ok', value=dict(type='tool/result', result={'proof': 49}))))
        completed = self.response(2, initial=True)['Result']
        self.assertIsNone(completed['error_text'])
        self.assertEqual(json.loads(completed['content_items'][0]['text'])['value'], {'proof': 49})
        self.assertGreaterEqual(completed['code_mode_host_duration_ns'], 0)

    def test_pending_parallel_tools_do_not_block_a_later_cell(self):
        self.execute('''calls: [2]BatchCall
for i: 0..1 { calls[i].name = "fixture"; calls[i].input = "{}" }
CallBatch(calls[:])
for i: 0..1 { Text(Body(*calls[i].reply)); Release(*calls[i].reply) }
''',
                     tools=[('fixture', 'function')])
        callbacks = [self.peer.receive(), self.peer.receive()]
        self.assertTrue(all(item['type'] == 'delegate/request' for item in callbacks), callbacks)
        self.assertEqual(len({item['id'] for item in callbacks}), 2)
        # The callbacks remain outstanding until the independent cell completes.
        self.execute('Text("independent cell completed")', ident=3)
        healthy = self.response(3, initial=True)['Result']
        self.assertIsNone(healthy['error_text'])
        self.assertEqual(healthy['content_items'], [dict(type='input_text', text='independent cell completed')])
        for callback in reversed(callbacks):
            self.peer.send(dict(type='delegate/response', id=callback['id'],
                                result=dict(status='ok', value=dict(type='tool/result', result=callback['id']))))
        completed = self.response(2, initial=True)['Result']
        self.assertIsNone(completed['error_text'])
        values = {json.loads(item['text'])['value'] for item in completed['content_items']}
        self.assertEqual(values, {item['id'] for item in callbacks})

    def test_callback_transport_failure_returns_and_next_cell_is_healthy(self):
        self.execute(r'r := Call("fixture", "{}"); defer Release(*r); if !r.ok { return 4 }; Text(Body(*r))',
                     tools=[('fixture', 'function')])
        callback = self.peer.receive()
        self.assertEqual(callback['type'], 'delegate/request')
        self.peer.send(dict(type='delegate/response', id=callback['id'],
                            result=dict(status='error', message='fixture connection failed')))
        completed = self.response(2, initial=True)['Result']
        self.assertIsNone(completed['error_text'])
        error = json.loads(completed['content_items'][0]['text'])
        self.assertFalse(error['ok'])
        self.assertIn('fixture connection failed', error['error'])
        self.execute('Text("recovered")', ident=3)
        self.assertIsNone(self.response(3, initial=True)['Result']['error_text'])

    def test_yield_wait_and_consumed_cell(self):
        cell = self.execute('Text("before"); Yield(); Text("after")')
        first = self.response(2, initial=True)['Yielded']
        self.assertEqual(first['content_items'], [dict(type='input_text', text='before')])
        self.peer.operation(3, 'session/wait', request=dict(cell_id=cell, yield_time_ms=30000))
        completed = self.response(3)['outcome']['LiveCell']['Result']
        self.assertIsNone(completed['error_text'])
        self.assertEqual(completed['content_items'], [dict(type='input_text', text='after')])
        self.peer.operation(4, 'session/wait', request=dict(cell_id=cell, yield_time_ms=30000))
        self.assertIn('MissingCell', self.response(4)['outcome'])

    def test_freeform_input_preserves_literal_bytes(self):
        self.execute('r := Call("patch", "line one\\nline two"); defer Release(*r); Text(Body(*r))',
                     tools=[('patch', 'freeform')])
        callback = self.peer.receive()
        self.assertEqual(callback['request']['invocation']['input'], 'line one\nline two')
        self.peer.send(dict(type='delegate/response', id=callback['id'],
                            result=dict(status='ok', value=dict(type='tool/result', result='done'))))
        completed = self.response(2, initial=True)['Result']
        self.assertIsNone(completed['error_text'])
        self.assertEqual(json.loads(completed['content_items'][0]['text'])['value'], 'done')

    def test_rejects_javascript_then_accepts_native_cell(self):
        self.peer.operation(2, 'session/execute', request=dict(tool_call_id='invalid', enabled_tools=[],
                            source='text("old JavaScript");', yield_time_ms=30000, max_output_tokens=1000))
        started = self.peer.receive()
        self.assertEqual(started['result']['status'], 'ok')
        failed = self.response(2, initial=True)['Result']
        self.assertIn('compilation failed', failed['error_text'])
        self.execute('Text("healthy")', ident=3)
        completed = self.response(3, initial=True)['Result']
        self.assertIsNone(completed['error_text'])


if __name__ == '__main__':
    unittest.main()
