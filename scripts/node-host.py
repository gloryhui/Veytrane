#!/usr/bin/env python3
"""User-scoped Agent install/lifecycle tool. Never deletes node data or kills an owner."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import plistlib
import re
import shutil
import subprocess
import tempfile
import time


def run(argv, *, check=True, env=None):
    result = subprocess.run([str(arg) for arg in argv], capture_output=True, text=True,
                            encoding='utf-8', errors='replace', env=env, timeout=45,
                            creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
    if check and result.returncode:
        raise RuntimeError(f'{argv[0]} failed ({result.returncode}): {result.stderr.strip()}')
    return result


def atomic_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix='.node-host-', dir=path.parent)
    try:
        with os.fdopen(fd, 'w', encoding='utf-8') as stream:
            json.dump(value, stream, ensure_ascii=False, indent=2)
            stream.write('\n')
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def user_data():
    if platform.system() == 'Windows':
        return Path(os.environ['LOCALAPPDATA']) / 'p2p-file' / 'node'
    if platform.system() == 'Darwin':
        return Path.home() / 'Library/Application Support/p2p-file/node'
    return Path(os.environ.get('XDG_DATA_HOME', Path.home() / '.local/share')) / 'p2p-file/node'


def powershell(script, settings=None):
    host = shutil.which('powershell.exe') or shutil.which('pwsh')
    if not host:
        raise RuntimeError('PowerShell is required for the current-user scheduled task')
    env = os.environ.copy()
    env.pop('PSModulePath', None)
    env['P2P_NODE_HOST_SETTINGS'] = json.dumps(settings or {}, ensure_ascii=True)
    return run([host, '-NoProfile', '-NonInteractive', '-Command',
                "$ErrorActionPreference='Stop';$s=$env:P2P_NODE_HOST_SETTINGS|ConvertFrom-Json;" + script], env=env)


def task_name():
    sid = powershell('[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value').stdout.strip()
    return 'P2PFile-Agent-' + hashlib.sha256(sid.encode()).hexdigest()[:12]


def locations():
    if platform.system() == 'Windows':
        root = Path(os.environ['LOCALAPPDATA']) / 'p2p-file/bin'
        return root, None
    if platform.system() == 'Darwin':
        return Path.home() / 'Library/Application Support/p2p-file/bin', Path.home() / 'Library/LaunchAgents/io.github.gloryhui.p2p-agent.plist'
    if platform.system() != 'Linux':
        raise RuntimeError('unsupported host')
    config = Path(os.environ.get('XDG_CONFIG_HOME', Path.home() / '.config'))
    return Path.home() / '.local/lib/p2p-file-node', config / 'systemd/user/p2p-file-agent.service'


def cli(state):
    return [Path(state['bin_dir']) / ('p2p-node.exe' if platform.system() == 'Windows' else 'p2p-node'),
            '--data-dir', state['data_dir']]


def response(state, command, check=True):
    result = run(cli(state) + command, check=False)
    try:
        value = json.loads(result.stdout)
    except ValueError:
        if check:
            raise RuntimeError('management CLI did not return a valid response')
        return None
    if result.returncode or value.get('error'):
        if check:
            raise RuntimeError(str(value.get('error') or 'management command failed'))
        return None
    return value['result']


def wait_released(state):
    # Successful IPC stop alone is not proof that file checkpoints/locks released.
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        if response(state, ['ownership-status'], check=False) is not None:
            return
        time.sleep(.25)
    raise RuntimeError('owner/identity/task locks did not release; no force kill or replacement performed')


def disable(state, service):
    if platform.system() == 'Windows':
        powershell('Disable-ScheduledTask -TaskName $s.name|Out-Null', {'name': task_name()})
    elif platform.system() == 'Darwin':
        run(['launchctl', 'disable', f'gui/{os.getuid()}/io.github.gloryhui.p2p-agent'])
    else:
        run(['systemctl', '--user', 'disable', 'p2p-file-agent.service'])


def stop(state, service):
    response(state, ['stop'], check=False)
    # Do not use systemctl stop/Stop-ScheduledTask: those may forcibly terminate
    # a process that is still committing a checkpoint.
    wait_released(state)


def service_write(state, service):
    agent = str(Path(state['bin_dir']) / ('p2p-agent.exe' if platform.system() == 'Windows' else 'p2p-agent'))
    if platform.system() == 'Windows':
        settings = {'name': task_name(), 'agent': agent,
                    'arguments': subprocess.list2cmdline(['--data-dir', state['data_dir']])}
        powershell("$sid=[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value;"
                   '$a=New-ScheduledTaskAction -Execute $s.agent -Argument $s.arguments;'
                   '$t=New-ScheduledTaskTrigger -AtLogOn -User $sid;'
                   '$p=New-ScheduledTaskPrincipal -UserId $sid -LogonType Interactive -RunLevel Limited;'
                   '$o=New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero);'
                   'Register-ScheduledTask -TaskName $s.name -Action $a -Trigger $t -Principal $p -Settings $o -Force|Out-Null', settings)
    elif platform.system() == 'Darwin':
        service.parent.mkdir(parents=True, exist_ok=True)
        contents = {'Label': 'io.github.gloryhui.p2p-agent', 'ProgramArguments': [agent, '--data-dir', state['data_dir']],
                    'RunAtLoad': True, 'KeepAlive': {'SuccessfulExit': False}, 'ThrottleInterval': 10}
        with service.open('wb') as stream:
            plistlib.dump(contents, stream)
        service.chmod(0o600)
    else:
        def quote(value):
            return '"' + str(value).replace('\\', '\\\\').replace('"', '\\"').replace('%', '%%') + '"'
        service.parent.mkdir(parents=True, exist_ok=True)
        service.write_text('[Unit]\nDescription=P2P File user Agent\nStartLimitIntervalSec=60\nStartLimitBurst=5\n'
                           '[Service]\nType=simple\nExecStart=' + quote(agent) + ' --data-dir ' + quote(state['data_dir']) +
                           '\nRestart=on-failure\nRestartSec=5\nTimeoutStopSec=30\n[Install]\nWantedBy=default.target\n', encoding='utf-8')
        run(['systemctl', '--user', 'daemon-reload'])


def enable(state, service):
    wait_released(state)
    service_write(state, service)
    if platform.system() == 'Darwin':
        run(['launchctl', 'bootout', f'gui/{os.getuid()}', service], check=False)
        run(['launchctl', 'enable', f'gui/{os.getuid()}/io.github.gloryhui.p2p-agent'])
        run(['launchctl', 'bootstrap', f'gui/{os.getuid()}', service])
    elif platform.system() == 'Linux':
        run(['systemctl', '--user', 'enable', 'p2p-file-agent.service'])


def start(state, service):
    if platform.system() == 'Windows':
        powershell('Start-ScheduledTask -TaskName $s.name', {'name': task_name()})
    elif platform.system() == 'Darwin':
        run(['launchctl', 'kickstart', f'gui/{os.getuid()}/io.github.gloryhui.p2p-agent'])
    else:
        run(['systemctl', '--user', 'start', 'p2p-file-agent.service'])


def install(source, base, data_dir, allow_dirty):
    suffix = '.exe' if platform.system() == 'Windows' else ''
    sources = {name: source / (name + suffix) for name in ('p2p-agent', 'p2p-node')}
    metadata = {name: json.loads(run([path, '--build-info']).stdout) for name, path in sources.items()}
    agent = metadata['p2p-agent']
    if not re.fullmatch('[0-9a-f]{40}', agent['build_sha']):
        raise RuntimeError('invalid build SHA')
    if agent.get('api_version') != 1 or any(metadata[name].get(key) != agent.get(key) for name in metadata for key in ('build_sha', 'target', 'version', 'source_state', 'api_version')):
        raise RuntimeError('Agent and CLI must come from exactly the same build')
    if not allow_dirty and agent['source_state'] != 'clean':
        raise RuntimeError('dirty build cannot be installed without explicit --allow-dirty')
    target = base / agent['build_sha']
    if target.exists():
        if target.is_symlink() or not target.is_dir():
            raise RuntimeError('version directory is not a plain directory')
        for name, source_file in sources.items():
            destination = target / source_file.name
            if not destination.is_file() or destination.is_symlink():
                raise RuntimeError('partial immutable version directory; inspect it before retrying')
            with source_file.open('rb') as source_stream, destination.open('rb') as destination_stream:
                if hashlib.file_digest(source_stream, 'sha256').digest() != hashlib.file_digest(destination_stream, 'sha256').digest():
                    raise RuntimeError('existing version content differs; it is never overwritten')
        return {'schema': 1, 'bin_dir': str(target), 'data_dir': str(data_dir), 'build': agent}
    target.mkdir(parents=True, mode=0o700)
    for name, path in sources.items():
        shutil.copy2(path, target / path.name)
        if platform.system() != 'Windows':
            (target / path.name).chmod(0o755)
    return {'schema': 1, 'bin_dir': str(target), 'data_dir': str(data_dir), 'build': agent}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('action', choices=['install', 'upgrade', 'enable', 'start', 'stop', 'status', 'disable', 'uninstall'])
    parser.add_argument('--bin-dir', type=Path, help='directory containing agent/node binaries for install/upgrade')
    parser.add_argument('--data-dir', type=Path)
    parser.add_argument('--allow-dirty', action='store_true')
    args = parser.parse_args()
    base, service = locations()
    state_file = base / 'installed.json'
    old = json.loads(state_file.read_text(encoding='utf-8')) if state_file.exists() else None
    if args.action in ('install', 'upgrade'):
        if not args.bin_dir:
            parser.error('--bin-dir is required')
        if args.action == 'install' and old:
            parser.error('already installed; use upgrade')
        if args.action == 'upgrade' and not old:
            parser.error('not installed')
        data_dir = (args.data_dir or (Path(old['data_dir']) if old else user_data())).resolve()
        if old and data_dir != Path(old['data_dir']):
            parser.error('upgrade cannot change the identity/data directory')
        previous_status = response(old, ['status']) if old else None
        candidate = install(args.bin_dir.resolve(), base, data_dir, args.allow_dirty)
        if old:
            stop(old, service)
            atomic_json(base / 'previous-install.json', {'installation': old, 'status': previous_status})
            disable(old, service)
        atomic_json(state_file, candidate)
        if old:
            enable(candidate, service)
            start(candidate, service)
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                status = response(candidate, ['status'], check=False)
                if status:
                    if (status['node']['node_id'] != previous_status['node']['node_id'] or
                            status['revision'] < previous_status['revision'] or status['build']['build_sha'] != candidate['build']['build_sha']):
                        raise RuntimeError('upgrade identity/revision/build mismatch; no automatic data rollback')
                    print(json.dumps({'upgraded': True, 'build_sha': candidate['build']['build_sha']}))
                    return
                time.sleep(.5)
            raise RuntimeError('new Agent did not become ready; previous installation is retained; no automatic permission rollback')
        print(json.dumps({'installed': True, 'startup_enabled': False, 'data_untouched': True, 'bin_dir': candidate['bin_dir']}))
        return
    if not old:
        parser.error('not installed')
    if args.action == 'status':
        print(json.dumps(response(old, ['status']), ensure_ascii=False))
    elif args.action == 'enable':
        enable(old, service)
    elif args.action == 'start':
        start(old, service)
    elif args.action == 'stop':
        stop(old, service)
    elif args.action == 'disable':
        disable(old, service)
    elif args.action == 'uninstall':
        stop(old, service)
        disable(old, service)
        if platform.system() == 'Windows':
            powershell('Unregister-ScheduledTask -TaskName $s.name -Confirm:$false', {'name': task_name()})
        elif service.exists():
            service.unlink()
            if platform.system() == 'Linux':
                run(['systemctl', '--user', 'daemon-reload'])
        state_file.unlink()
        print(json.dumps({'uninstalled_startup': True, 'data_and_version_directories_preserved': True}))


if __name__ == '__main__':
    main()
