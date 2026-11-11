"""Run the real CLI, native host and real shell tool against an offline model fixture."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import subprocess
import tempfile
import threading
import unittest

ROOT = Path(__file__).resolve().parents[1]
CLI = Path(os.environ.get('XCODER_TEST_CLI', ROOT / 'build/bin/xcoder'))
MARKER = 'xcoder-native-tool-ok'


class NativeCliTest(unittest.TestCase):
    def test_agent_loop_uses_native_cell_and_real_approved_tool_path(self):
        requests = []
        failures = []
        memory = {}
        process_ready = threading.Event()
        process_state = {}
        arguments = json.dumps(dict(cmd='printf ' + MARKER, max_output_tokens=1000))
        source = ('using X :: #import "xcoder/runtime"\nmain :: () -> s32 { '
                  'r := Call("exec_command", ' + json.dumps(arguments) + '); '
                  'defer Release(*r); Text(Body(*r)); return 0 }')

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                self.send_error(404)

            def do_POST(self):
                try:
                    body = self.rfile.read(int(self.headers.get('Content-Length', 0)))
                    requests.append(json.loads(body))
                    count = len(requests)
                    if count == 2:
                        process_ready.wait()
                        pending = [process_state['pid']]
                        while pending:
                            pid = pending.pop()
                            try:
                                executable = Path(os.readlink(f'/proc/{pid}/exe')).name
                                if executable.startswith('codex-code-mode-host'):
                                    raise AssertionError('ordinary native tool calls started a separate tool host')
                                for task in Path(f'/proc/{pid}/task').iterdir():
                                    pending.extend(int(child) for child in (task / 'children').read_text().split())
                            except (FileNotFoundError, PermissionError):
                                continue
                        for filename, fields in [('status', ['VmRSS', 'VmHWM', 'VmSwap']),
                                                 ('smaps_rollup', ['Rss', 'Pss'])]:
                            try:
                                for line in Path(f'/proc/{process_state["pid"]}/{filename}').read_text().splitlines():
                                    parts = line.split()
                                    if parts[0].rstrip(':') in fields:
                                        memory[parts[0].rstrip(':') + '_KiB'] = int(parts[1])
                            except (FileNotFoundError, PermissionError):
                                pass
                    if count > 2:
                        raise AssertionError('fixture received an unexpected additional model request')
                    item = dict(type='custom_tool_call', call_id='native-e2e', name='exec', input=source)
                    if count == 2:
                        item = dict(type='message', role='assistant', id='native-finished',
                                    content=[dict(type='output_text', text='native fixture complete')])
                    events = [dict(type='response.created', response=dict(id='fixture-' + str(count))),
                              dict(type='response.output_item.done', item=item),
                              dict(type='response.completed', response=dict(id='fixture-' + str(count),
                                   usage=dict(input_tokens=0, output_tokens=0, total_tokens=0)))]
                    payload = ''.join('event: ' + event['type'] + '\ndata: ' + json.dumps(event) + '\n\n'
                                      for event in events).encode()
                    self.send_response(200)
                    self.send_header('Content-Type', 'text/event-stream')
                    self.send_header('Content-Length', str(len(payload)))
                    self.end_headers()
                    self.wfile.write(payload)
                except BaseException as error:
                    failures.append(error)
                    self.send_error(500)

        parent = ROOT / 'build/native-cli-tests'
        parent.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=parent) as temporary:
            directory = Path(temporary)
            home = directory / 'home'
            config = home / '.codex'
            config.mkdir(parents=True)
            server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
            server.daemon_threads = True
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            port = server.server_address[1]
            (config / 'config.toml').write_text(f'''model = "gpt-6.1-sol"
model_provider = "fixture"
check_for_update_on_startup = false
cli_auth_credentials_store = "file"
[features]
code_mode = true
code_mode_only = true
enable_request_compression = false
[model_providers.fixture]
name = "Offline fixture"
base_url = "http://127.0.0.1:{port}/v1"
wire_api = "responses"
requires_openai_auth = false
supports_websockets = false
''')
            env = {key: value for key, value in os.environ.items() if key in ['PATH', 'LANG', 'LC_ALL', 'XCODER_ZIRAN_TOOLCHAIN']}
            env.update(HOME=str(home), CODEX_HOME=str(config), XDG_CONFIG_HOME=str(home / '.config'),
                       XDG_CACHE_HOME=str(directory / 'cache'), XCODER_RUNTIME_DIR=str(directory / 'runtime'),
                       NO_PROXY='127.0.0.1,localhost', RUST_LOG='warn',
                       HARMONY_SOURCE_SESSION='xcoder-offline-local-test', HARMONY_MODEL_INPUT_DISABLE='1')
            try:
                command = [str(CLI), 'exec', '--ephemeral', '--skip-git-repo-check',
                           '--sandbox', 'read-only', '--json', 'Run the native local fixture.']
                process = subprocess.Popen(command, cwd=directory, env=env, stdout=subprocess.PIPE,
                                           stderr=subprocess.PIPE, text=True)
                process_state['pid'] = process.pid
                process_ready.set()
                try:
                    stdout, stderr = process.communicate(timeout=180)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.communicate()
                    raise
                result = subprocess.CompletedProcess(command, process.returncode, stdout, stderr)
            finally:
                server.shutdown()
                server.server_close()
                thread.join(timeout=5)
            if failures:
                raise failures[0]
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertEqual(len(requests), 2, result.stdout + result.stderr)
            request_record = ROOT / 'build/native-cli-request.json'
            request_record.write_text(json.dumps(requests[0], indent=2) + '\n')
            request_record.chmod(0o600)
            def exec_definitions(definitions):
                for definition in definitions:
                    if definition.get('name') in ['exec', 'functions.exec']:
                        yield definition
                    yield from exec_definitions(definition.get('tools', []))
            definitions = list(requests[0].get('tools', []))
            # The current Responses transport places tool configuration among
            # input items; older transports used a top-level tools array.
            for item in requests[0].get('input', []):
                definitions.extend(item.get('tools', []))
            advertised = list(exec_definitions(definitions))
            self.assertEqual(len(advertised), 1, json.dumps(definitions))
            self.assertIn('Run native Ziran code', advertised[0]['description'])
            outputs = [item for item in requests[-1]['input'] if item.get('type') == 'custom_tool_call_output'
                       and item.get('call_id') == 'native-e2e']
            self.assertEqual(len(outputs), 1, json.dumps(requests[-1]))
            output = json.dumps(outputs[0]['output'])
            self.assertIn(MARKER, output, result.stdout + result.stderr + output)
            self.assertNotIn('unknown or ambiguous enabled tool', output)
            self.assertNotIn('compilation failed', output)
            self.assertIn('native fixture complete', result.stdout)
            (ROOT / 'build/native-cli-metrics.json').write_text(json.dumps(
                dict(fixture='offline native tool loop', executable=str(CLI), separate_tool_hosts=0, **memory), indent=2) + '\n')


if __name__ == '__main__':
    unittest.main()
