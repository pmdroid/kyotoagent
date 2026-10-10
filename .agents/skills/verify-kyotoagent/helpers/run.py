import http.client
import json
import math
import os
from pathlib import Path
import re
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request


HELPER = Path(__file__).with_name('verify.sh').resolve()


class Interrupted(Exception):
    def __init__(self, code):
        self.code = code


def interrupt(signum, frame):
    raise Interrupted(124 if signum == signal.SIGALRM else 128 + signum)


def v1(path, route, body=None):
    connection = http.client.HTTPConnection('kyotoagent', timeout=2)
    connection.sock = socket.socket(socket.AF_UNIX)
    connection.sock.settimeout(2)
    try:
        connection.sock.connect(str(path))
        connection.request('POST' if body is not None else 'GET', route, json.dumps(body) if body is not None else None, {'Host': 'kyotoagent', 'Content-Type': 'application/json'})
        response = connection.getresponse()
        data = response.read()
        if response.status >= 400:
            raise RuntimeError(f'{route} returned HTTP {response.status}')
        return json.loads(data)
    finally:
        connection.close()


def start_identity(pid):
    return Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[19]


def owned_server(meta):
    pid = meta['pid']
    values = Path(f'/proc/{pid}/environ').read_bytes().split(b'\0')
    expected = [f'HOME={meta["home"]}', f'KYOTOAGENT_ROOT={meta["kyotoagentRoot"]}']
    if not all(value.encode() in values for value in expected):
        raise RuntimeError('refusing to stop a server with a different HOME/root')
    if start_identity(pid) != meta['startIdentity'] or Path(f'/proc/{pid}/exe').resolve() != Path(meta['bin']):
        raise RuntimeError('refusing to stop a different server process')
    if Path(meta['socket']) != Path(meta['kyotoagentRoot']) / 'kyotoagent.sock':
        raise RuntimeError('refusing an unrelated socket')
    if Path(meta['socket']).exists() and not Path(meta['socket']).is_socket():
        raise RuntimeError('server socket was replaced')


def context():
    home = Path(os.environ['HOME'])
    root = Path(os.environ['KYOTOAGENT_ROOT'])
    if home.parent != Path('/tmp') or not home.name.startswith('verify-kyotoagent-') or home.resolve() != home or root != home / '.kyotoagent' or root.resolve() != root:
        raise RuntimeError('expected a disposable HOME and matching KYOTOAGENT_ROOT')
    meta = json.loads((home / 'instance.json').read_text())
    if meta['home'] != str(home) or meta['kyotoagentRoot'] != str(root):
        raise RuntimeError('instance does not belong to this HOME/root')
    owned_server(meta)
    return meta


def catalog(meta, key):
    headers = {'Authorization': 'Bearer ' + key} if key else {}
    request = urllib.request.Request(meta['baseUrl'] + '/models', headers=headers)
    with urllib.request.urlopen(request, timeout=5) as response:
        data = json.load(response)
    (Path(meta['evidence']) / 'models.json').write_text(json.dumps(data))
    row = next((row for row in data.get('data', []) if row.get('id') == meta['model']), None)
    if row is None:
        raise RuntimeError('selected model is not advertised by the provider')
    return row


def stop(process):
    if process is None:
        return
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=2)
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def sanitize(evidence, secret):
    for path in evidence.rglob('*'):
        if path.is_symlink() or (not path.is_file() and not path.is_dir()):
            path.unlink()
        elif path.is_file() and secret:
            path.write_bytes(path.read_bytes().replace(secret.encode(), b'[redacted]'))


def run(args):
    base = os.environ.get('KYOTOAGENT_E2E_BASE_URL', '').rstrip('/')
    model = os.environ.get('KYOTOAGENT_E2E_MODEL', '')
    parsed = urllib.parse.urlsplit(base)
    if parsed.scheme not in ('http', 'https') or not parsed.hostname or parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise ValueError('set KYOTOAGENT_E2E_BASE_URL to a provider URL without credentials/query/fragment')
    if not model.strip():
        raise ValueError('set KYOTOAGENT_E2E_MODEL to the exact model ID')
    name = os.environ.get('KYOTOAGENT_E2E_API_KEY_ENV', '')
    if name and (not re.fullmatch(r'[A-Za-z_][A-Za-z0-9_]*', name) or not os.environ.get(name)):
        raise ValueError('the named provider credential environment variable is missing or invalid')
    key = os.environ.get(name, '')
    timeout = float(os.environ.get('KYOTOAGENT_E2E_TIMEOUT', '240'))
    if not math.isfinite(timeout) or timeout <= 0:
        raise ValueError('KYOTOAGENT_E2E_TIMEOUT must be a positive finite number')
    binary = Path(os.environ.get('BIN', str(HELPER.parents[4] / 'target/debug/kyotoagent'))).resolve(strict=True)
    if not os.access(binary, os.X_OK):
        raise ValueError('BIN must name an executable local build')
    if args and args[0] == '--':
        command = args[1:]
        if not command:
            raise ValueError('supply a command after --')
    elif not args or (len(args) == 2 and args[0] == '--text'):
        command = ['bash', str(HELPER), 'ask', *args]
    else:
        raise ValueError('usage: verify.sh run [--text TEXT | -- COMMAND ARGS...]')
    home = Path(tempfile.mkdtemp(prefix='verify-kyotoagent-', dir='/tmp'))
    root = home / '.kyotoagent'
    if home.resolve() != home or root.resolve() != root:
        raise RuntimeError('temporary HOME/root did not resolve inside the new run')
    evidence = home / 'evidence'
    workspace = home / 'workspace'
    for directory in (root, evidence, workspace):
        directory.mkdir(mode=0o700)
    sock = root / 'kyotoagent.sock'
    env = {'PATH': '/usr/bin:/bin', 'HOME': str(home), 'KYOTOAGENT_ROOT': str(root), 'XDG_CONFIG_HOME': str(home / '.config'), 'XDG_CACHE_HOME': str(home / '.cache'), 'XDG_DATA_HOME': str(home / '.local/share'), 'WORKSPACE': str(workspace), 'EVIDENCE': str(evidence), 'SOCKET': str(sock), 'VERIFY_HELPER': str(HELPER), 'KYOTOAGENT_VERIFY_API_KEY': key}
    config = f'base_url = {json.dumps(base)}\nmodel = {json.dumps(model)}\ntitle_model = {json.dumps(model)}\nmax_steps = 8\n'
    if key:
        config += 'api_key_env = "KYOTOAGENT_VERIFY_API_KEY"\n'
    (root / 'config.toml').write_text(config)
    (root / 'config.toml').chmod(0o600)
    meta = {'home': str(home), 'kyotoagentRoot': str(root), 'socket': str(sock), 'workspace': str(workspace), 'evidence': str(evidence), 'bin': str(binary), 'baseUrl': base, 'model': model}
    server = driver = None
    status = 1
    error = None
    for sig in (signal.SIGINT, signal.SIGTERM, signal.SIGALRM):
        signal.signal(sig, interrupt)
    signal.setitimer(signal.ITIMER_REAL, timeout)
    try:
        with (evidence / 'serve.log').open('wb') as log:
            server = subprocess.Popen([str(binary), 'serve'], env=env, cwd=workspace, stdout=log, stderr=log, start_new_session=True)
        meta.update(pid=server.pid, startIdentity=start_identity(server.pid))
        (home / 'instance.json').write_text(json.dumps(meta))
        deadline = time.monotonic() + 10
        while True:
            if server.poll() is not None:
                raise RuntimeError('server exited before readiness; see serve.log')
            try:
                v1(sock, '/v1/sessions')
                break
            except (OSError, http.client.HTTPException):
                if time.monotonic() >= deadline:
                    raise RuntimeError('server did not become ready')
                time.sleep(.05)
        v1(sock, '/v1/https', {'listen': '127.0.0.1:0'})
        meta['address'] = v1(sock, '/v1/https')['addr']
        (home / 'instance.json').write_text(json.dumps(meta))
        owned_server(meta)
        print(json.dumps(meta), flush=True)
        catalog(meta, key)
        env['VERIFY_ADDRESS'] = meta['address']
        with (evidence / 'driver.log').open('wb') as log:
            driver = subprocess.Popen(command, env=env, cwd=workspace, stdout=log, stderr=log, start_new_session=True)
        status = driver.wait()
        if status < 0:
            status = 128 - status
    except Interrupted as failure:
        status = failure.code
        error = 'verification deadline exceeded' if status == 124 else 'verification interrupted'
    except Exception as failure:
        error = str(failure)
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        for sig in (signal.SIGINT, signal.SIGTERM):
            signal.signal(sig, signal.SIG_IGN)
        try:
            stop(driver)
            if server is not None and server.poll() is None:
                owned_server(meta)
            stop(server)
        except Exception as failure:
            status = 1
            error = f'cleanup refused: {failure}'
        else:
            for path in home.iterdir():
                if path == evidence:
                    continue
                if path.is_dir() and not path.is_symlink():
                    shutil.rmtree(path)
                else:
                    path.unlink()
        (evidence / 'instance.json').write_text(json.dumps(meta))
        (evidence / 'outcome.json').write_text(json.dumps({'exitCode': status, 'error': error}))
        sanitize(evidence, key)
        log = evidence / 'driver.log'
        if log.exists():
            print(log.read_text(errors='replace'), end='')
        if error:
            print(error.replace(key, '[redacted]') if key else error, file=sys.stderr)
        print(f'evidence: {evidence}', flush=True)
    return status


if __name__ == '__main__':
    try:
        if sys.argv[1:2] == ['doctor']:
            meta = context()
            row = catalog(meta, os.environ.get('KYOTOAGENT_VERIFY_API_KEY', ''))
            v1(meta['socket'], '/v1/sessions')
            print(json.dumps(row))
            sys.exit(0)
        sys.exit(run(sys.argv[1:]))
    except Exception as failure:
        print(str(failure), file=sys.stderr)
        sys.exit(1)
