"""Isolated legacy CLI process fixtures for node regression scripts (stdlib only)."""
import contextlib
import asyncio
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request


def port(kind=socket.SOCK_STREAM):
    with socket.socket(type=kind) as listener:
        listener.bind(('127.0.0.1', 0))
        return listener.getsockname()[1]


def exchange(stream, data):
    stream.sendall(data)
    received = b''
    while len(received) < len(data):
        chunk = stream.recv(len(data) - len(received))
        if not chunk:
            raise ConnectionError('echo stream closed before full response')
        received += chunk
    if received != data:
        raise AssertionError('echo payload mismatch')


def execution_binary(source):
    """Keep one executable path per build directory across fixture runs.

    Windows firewall grants are path based. Per-run copies cause a new security
    prompt on every run; running the Cargo output itself prevents relinking it.
    Retain stable copies separately for baseline and current builds instead.
    Never replace a running copy: Windows reports a sharing error in that case.
    """
    directory = Path(__file__).resolve().parents[1] / 'target' / 'node-smoke-bin' / source.parent.name
    directory.mkdir(parents=True, exist_ok=True)
    destination = directory / source.name
    def digest(path):
        with path.open('rb') as stream:
            return hashlib.file_digest(stream, 'sha256').digest()
    if not destination.exists() or digest(source) != digest(destination):
        try:
            shutil.copy2(source, destination)
        except PermissionError as error:
            raise RuntimeError(f'stop the previous smoke fixture before updating {destination}') from error
    return destination


class Fixture:
    def __init__(self, binary, output, relay=False):
        source_binary = Path(binary).resolve()
        self.output = Path(output).resolve()
        self.output.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix='p2p-node-smoke-')
        self.root = Path(self.temporary.name)
        self.binary = str(execution_binary(source_binary))
        self.processes = []
        self.handles = []
        self.token = 'isolated-test-token-' + 'a' * 48
        self.signal_port = port()
        self.relay = relay
        self.backend_port = port() if relay else self.signal_port
        self.relay_port = port(socket.SOCK_DGRAM) if relay else None
        self.roles = {}
        self.echo = socket.socket()
        self.echo.bind(('127.0.0.1', 0))
        self.echo.listen()
        self.echo.settimeout(.2)
        self.echo_port = self.echo.getsockname()[1]
        self.echo_stop = threading.Event()
        self.echo_connections = []
        self.echo_thread = threading.Thread(target=self._echo, daemon=True)
        self.echo_thread.start()
        self.server_id = self.identity('server')
        self.client_id = self.identity('client')
        self.a, self.b = port(), port()
        self.server_web, self.client_web = port(), port()
        self.service = {'version': 1, 'forward_targets': [self.target('echo'), self.target('spare')]}
        self.service['forward_targets'][1].update(target='127.0.0.1:9', enabled=False)
        self.client = {'version': 1, 'rules': [self.rule('a', self.a), self.rule('b', self.b)]}
        self.write('service.json', self.service)
        self.write('client.json', self.client)
        (self.root / 'admin.token').write_text(self.token, encoding='utf-8')
        (self.root / 'admin.token').chmod(0o600)

    def identity(self, name):
        text = subprocess.check_output([self.binary, '--key-file', str(self.root / (name + '.key')), 'id'], encoding='utf-8', errors='replace', timeout=10)
        return re.search(r'(?<![0-9a-f])[0-9a-f]{32}(?![0-9a-f])', text).group()

    def target(self, name):
        return {'id': name, 'name': name, 'target': f'127.0.0.1:{self.echo_port}', 'enabled': True, 'allowed_peers': [self.client_id]}

    def rule(self, name, listen):
        return {'id': name, 'name': name, 'peer_node_id': self.server_id, 'listen': f'127.0.0.1:{listen}', 'target': f'127.0.0.1:{self.echo_port}', 'enabled': True, 'auto_start': True}

    def write(self, name, data):
        (self.root / name).write_text(json.dumps(data), encoding='utf-8')

    def _echo(self):
        def serve(stream):
            with stream:
                try:
                    while chunk := stream.recv(65536):
                        stream.sendall(chunk)
                except OSError:
                    pass
        while not self.echo_stop.is_set():
            try:
                stream, _ = self.echo.accept()
                self.echo_connections.append(stream)
                threading.Thread(target=serve, args=(stream,), daemon=True).start()
            except socket.timeout:
                continue
            except OSError:
                break

    def spawn(self, name, args):
        handle = (self.output / (name + '.log')).open('w', encoding='utf-8')
        self.handles.append(handle)
        env = os.environ.copy()
        env['RUST_LOG'] = 'info'
        env['NO_COLOR'] = '1'
        process = subprocess.Popen([self.binary] + args, stdout=handle, stderr=handle, env=env)
        self.processes.append(process)
        return process

    def wait_log(self, name, marker, timeout=30):
        until = time.monotonic() + timeout
        while time.monotonic() < until:
            text = (self.output / (name + '.log')).read_text(encoding='utf-8', errors='replace')
            if marker in text:
                return text
            time.sleep(.05)
        raise AssertionError(f'{name}: missing readiness marker {marker!r}; see {self.output}')

    def start_signal(self, name='signal'):
        if not self.relay:
            self.signal = self.spawn(name, ['signal-server', '--listen', f'127.0.0.1:{self.signal_port}'])
            self.wait_log(name, '信令服务器已启动')
            return
        if name == 'signal':
            self.spawn('relay-backend', ['signal-server', '--listen', f'127.0.0.1:{self.backend_port}', '--relay-listen', f'127.0.0.1:{self.relay_port}'])
            self.wait_log('relay-backend', '信令服务器已启动')
        # A separately killable TCP frontend cuts ONLY signaling. Its UDP Relay
        # backend stays alive, so this never confuses Relay shutdown with BUG-01.
        log = (self.output / (name+'.log')).open('w',encoding='utf-8');self.handles.append(log)
        self.signal = subprocess.Popen([sys.executable,str(Path(__file__).resolve()),'signal-proxy',str(self.signal_port),str(self.backend_port)],stdout=log,stderr=log)
        self.processes.append(self.signal)
        self.wait_log(name,'proxy-ready')

    def start(self):
        self.start_signal()
        for name, web, config, command in [('server', self.server_web, 'service.json', 'serve'), ('client', self.client_web, 'client.json', 'forward')]:
            udp = port(socket.SOCK_DGRAM)
            args = ['--key-file', str(self.root / (name + '.key')), command, '--signal', f'127.0.0.1:{self.signal_port}', '--stun', '127.0.0.1:9', '--ip-family', 'ipv4-only', '--port', str(udp), '--advertise-only', '--advertise', f'127.0.0.1:{udp}', '--config', str(self.root / config), '--admin-listen', f'127.0.0.1:{web}', '--admin-token-file', str(self.root / 'admin.token')]
            if name == 'server':
                args += ['--re-punch-after', '120']
            if self.relay:
                args[args.index('--advertise')+1] = '127.0.0.1:9'
                args += ['--relay',f'127.0.0.1:{self.relay_port}']
            self.roles[name] = args
            process = self.spawn(name, args)
            setattr(self, name + '_process', process)
        self.wait_log('client', '常驻运行中')
        self.wait_log('server', '直连已就绪')
        self.wait_log('client', 'Web 管理页已启动')
        self.request_echo(self.a)
        text = (self.output / 'client.log').read_text(encoding='utf-8', errors='replace')
        assert 'authenticated winner transport' in text and f'path_kind="{"relay" if self.relay else "direct"}"' in text, 'fixture did not prove actual winner'

    def restart_client(self):
        self.client_process.terminate();self.client_process.wait(timeout=5)
        self.client_process=self.spawn('client-restored',self.roles['client'])
        self.wait_log('client-restored','Web 管理页已启动')
        self.request_echo(self.a)
        text=(self.output/'client-restored.log').read_text(encoding='utf-8',errors='replace')
        assert f'path_kind="{"relay" if self.relay else "direct"}"' in text

    def connect(self, listen):
        stream = socket.create_connection(('127.0.0.1', listen), timeout=4)
        stream.settimeout(4)
        return stream

    def request_echo(self, listen):
        with self.connect(listen) as stream:
            exchange(stream, b'isolated-node-smoke-request')

    def api(self, role, route, method='GET', value=None):
        web = self.server_web if role == 'service' else self.client_web
        req = urllib.request.Request(f'http://127.0.0.1:{web}/api/v1/{route}', method=method,
            data=None if value is None else json.dumps(value).encode(),
            headers={'Authorization': 'Bearer ' + self.token, 'Content-Type': 'application/json'})
        try:
            with urllib.request.urlopen(req, timeout=8) as response:
                return response.status, json.load(response)
        except urllib.error.HTTPError as response:
            return response.code, json.load(response)

    def close(self):
        for process in reversed(self.processes):
            if process.poll() is None:
                process.terminate()
            with contextlib.suppress(subprocess.TimeoutExpired):
                process.wait(timeout=5)
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)
        for handle in self.handles:
            handle.close()
        self.echo_stop.set()
        self.echo.close()
        for stream in self.echo_connections:
            with contextlib.suppress(OSError):
                stream.shutdown(socket.SHUT_RDWR)
        self.echo_thread.join(timeout=2)
        self.temporary.cleanup()

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()


async def signal_proxy(listen, backend):
    async def connected(reader,writer):
        upstream=None
        try:
            upstream_reader,upstream=await asyncio.open_connection('127.0.0.1',backend)
            async def copy(source,target):
                while data:=await source.read(65536):
                    target.write(data);await target.drain()
            one=asyncio.create_task(copy(reader,upstream));two=asyncio.create_task(copy(upstream_reader,writer))
            done,pending=await asyncio.wait([one,two],return_when=asyncio.FIRST_COMPLETED)
            for task in pending:task.cancel()
            await asyncio.gather(*done,*pending,return_exceptions=True)
        finally:
            writer.close()
            if upstream:upstream.close()
    server=await asyncio.start_server(connected,'127.0.0.1',listen)
    print('proxy-ready',flush=True)
    async with server:await server.serve_forever()


if __name__=='__main__' and len(sys.argv)==4 and sys.argv[1]=='signal-proxy':
    asyncio.run(signal_proxy(int(sys.argv[2]),int(sys.argv[3])))
