#!/usr/bin/env python3
"""Install the independent Mac navigation backend, preserving every Agent/forward.

Only modifies this control plane's versioned runtime, its own LaunchAgent and the
existing nginx navigation vhost's /api/navigation/ location. No Agent lifecycle
commands or node-config writes are performed.
"""
import argparse
import datetime
import hashlib
import http.client
import json
import os
from pathlib import Path
import plistlib
import re
import socket
import subprocess
import sys
import time

from navigation_manager import write_private


def run(argv, check=True):
    return subprocess.run([str(s) for s in argv], capture_output=True, text=True, check=check, timeout=110)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--replace", action="store_true", help="gracefully replace only an already installed navigation backend")
    parser.add_argument("--nginx-conf", type=Path, default=Path('/opt/homebrew/etc/nginx/conf.d/p2p-service-navigation.conf'))
    args = parser.parse_args()
    if sys.platform != "darwin":
        parser.error("This installer targets the current Mac only")
    here = Path(__file__).resolve().parent
    private = Path.home() / 'Library/Application Support/p2p-file/navigation'
    label = 'io.github.gloryhui.p2p-navigation-control'
    service = f'gui/{os.getuid()}/{label}'
    plist = Path.home() / 'Library/LaunchAgents' / (label + '.plist')
    nginx = Path('/opt/homebrew/bin/nginx')
    for path in (private, args.nginx_conf, plist.parent):
        if not path.exists() or path.is_symlink() or path.resolve() != path.absolute():
            raise SystemExit(f'Expected an existing canonical non-symlink path: {path}')
        if path.stat().st_uid != os.getuid():
            raise SystemExit(f'Expected current-user ownership: {path}')
    old = args.nginx_conf.read_text()
    if 'listen 127.0.0.1:3001;' not in old or 'location = /service-navigation.html' not in old:
        raise SystemExit('The selected nginx file is not the existing local navigation vhost')
    run([nginx, '-t'])
    job = run(['/bin/launchctl', 'print', service], check=False)
    loaded = job.returncode == 0
    match = re.search(r'^\s*pid = ([0-9]+)\s*$', job.stdout, re.MULTILINE)
    old_pid = int(match[1]) if match else None
    if loaded and not args.replace:
        raise SystemExit('Navigation backend already running. Use --replace for a graceful gateway-only replacement.')
    if not loaded:
        with socket.socket() as probe:
            probe.bind(('127.0.0.1', 17790))
    if loaded:
        installed_plist = plistlib.loads(plist.read_bytes())
        command = installed_plist.get('ProgramArguments', [])
        if installed_plist.get('Label') != label or len(command) < 2 or not Path(command[1]).is_relative_to(private / 'runtime'):
            raise SystemExit('Existing LaunchAgent is not owned by this installer')
    names = ('navigation-server.py', 'navigation_manager.py')
    digest = hashlib.sha256(b''.join((here / n).read_bytes() for n in names)).hexdigest()[:16]
    runtime = private / 'runtime' / digest
    runtime.mkdir(parents=True, exist_ok=True, mode=0o700)
    if runtime.resolve() != runtime:
        raise SystemExit('Runtime path must not traverse symlinks')
    for name in names:
        source, destination = here / name, runtime / name
        if destination.exists() and destination.read_bytes() != source.read_bytes():
            raise SystemExit('Versioned runtime already exists with different content')
        if not destination.exists():
            write_private(destination, source.read_text())
    backup = private / 'installation-backups' / datetime.datetime.now().strftime('%Y%m%d-%H%M%S-%f')
    backup.mkdir(parents=True, mode=0o700)
    write_private(backup / 'nginx-before.conf', old)
    if plist.exists():
        write_private(backup / 'launchagent-before.plist', plist.read_text())
    page = private / 'service-navigation.html'
    if page.exists():
        write_private(backup / 'navigation-before.html', page.read_text())
    location = '''    location ^~ /api/navigation/ {
        proxy_pass http://127.0.0.1:17790;
        proxy_set_header Host $http_host;
        proxy_http_version 1.1;
        proxy_read_timeout 100s;
        proxy_send_timeout 100s;
        client_max_body_size 32k;
    }
'''
    if 'location ^~ /api/navigation/' in old:
        if 'proxy_pass http://127.0.0.1:17790;' not in old:
            raise SystemExit('Existing navigation API proxy uses an unexpected destination')
        new = old
    else:
        if old.count('    location / {') != 1:
            raise SystemExit('Cannot identify the navigation fallback location')
        new = old.replace('    location / {', location + '    location / {')
    for header, value in [
        ('X-Frame-Options', 'DENY'),
        ('Referrer-Policy', 'no-referrer'),
        ('Content-Security-Policy', "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; base-uri 'none'; frame-ancestors 'none'; form-action 'none'"),
    ]:
        if header not in new:
            new = new.replace('    autoindex off;', '    autoindex off;\n    add_header ' + header + ' "' + value + '" always;')
    logs = private / 'control'
    logs.mkdir(mode=0o700, exist_ok=True)
    launch = {'Label': label, 'ProgramArguments': [sys.executable, str(runtime / 'navigation-server.py')],
              'RunAtLoad': True, 'KeepAlive': True, 'ThrottleInterval': 5, 'ExitTimeOut': 100,
              'Umask': 0o077, 'StandardOutPath': str(logs / 'stdout.log'), 'StandardErrorPath': str(logs / 'stderr.log')}
    if loaded:
        run(['/bin/launchctl', 'bootout', service])
        # launchd acknowledges bootout before it has drained/removed the job.
        deadline = time.monotonic() + 100
        def draining():
            if run(['/bin/launchctl', 'print', service], check=False).returncode == 0:
                return True
            if old_pid is not None:
                try:
                    os.kill(old_pid, 0)  # Read-only existence check; never kill an owner.
                    return True
                except ProcessLookupError:
                    pass
            return False
        while draining():
            if time.monotonic() > deadline:
                raise SystemExit('The old navigation gateway is still draining. No Agent was restarted.')
            time.sleep(.2)
    write_private(plist, plistlib.dumps(launch).decode())
    run(['/bin/launchctl', 'bootstrap', f'gui/{os.getuid()}', plist])
    deadline = time.monotonic() + 12
    while True:
        try:
            connection = http.client.HTTPConnection('127.0.0.1', 17790, timeout=1)
            connection.request('GET', '/api/navigation/bootstrap', headers={'X-Navigation-Client': '1'})
            response = connection.getresponse()
            ready = response.status == 200 and bool(json.loads(response.read()).get('token'))
            connection.close()
            if ready:
                break
        except (OSError, ValueError, http.client.HTTPException):
            pass
        if time.monotonic() > deadline:
            raise SystemExit('The navigation backend has not become ready. Check navigation/control/stderr.log.')
        time.sleep(.2)
    if new != old:
        write_private(args.nginx_conf, new)
        try:
            run([nginx, '-t'])
        except subprocess.CalledProcessError:
            write_private(args.nginx_conf, old)
            raise
        run([nginx, '-s', 'reload'])
    print(json.dumps({'installed': True, 'runtime': str(runtime), 'backup': str(backup),
                      'url': 'http://127.0.0.1:3001/', 'agent_restarted': False}, ensure_ascii=False))


if __name__ == '__main__':
    main()
