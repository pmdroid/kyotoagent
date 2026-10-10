import concurrent.futures
import http.server
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest


HELPER = Path(__file__).with_name('verify.sh').resolve()
BINARY = Path(sys.argv.pop(1)).resolve()


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def reply(self, value, status=200):
        data = json.dumps(value).encode()
        self.send_response(status)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        self.server.calls.append((self.path, self.headers.get('Authorization'), None))
        self.reply({'data': [{'id': 'expensive'}, {'id': 'cheap', 'context_length': 32768}]})

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        self.server.calls.append((self.path, self.headers.get('Authorization'), body))
        if self.server.fail:
            self.reply({'error': 'provider unavailable'}, 401)
            return
        message = {'role': 'assistant', 'content': 'pong'}
        if body.get('tools'):
            message = {'role': 'assistant', 'tool_calls': [{'id': 'finish-1', 'type': 'function', 'function': {'name': 'finish', 'arguments': json.dumps({'text': 'pong'})}}]}
        self.reply({'choices': [{'message': message, 'finish_reason': 'stop'}]})


class Runs(unittest.TestCase):
    def setUp(self):
        self.home = tempfile.TemporaryDirectory(prefix='kyoto-verify-test-')
        self.root = Path(self.home.name)
        self.provider = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
        self.provider.calls = []
        self.provider.fail = False
        self.thread = threading.Thread(target=self.provider.serve_forever)
        self.thread.start()
        self.env = {'PATH': '/usr/bin:/bin', 'HOME': str(self.root), 'KYOTOAGENT_ROOT': str(self.root / '.kyotoagent'), 'BIN': str(BINARY), 'KYOTOAGENT_E2E_BASE_URL': f'http://127.0.0.1:{self.provider.server_port}/v1', 'KYOTOAGENT_E2E_MODEL': 'cheap', 'KYOTOAGENT_E2E_API_KEY_ENV': 'TEST_PROVIDER_KEY', 'TEST_PROVIDER_KEY': 'secret-for-test', 'KYOTOAGENT_URL': 'https://127.0.0.1:1', 'KYOTOAGENT_E2E_TIMEOUT': '20'}
        (self.root / '.kyotoagent').mkdir()
        self.sentinel = self.root / '.kyotoagent/config.toml'
        self.sentinel.write_text('leave me alone')
        self.runs = []

    def tearDown(self):
        self.assertEqual(self.sentinel.read_text(), 'leave me alone')
        self.provider.shutdown()
        self.provider.server_close()
        self.thread.join()
        for home in self.runs:
            import shutil
            shutil.rmtree(home)
        self.home.cleanup()

    def run_helper(self, *args, env=None):
        result = subprocess.run(['bash', str(HELPER), 'run', *args], env=env or self.env, capture_output=True, text=True, timeout=30)
        return result

    def evidence(self, output):
        rows = [json.loads(line) for line in output.splitlines() if line.startswith('{')]
        self.assertTrue(rows, output)
        meta = next(row for row in rows if 'home' in row)
        home = Path(meta['home'])
        self.runs.append(home)
        self.assertFalse(Path(meta['kyotoagentRoot']).exists())
        self.assertFalse(Path(meta['workspace']).exists())
        self.assertFalse(Path('/proc', str(meta['pid'])).exists())
        return meta, Path(meta['evidence'])

    def test_smoke_uses_explicit_model_and_preserves_result(self):
        result = self.run_helper()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        meta, evidence = self.evidence(result.stdout)
        self.assertTrue(meta['address'].startswith('127.0.0.1:'))
        self.assertNotEqual(meta['address'], '127.0.0.1:0')
        self.assertEqual(json.loads((evidence / 'outcome.json').read_text())['exitCode'], 0)
        self.assertTrue((evidence / 'view-idle.json').exists())
        self.assertTrue((evidence / 'request.json').exists())
        chats = [body for _, _, body in self.provider.calls if body]
        self.assertTrue(chats)
        self.assertEqual({body['model'] for body in chats}, {'cheap'})
        self.assertTrue(all(auth == 'Bearer secret-for-test' for _, auth, _ in self.provider.calls))
        self.assertNotIn('secret-for-test', ''.join(path.read_text() for path in evidence.iterdir() if path.is_file()))

    def test_concurrent_runs_and_failure_leave_existing_run_alive(self):
        driver = 'test -z "${KYOTOAGENT_URL:-}"; sleep 2; curl -fsS --unix-socket "$SOCKET" -H "Host: kyotoagent" http://kyotoagent/v1/sessions'
        with concurrent.futures.ThreadPoolExecutor() as pool:
            existing = pool.submit(self.run_helper, '--', 'sh', '-ec', driver)
            failed = self.run_helper('--', 'sh', '-c', 'exit 7')
            healthy = existing.result()
        self.assertEqual(failed.returncode, 7, failed.stdout + failed.stderr)
        self.assertEqual(healthy.returncode, 0, healthy.stdout + healthy.stderr)
        first, _ = self.evidence(failed.stdout)
        second, _ = self.evidence(healthy.stdout)
        for key in ['home', 'socket', 'address', 'workspace']:
            self.assertNotEqual(first[key], second[key])

    def test_timeout_cleans_driver_and_server(self):
        result = self.run_helper('--', 'sh', '-c', 'sleep 30 & child=$!; echo "$child" > "$EVIDENCE/child.pid"; wait "$child"', env=dict(self.env, KYOTOAGENT_E2E_TIMEOUT='2'))
        self.assertEqual(result.returncode, 124, result.stdout + result.stderr)
        _, evidence = self.evidence(result.stdout)
        pid = (evidence / 'child.pid').read_text().strip()
        stat = Path('/proc', pid, 'stat')
        self.assertTrue(not stat.exists() or stat.read_text().rsplit(')', 1)[1].split()[0] == 'Z')

    def test_interrupt_cleans_driver_and_server(self):
        process = subprocess.Popen(['bash', str(HELPER), 'run', '--', 'sleep', '30'], env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            first = process.stdout.readline()
            self.assertIn('home', first)
            process.send_signal(signal.SIGINT)
            out, err = process.communicate(timeout=10)
            self.assertEqual(process.returncode, 130, first + out + err)
            self.evidence(first + out)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait()

    def test_provider_failure_is_not_a_successful_result(self):
        self.provider.fail = True
        result = self.run_helper()
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.evidence(result.stdout)
        self.assertEqual({body['model'] for _, _, body in self.provider.calls if body}, {'cheap'})

    def test_missing_key_and_model_fail_before_launch(self):
        for missing in ['TEST_PROVIDER_KEY', 'KYOTOAGENT_E2E_MODEL']:
            env = dict(self.env)
            del env[missing]
            result = self.run_helper(env=env)
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn('"pid"', result.stdout)
        self.assertFalse(self.provider.calls)

    def test_unknown_model_fails_without_fallback(self):
        result = self.run_helper(env=dict(self.env, KYOTOAGENT_E2E_MODEL='absent'))
        self.assertNotEqual(result.returncode, 0)
        self.evidence(result.stdout)
        self.assertFalse(any(body for _, _, body in self.provider.calls))

    def test_ownership_check_refuses_an_unrelated_pid(self):
        driver = 'python3 -c \'import json,os; from pathlib import Path; p=Path(os.environ["HOME"])/"instance.json"; d=json.loads(p.read_text()); d["pid"]=os.getpid(); p.write_text(json.dumps(d))\'; "$VERIFY_HELPER" doctor'
        result = self.run_helper('--', 'sh', '-ec', driver)
        self.assertNotEqual(result.returncode, 0)
        self.evidence(result.stdout)

    def test_non_run_commands_refuse_operator_context(self):
        result = subprocess.run(['bash', str(HELPER), 'doctor'], env=self.env, capture_output=True, text=True, timeout=5)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.provider.calls)

    def test_driver_output_is_redacted(self):
        result = self.run_helper('--', 'sh', '-c', 'echo "$KYOTOAGENT_VERIFY_API_KEY"; echo "$KYOTOAGENT_VERIFY_API_KEY" > "$EVIDENCE/custom.txt"')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        _, evidence = self.evidence(result.stdout)
        self.assertNotIn('secret-for-test', result.stdout + result.stderr)
        self.assertEqual((evidence / 'custom.txt').read_text().strip(), '[redacted]')


unittest.main()
