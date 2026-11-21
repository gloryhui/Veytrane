#!/usr/bin/env python3
"""User-scoped Agent install/lifecycle tool. Never deletes node data or kills an owner."""
import argparse
import hashlib
import json
import ntpath
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
    host = shutil.which('pwsh')
    if not host:
        raise RuntimeError(
            'PowerShell 7+ (pwsh) is required for the current-user scheduled task; '
            'pwsh was not found on PATH and PowerShell 5.1 is not selected automatically'
        )
    env = os.environ.copy()
    env.pop('PSModulePath', None)
    env['P2P_NODE_HOST_SETTINGS'] = json.dumps(settings or {}, ensure_ascii=True)
    return run([host, '-NoProfile', '-NonInteractive', '-Command',
                "$ErrorActionPreference='Stop';"
                "if($PSVersionTable.PSVersion.Major -lt 7){"
                "throw 'PowerShell 7 or newer is required; pwsh did not provide a compatible PowerShell runtime'};"
                "$s=$env:P2P_NODE_HOST_SETTINGS|ConvertFrom-Json;" + script], env=env)


def task_name():
    sid = powershell('[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value').stdout.strip()
    return 'P2PFile-Agent-' + hashlib.sha256(sid.encode()).hexdigest()[:12]


def windows_path(value):
    return ntpath.normcase(ntpath.normpath(str(value)))


def windows_data_dirs(arguments):
    pattern = r'(?:^|\s)--data-dir(?:=|\s+)(?:"([^"]*)"|([^\s]+))(?=\s|$)'
    return [quoted or bare for quoted, bare in re.findall(pattern, arguments or '', re.IGNORECASE)]


def windows_task_preflight(state):
    if platform.system() != 'Windows':
        raise RuntimeError('Windows task preflight requires Windows')
    script = r'''
$current=[System.Security.Principal.WindowsIdentity]::GetCurrent();
$tasks=@();
foreach($task in @(Get-ScheduledTask -ErrorAction SilentlyContinue)){
  $principalSid=$null;
  $principalUser=$task.Principal.UserId;
  if($principalUser -match '^S-\d-\d+(-\d+)+$'){
    $principalSid=$principalUser;
  } else {
    try {$principalSid=(New-Object System.Security.Principal.NTAccount($principalUser)).Translate([System.Security.Principal.SecurityIdentifier]).Value} catch {};
  };
  $actions=@();
  foreach($action in @($task.Actions)){
    $actions += [pscustomobject]@{
      execute=$action.Execute;
      arguments=$action.Arguments;
      working_directory=$action.WorkingDirectory
    };
  };
  $tasks += [pscustomobject]@{
    name=$task.TaskName;
    path=$task.TaskPath;
    principal_user=$principalUser;
    principal_sid=$principalSid;
    logon_type=$task.Principal.LogonType.ToString();
    actions=$actions
  };
};
[pscustomobject]@{
  current_sid=$current.User.Value;
  tasks=$tasks
}|ConvertTo-Json -Compress -Depth 7
'''
    result = powershell(script)
    try:
        value = json.loads(result.stdout.lstrip('\ufeff'))
    except (TypeError, ValueError) as error:
        raise RuntimeError(f'Windows startup task preflight returned invalid data: {error}') from error
    if not isinstance(value, dict) or not isinstance(value.get('tasks'), list):
        raise RuntimeError('Windows startup task preflight returned incomplete data')
    return value


def select_windows_startup_task(preflight, state):
    current_sid = preflight.get('current_sid')
    if not current_sid:
        raise RuntimeError('Windows startup task preflight did not identify the current user SID')
    expected_agent = windows_path(Path(state['bin_dir']) / 'p2p-agent.exe')
    expected_data = windows_path(state['data_dir'])
    matches = []
    for task in preflight['tasks']:
        if not isinstance(task, dict):
            continue
        for index, action in enumerate(task.get('actions') or []):
            if not isinstance(action, dict) or windows_path(action.get('execute') or '') != expected_agent:
                continue
            data_dirs = windows_data_dirs(action.get('arguments'))
            if len(data_dirs) == 1 and windows_path(data_dirs[0]) == expected_data:
                matches.append((task, index))
    if not matches:
        raise RuntimeError('no Windows startup task action exactly matches the old Agent executable and --data-dir; upgrade refused before Agent shutdown')
    if len(matches) != 1:
        labels = ', '.join(f"{task.get('path', '')}{task.get('name', '')}" for task, _ in matches)
        raise RuntimeError(f'ambiguous Windows startup task selection ({labels}); upgrade refused before Agent shutdown')
    task, action_index = matches[0]
    label = f"{task.get('path', '')}{task.get('name', '')}"
    action = task['actions'][action_index]
    if task.get('principal_sid') != current_sid:
        raise RuntimeError(f'Windows startup task {label} belongs to a different account; upgrade refused before Agent shutdown')
    return {'name': task.get('name'), 'path': task.get('path') or '\\', 'action_index': action_index}


def update_windows_task(task, old, candidate):
    settings = {
        'name': task['name'],
        'path': task['path'],
        'old_agent': str(Path(old['bin_dir']) / 'p2p-agent.exe'),
        'old_working_directory': str(Path(old['bin_dir'])),
        'new_agent': str(Path(candidate['bin_dir']) / 'p2p-agent.exe'),
        'new_working_directory': str(Path(candidate['bin_dir'])),
        'data_dir': str(Path(old['data_dir'])),
    }
    script = r'''
function Test-SamePath([string]$left,[string]$right){
  if(-not $left -or -not $right){return $false};
  return [string]::Equals([IO.Path]::GetFullPath($left),[IO.Path]::GetFullPath($right),[StringComparison]::OrdinalIgnoreCase)
};
$task=Get-ScheduledTask -TaskName $s.name -TaskPath $s.path -ErrorAction Stop;
if($null -eq $task){throw 'startup task disappeared'};
$before=[xml](Export-ScheduledTask -TaskName $s.name -TaskPath $s.path);
$beforePrincipal=$before.Task.Principals.OuterXml;
$beforeTriggers=$before.Task.Triggers.OuterXml;
$beforeSettings=$before.Task.Settings.OuterXml;
$beforeActions=@($before.Task.Actions.ChildNodes);
$pattern='(?:^|\s)--data-dir(?:=|\s+)(?:"'+[regex]::Escape($s.data_dir)+'"|'+[regex]::Escape($s.data_dir)+')(?=\s|$)';
$matched=@();
$actions=@($task.Actions);
for($index=0;$index -lt $actions.Count;$index++){
  $action=$actions[$index];
  if((Test-SamePath $action.Execute $s.old_agent) -and
     [regex]::IsMatch($action.Arguments,$pattern,[Text.RegularExpressions.RegexOptions]::IgnoreCase)){
    $matched+=@($index);
  };
};
if($matched.Count -ne 1){throw 'startup task no longer has one exact old installation action'};
$actionIndex=$matched[0];
$beforeArguments=$actions[$actionIndex].Arguments;
$updateWorkingDirectory=(Test-SamePath $actions[$actionIndex].WorkingDirectory $s.old_working_directory);
$actions[$matched[0]].Execute=$s.new_agent;
if($updateWorkingDirectory){$actions[$matched[0]].WorkingDirectory=$s.new_working_directory};
$updated=Set-ScheduledTask -TaskName $s.name -TaskPath $s.path -Action $actions -ErrorAction Stop;
$after=[xml](Export-ScheduledTask -TaskName $s.name -TaskPath $s.path);
$afterActions=@($after.Task.Actions.ChildNodes);
if($after.Task.Principals.OuterXml -ne $beforePrincipal -or
   $after.Task.Triggers.OuterXml -ne $beforeTriggers -or
   $after.Task.Settings.OuterXml -ne $beforeSettings -or
   $afterActions.Count -ne $beforeActions.Count){throw 'task principal, triggers, settings, or action count changed'};
for($index=0;$index -lt $beforeActions.Count;$index++){
  if($index -eq $actionIndex){
    $beforeAction=$beforeActions[$index];
    $afterAction=$afterActions[$index];
    if($beforeAction.Attributes.Count -ne $afterAction.Attributes.Count){throw 'matched task action attributes changed'};
    foreach($attribute in @($beforeAction.Attributes)){
      if($afterAction.GetAttribute($attribute.Name) -ne $attribute.Value){throw 'matched task action attributes changed'};
    };
    if($afterAction.Arguments -ne $beforeArguments -or
       $afterAction.Command -ne $s.new_agent -or
       ($updateWorkingDirectory -and $afterAction.WorkingDirectory -ne $s.new_working_directory) -or
       (-not $updateWorkingDirectory -and $afterAction.WorkingDirectory -ne $beforeAction.WorkingDirectory)){
      throw 'matched task action did not preserve arguments or apply only the allowed path update';
    };
  } elseif($afterActions[$index].OuterXml -ne $beforeActions[$index].OuterXml){
    throw 'another task action changed';
  };
};
[pscustomobject]@{name=$updated.TaskName;path=$updated.TaskPath;logon_type=$updated.Principal.LogonType.ToString()}|ConvertTo-Json -Compress
'''
    try:
        result = powershell(script, settings)
    except RuntimeError as error:
        raise RuntimeError('Windows startup task update or verification failed before Agent shutdown; the old Agent is still running and no credentials were requested') from error
    try:
        value = json.loads(result.stdout.lstrip('\ufeff'))
    except (TypeError, ValueError) as error:
        raise RuntimeError(f'Windows startup task update returned invalid data: {error}') from error
    if value.get('name') != task['name'] or value.get('path') != task['path']:
        raise RuntimeError('Windows startup task identity changed during update')
    return value


def start_windows_task(task):
    powershell('Start-ScheduledTask -TaskName $s.name -TaskPath $s.path',
               {'name': task['name'], 'path': task['path']})


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
        logs = Path.home() / 'Library/Logs/p2p-file-node'
        logs.mkdir(parents=True, exist_ok=True, mode=0o700)
        contents = {'Label': 'io.github.gloryhui.p2p-agent', 'ProgramArguments': [agent, '--data-dir', state['data_dir'], '--managed'],
                    'RunAtLoad': True, 'KeepAlive': {'SuccessfulExit': False}, 'ThrottleInterval': 10,
                    'ExitTimeOut': 30, 'EnvironmentVariables': {'RUST_LOG': 'warn'},
                    'StandardOutPath': str(logs / 'agent.stdout.log'),
                    'StandardErrorPath': str(logs / 'agent.stderr.log')}
        with service.open('wb') as stream:
            plistlib.dump(contents, stream)
        service.chmod(0o600)
    else:
        def quote(value):
            return '"' + str(value).replace('\\', '\\\\').replace('"', '\\"').replace('%', '%%') + '"'
        service.parent.mkdir(parents=True, exist_ok=True)
        service.write_text('[Unit]\nDescription=Veytrane user Agent\nStartLimitIntervalSec=60\nStartLimitBurst=5\n'
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
    if agent.get('api_version') != 1 or any(metadata[name].get(key) != agent.get(key) for name in metadata for key in ('build_sha', 'target', 'version', 'source_state', 'api_version', 'edition', 'files')):
        raise RuntimeError('Agent and CLI must come from exactly the same build')
    edition = agent.get('edition')
    if any('edition' in info or 'files' in info for info in metadata.values()):
        if edition not in ('standard', 'files') or agent.get('files') is not (edition == 'files'):
            raise RuntimeError('invalid Agent/CLI build edition')
        if any(info.get('name') != name for name, info in metadata.items()):
            raise RuntimeError('incorrect Agent/CLI binary role')
    if not allow_dirty and agent['source_state'] != 'clean':
        raise RuntimeError('dirty build cannot be installed without explicit --allow-dirty')
    version_key = f"{agent['build_sha']}-{edition}" if edition else agent['build_sha']
    target = base / version_key
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


def check_candidate_config(candidate):
    if 'edition' in candidate['build']:
        data_dir = Path(candidate['data_dir'])
        config = data_dir / 'node.json'
        if config.exists() or config.is_symlink():
            suffix = '.exe' if platform.system() == 'Windows' else ''
            agent = Path(candidate['bin_dir']) / f'p2p-agent{suffix}'
            run([agent, '--data-dir', data_dir, '--check-config'])


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
        startup_task = select_windows_startup_task(windows_task_preflight(old), old) if old and platform.system() == 'Windows' else None
        previous_status = response(old, ['status']) if old else None
        candidate = install(args.bin_dir.resolve(), base, data_dir, args.allow_dirty)
        check_candidate_config(candidate)
        if old:
            atomic_json(base / 'previous-install.json', {'installation': old, 'status': previous_status})
            if startup_task:
                update_windows_task(startup_task, old, candidate)
            stop(old, service)
            if not startup_task:
                disable(old, service)
        atomic_json(state_file, candidate)
        if old:
            if startup_task:
                start_windows_task(startup_task)
            else:
                enable(candidate, service)
                start(candidate, service)
            deadline = time.monotonic() + 30
            while time.monotonic() < deadline:
                status = response(candidate, ['status'], check=False)
                if status:
                    if (status['node']['node_id'] != previous_status['node']['node_id'] or
                            status['revision'] < previous_status['revision'] or
                            any(status['build'].get(key) != candidate['build'].get(key)
                                for key in ('build_sha', 'edition'))):
                        raise RuntimeError('upgrade identity/revision/build mismatch; no automatic data rollback')
                    print(json.dumps({'upgraded': True, 'build_sha': candidate['build']['build_sha'],
                                      'edition': candidate['build'].get('edition')}))
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
