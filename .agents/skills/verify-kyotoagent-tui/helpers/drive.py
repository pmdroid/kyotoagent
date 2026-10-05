import fcntl
import codecs
import json
import os
from pathlib import Path
import pty
import select
import socket
import ssl
import struct
import subprocess
import termios
import time
import tomllib
import urllib.request
import pyte
import argparse
import tempfile
import re
import shutil
from PIL import Image, ImageDraw, ImageFont

parser = argparse.ArgumentParser()
parser.add_argument('--binary', type=Path, required=True)
parser.add_argument('--base-url', required=True)
parser.add_argument('--evidence', type=Path, required=True)
parser.add_argument('--doctor', action='store_true')
args = parser.parse_args()
binary = str(args.binary.resolve(strict=True))
base = args.base_url.rstrip('/')
subprocess.run([binary, '--help'], check=True, capture_output=True, timeout=5)
with urllib.request.urlopen(base + '/models', timeout=5) as response:
    assert response.status == 200
    catalog = json.load(response)
models = [row['id'] for row in catalog['data']]
assert models, 'Model server has no advertised models'
font = ImageFont.truetype('DejaVuSansMono.ttf', 15)
if args.doctor:
    print(json.dumps({'binary': binary, 'base_url': base, 'models': models}))
    raise SystemExit(0)
evidence = args.evidence.resolve()
evidence.mkdir(parents=True, exist_ok=False)
(evidence / 'catalog.json').write_text(json.dumps(catalog, indent=2))
root = Path(tempfile.mkdtemp(prefix='ka-tui-'))
actions = []
client_home = root / 'client'
client_home.mkdir(exist_ok=True)
env = dict(os.environ, HOME=str(client_home), KYOTOAGENT_ROOT=str(client_home / '.kyotoagent'), TERM='xterm-256color')
env.pop('KYOTOAGENT_URL', None)
processes = []
logs = []
master = None

def cli(args, target_env=env):
    result = subprocess.run([binary] + args, env=target_env, capture_output=True, timeout=12)
    if result.returncode:
        raise RuntimeError(result.stderr.decode())
    return result.stdout.decode()

def start_remote(name):
    home = root / name
    target = home / '.kyotoagent'
    certs = target / 'certs'
    certs.mkdir(parents=True, exist_ok=True)
    subprocess.run(['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1', '-subj', '/CN=localhost', '-keyout', str(certs / 'server.key'), '-out', str(certs / 'server.crt')], check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    with socket.socket() as reservation:
        reservation.bind(('127.0.0.1', 0))
        port = reservation.getsockname()[1]
    (target / 'config.toml').write_text(f'base_url = "{base}"\nmodel = "unavailable-configured-model"\nlisten = "127.0.0.1:{port}"\n[providers.unconfigured-router]\nbase_url = "https://openrouter.ai/api/v1"\nmodel = "openai/gpt-4o"\n')
    server_env = dict(env, HOME=str(home), KYOTOAGENT_ROOT=str(target))
    log = open(root / (name + '.log'), 'wb')
    logs.append(log)
    server = subprocess.Popen([binary, 'serve'], env=server_env, stdout=log, stderr=log)
    processes.append(server)
    deadline = time.monotonic() + 8
    while not (target / 'kyotoagent.sock').exists():
        if server.poll() is not None or time.monotonic() > deadline:
            raise RuntimeError('server did not start')
        time.sleep(.05)
    uri = cli(['pair', f'127.0.0.1:{port}'], server_env).splitlines()[-1]
    cli(['pair', uri])
    saved = tomllib.loads((client_home / '.kyotoagent/config.toml').read_text())
    credential = saved['servers'][f'127.0.0.1:{port}']
    assert credential != uri
    uri = credential
    token = uri.split('token=', 1)[1]
    workspace = root / ('work-' + name)
    workspace.mkdir(exist_ok=True)
    project = urllib.request.Request(f'https://127.0.0.1:{port}/v1/projects', data=json.dumps({'id': name, 'name': name, 'path': str(workspace)}).encode(), headers={'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json'})
    with urllib.request.urlopen(project, context=ssl._create_unverified_context(), timeout=8) as reply:
        assert reply.status == 201
    request = urllib.request.Request(f'https://127.0.0.1:{port}/v1/sessions', data=json.dumps({'workspace': str(workspace)}).encode(), headers={'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, context=ssl._create_unverified_context(), timeout=8) as reply:
        assert reply.status == 201
    return uri, f'127.0.0.1:{port}'

try:
    first_uri, first = start_remote('remote-a')
    second_uri, second = start_remote('remote-b')
    cli(['pair', first_uri])
    cfg = client_home / '.kyotoagent/config.toml'
    cfg.write_text(f'base_url = "{base}"\nmodel = "{models[0]}"\n' + cfg.read_text())
    local_log = open(root / 'local.log', 'wb')
    logs.append(local_log)
    local_server = subprocess.Popen([binary, 'serve'], env=env, stdout=local_log, stderr=local_log)
    processes.append(local_server)
    deadline = time.monotonic() + 8
    while not (client_home / '.kyotoagent/kyotoagent.sock').exists():
        assert local_server.poll() is None and time.monotonic() < deadline
        time.sleep(.05)
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 120, 0, 0))
    tui = subprocess.Popen([binary], env=env, stdin=slave, stdout=slave, stderr=slave, start_new_session=True, cwd=client_home)
    processes.append(tui)
    os.close(slave)
    screen = pyte.Screen(120, 30)
    stream = pyte.Stream(screen)
    decoder = codecs.getincrementaldecoder('utf-8')()
    def pump(seconds=.7):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            ready, _, _ = select.select([master], [], [], .1)
            if ready:
                chunk = os.read(master, 65536)
                transcript.write(chunk)
                stream.feed(decoder.decode(chunk))
        return '\n'.join(screen.display)
    transcript = open(evidence / 'terminal.ansi', 'wb')
    logs.append(transcript)
    def capture(name):
        frame = pump()
        assert 'token=' not in frame
        (evidence / (name + '.txt')).write_text(frame)
        image = Image.new('RGB', (120 * 10 + 24, 30 * 21 + 24), '#101218')
        draw = ImageDraw.Draw(image)
        for line, text in enumerate(screen.display):
            draw.text((12, 12 + line * 21), text, font=font, fill='#d6dae4')
        image.save(evidence / (name + '.png'))
        return frame
    def send(value):
        actions.append({'input': value.decode('utf-8', errors='replace'), 'time': time.monotonic()})
        os.write(master, value)
        return pump()
    def open_session(name):
        frame = pump()
        row = next(index + 1 for index, line in enumerate(frame.splitlines()) if name in line and index > 1)
        send(f'\x1b[<0;8;{row}M'.encode())
        send(f'\x1b[<0;8;{row}m'.encode())
    def choose_project(name):
        frame = pump()
        selected = re.search(r'(\d+)\s+' + re.escape(name), frame)
        assert selected, frame
        send(selected.group(1).encode())
    names = sorted([first, second])
    def switch(target):
        send(b'\x0b')
        send(b'Server')
        send(b'\r')
        frame = capture('server-picker-' + (target or 'local').replace(':', '-'))
        assert 'Which server?' in frame, frame
        assert 'token=' not in frame
        index = 1 if target is None else names.index(target) + 2
        send(str(index).encode())
    frame = capture('connected-a')
    assert 'work-remote-a' in frame.splitlines()[1]
    assert 'remote-a' in frame and 'remote-b' in frame, frame
    send(b'draft for a')
    open_session('work-remote-b')
    assert 'work-remote-b' in capture('connected-b').splitlines()[1]
    send(b'draft for b')
    switch(first)
    frame = capture('restored-a')
    assert 'work-remote-a' in frame.splitlines()[1] and 'draft for a' in frame
    send(b'\x0b')
    send(b'Open model')
    send(b'\r')
    frame = capture('remote-models')
    assert models[0] in frame, frame
    frame_lines = frame.splitlines()
    selected_row = next(i for i, line in enumerate(frame_lines) if models[0] in line and line.count('│') >= 3)
    picker_top = max(i for i in range(selected_row) if '╭' in frame_lines[i])
    picker_left = frame_lines[picker_top].rfind('╭')
    picker_bottom = next(i for i in range(selected_row + 1, len(frame_lines)) if frame_lines[i][picker_left] == '╰')
    picker_rows = frame_lines[picker_top + 1:picker_bottom]
    assert picker_rows and all('unavailable-configured-model' not in line for line in picker_rows), frame
    assert all('unconfigured-router' not in line for line in picker_rows), frame
    send(b'\x1b')
    send(b'\x14')
    frame = capture('remote-projects')
    assert 'This directory' not in frame and 'Add project' in frame, frame
    choose_project('Add project')
    send(b'TUI project')
    send(b'\r')
    new_workspace = root / 'work-tui-project'
    new_workspace.mkdir()
    send(str(new_workspace).encode())
    send(b'\r')
    send(b'\r')
    frame = capture('project-created')
    assert 'TUI project' in frame and 'This directory' not in frame, frame
    send(b'\x1b')
    send(b'\x0b')
    send(b'Projects')
    send(b'\r')
    send(b'3')
    send(b'\x15')
    send(b'Edited project')
    send(b'\r')
    send(b'\r')
    send(b'\r')
    frame = capture('project-edited')
    assert 'Edited project' in frame, frame
    import tomllib
    remote_config = tomllib.loads((root / 'remote-a/.kyotoagent/config.toml').read_text())
    assert remote_config['projects']['tui-project']['name'] == 'Edited project'
    assert remote_config['projects']['tui-project']['path'] == str(new_workspace)
    assert 'unconfigured-router' in remote_config['providers']
    send(b'\x1b')
    assert 'draft for a' in capture('project-draft-preserved')
    unconfigured = root / 'unconfigured'
    unconfigured.mkdir()
    rejected = urllib.request.Request(f'https://{first}/v1/sessions', data=json.dumps({'workspace': str(unconfigured)}).encode(), headers={'Authorization': 'Bearer ' + first_uri.split('token=', 1)[1], 'Content-Type': 'application/json'})
    try:
        urllib.request.urlopen(rejected, context=ssl._create_unverified_context(), timeout=8)
        raise AssertionError('Remote server accepted an unconfigured directory')
    except urllib.error.HTTPError as error:
        assert error.code == 400
    delete_repo = root / 'delete-repo'
    delete_repo.mkdir()
    subprocess.run(['git', 'init', '-b', 'main', str(delete_repo)], check=True, capture_output=True)
    (delete_repo / 'kept.txt').write_text('original repository')
    subprocess.run(['git', '-C', str(delete_repo), 'add', '.'], check=True, capture_output=True)
    subprocess.run(['git', '-C', str(delete_repo), '-c', 'user.name=Verify', '-c', 'user.email=verify@example.test', 'commit', '-m', 'Initial'], check=True, capture_output=True)
    def remote_request(method, path, body=None, uri=first_uri, host=first):
        request = urllib.request.Request(f'https://{host}' + path, method=method, data=None if body is None else json.dumps(body).encode(), headers={'Authorization': 'Bearer ' + uri.split('token=', 1)[1], 'Content-Type': 'application/json'})
        with urllib.request.urlopen(request, context=ssl._create_unverified_context(), timeout=8) as reply:
            data = reply.read()
            return json.loads(data) if data else None
    remote_request('POST', '/v1/projects', {'id': 'delete-repo', 'name': 'Delete repo', 'path': str(delete_repo)})
    def create_worktree():
        send(b'\x14')
        frame = capture('delete-project-picker')
        assert 'Delete repo' in frame
        choose_project('Delete repo')
        send(b'2')
        send(b'1')
        capture('worktree-created')
        return max(remote_request('GET', '/v1/sessions'), key=lambda row: row['createdAt'])
    worktree = create_worktree()
    checkout = Path(worktree['workspace'])
    (checkout / 'unsaved.txt').write_text('uncommitted work')
    send(b'\x17')
    frame = capture('dirty-workspace-delete')
    assert '[ ] Also delete workspace' in frame and 'unsaved.txt' in frame, frame
    send(b' ')
    send(b'\r')
    frame = capture('dirty-workspace-confirmation')
    assert 'Delete session and uncommitted files' in frame and checkout.exists(), frame
    send(b'\x1b[A')
    send(b'\r')
    capture('dirty-workspace-removed')
    assert not checkout.exists() and delete_repo.exists()
    assert all(row['id'] != worktree['id'] for row in remote_request('GET', '/v1/sessions'))
    worktree = create_worktree()
    checkout = Path(worktree['workspace'])
    send(b'\x17')
    send(b'\r')
    capture('session-deleted-workspace-kept')
    assert checkout.exists() and delete_repo.exists()
    assert all(row['id'] != worktree['id'] for row in remote_request('GET', '/v1/sessions'))
    for pane in ['Todos', 'Tasks', 'Schedules', 'Closeout']:
        send(b'\x0b')
        send(pane.encode())
        send(b'\r')
    frame = capture('empty-right-panes')
    assert 'No todos yet' in frame and 'No schedules yet' in frame, frame
    assert 'No tasks yet' in frame and 'No closeout yet' in frame, frame
    time.sleep(1)
    frame = capture('empty-right-panes-after-refresh')
    assert 'No schedules yet' in frame and 'No closeout yet' in frame, frame
    send(b'\x0b')
    send(b'Closeout')
    send(b'\r')
    send(b'\x02')
    send(b'\x1b[<0;97;10M')
    send(b'\x1b[<32;90;10M')
    send(b'\x1b[<0;90;10m')
    frame = capture('layout-before-save')
    assert '[Save layout]' not in frame and 'No schedules yet' in frame, frame
    send(b'draft kept while saving')
    send(b'\x0b')
    send(b'Save layout')
    send(b'\r')
    frame = capture('layout-saved')
    assert 'Layout saved for all sessions' in frame, frame
    assert 'draft kept while saving' in '\n'.join(frame.splitlines()[-3:]), frame
    assert 'Layout saved' not in '\n'.join(frame.splitlines()[-3:]), frame
    send(b'!')
    frame = capture('toast-with-edited-draft')
    assert 'Layout saved for all sessions' in frame, frame
    assert 'draft kept while saving!' in frame, frame
    pump(5.2)
    frame = capture('toast-expired-draft-preserved')
    assert 'Layout saved for all sessions' not in frame, frame
    assert 'draft kept while saving!' in frame, frame
    send(b'\x15')
    import tomllib
    server_config = root / 'remote-a/.kyotoagent/config.toml'
    layout = tomllib.loads(server_config.read_text())['layout']
    assert layout['left_open'] is False and layout['right_open'] is True, layout
    assert layout['right_width'] > 24, layout
    assert set(layout['right_panes']) == {'todos', 'tasks', 'schedules'}, layout
    assert remote_request('GET', '/v1/layout') == layout
    create_worktree()
    frame = capture('pane-layout-after-session-switch')
    assert 'No schedules yet' in frame and 'No closeout yet' not in frame, frame
    switch(None)
    frame = capture('local')
    assert 'remote-a' in frame and 'remote-b' in frame, frame
    assert 'work-remote-a' not in frame.splitlines()[1] and 'work-remote-b' not in frame.splitlines()[1], frame
    switch(second)
    frame = capture('restored-b')
    assert 'work-remote-b' in frame and 'draft for b' in frame
    import tomllib
    saved = tomllib.loads(cfg.read_text())
    assert saved['server'] == second
    assert len(saved['servers']) == 2
    assert cfg.stat().st_mode & 0o777 == 0o600
    switch(first)
    tui.terminate()
    tui.wait(timeout=5)
    os.close(master)
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 120, 0, 0))
    tui = subprocess.Popen([binary], env=env, stdin=slave, stdout=slave, stderr=slave, start_new_session=True, cwd=client_home)
    processes.append(tui)
    os.close(slave)
    screen.reset()
    decoder = codecs.getincrementaldecoder('utf-8')()
    frame = capture('saved-layout-after-restart')
    assert 'No schedules yet' in frame and 'No closeout yet' not in frame, frame
    assert 'ctrl-n/p' not in frame, frame
    assert tomllib.loads(server_config.read_text())['layout'] == layout
    processes[0].terminate()
    processes[0].wait(timeout=5)
    before = remote_request('GET', '/v1/sessions', uri=second_uri, host=second)
    send(b'\x14')
    frame = capture('projects-with-one-server-offline')
    assert 'remote-b' in frame and 'Add project' in frame, frame
    choose_project('remote-b')
    send(b'1')
    send(b'1')
    frame = capture('session-created-on-other-server')
    assert 'work-remote-b' in frame.splitlines()[1], frame
    after = remote_request('GET', '/v1/sessions', uri=second_uri, host=second)
    assert len(after) == len(before) + 1
    result = {'verified': ['projects from all saved servers appear on launch', 'project badges show their owning server', 'sidebar selection routes to the owning server', 'new sessions use the chosen project server while another saved server is offline', 'authenticated remote switching', 'Local selection', 'draft restoration', 'remote model picker', 'unconfigured provider exclusion', 'persisted selected server', 'private config', 'remote configured projects only', 'project creation and editing through the TUI', 'server project config persistence', 'project form draft preservation', 'unchecked workspace deletion default', 'dirty workspace warning and second confirmation', 'workspace retained after session-only deletion', 'main repository retained after worktree deletion', 'empty right panes survive refresh', 'pane selection survives session switching', 'Save layout palette command writes the server config', 'saved-layout toast stays above the composer while typing', 'toast expires while preserving the edited draft', 'saved column widths and panes survive TUI restart'], 'evidence': str(evidence)}
    (evidence / 'proof.json').write_text(json.dumps(result, indent=2))
    print(json.dumps(result))
finally:
    for process in reversed(processes):
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
    if master is not None:
        os.close(master)
    for log in logs:
        log.close()
    shutil.rmtree(root)
    (evidence / 'actions.json').write_text(json.dumps(actions, indent=2))
