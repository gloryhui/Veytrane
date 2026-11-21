#!/usr/bin/env python3
"""Hosting boundaries and optional isolated current-user Windows scheduled task smoke."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location('node_host', Path(__file__).with_name('node-host.py'))
host = importlib.util.module_from_spec(spec)
spec.loader.exec_module(host)


class Hosting(unittest.TestCase):
    def test_windows_powershell_prefers_pwsh_and_preserves_structured_environment(self):
        captured = {}
        def choose(command):
            captured.setdefault('lookups', []).append(command)
            return r'C:\Program Files\PowerShell\7\pwsh.exe' if command == 'pwsh' else r'C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe'
        def invoke(argv, *, check=True, env=None):
            captured['argv'] = argv
            captured['check'] = check
            captured['env'] = env
            return subprocess.CompletedProcess(argv, 0, 'ok', '')
        with patch.object(host.shutil, 'which', side_effect=choose), \
                patch.object(host, 'run', side_effect=invoke), \
                patch.dict(host.os.environ, {'PSModulePath': 'legacy-module-path'}, clear=False):
            result = host.powershell('Write-Output $s.value', {'value': 'fixture'})
        self.assertEqual(result.stdout, 'ok')
        self.assertEqual(captured['lookups'], ['pwsh'])
        self.assertEqual(captured['argv'][0], r'C:\Program Files\PowerShell\7\pwsh.exe')
        self.assertIn('$PSVersionTable.PSVersion.Major -lt 7', captured['argv'][-1])
        self.assertNotIn('PSModulePath', captured['env'])
        self.assertEqual(json.loads(captured['env']['P2P_NODE_HOST_SETTINGS']), {'value': 'fixture'})

    def test_windows_powershell_missing_pwsh_fails_without_legacy_fallback(self):
        with patch.object(host.shutil, 'which', return_value=None) as choose, \
                patch.object(host, 'run') as invoke:
            with self.assertRaisesRegex(RuntimeError, r'PowerShell 7\+ \(pwsh\).*not found on PATH.*5\.1 is not selected'):
                host.powershell('Write-Output "not executed"')
        choose.assert_called_once_with('pwsh')
        invoke.assert_not_called()

    def test_mac_login_host_enables_managed_recovery_and_keeps_intentional_stop(self):
        import plistlib
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            service = root / 'LaunchAgent.plist'
            state = {'bin_dir': str(root / 'bin'), 'data_dir': str(root / 'data')}
            with patch.object(host.platform, 'system', return_value='Darwin'), patch.object(host.Path, 'home', return_value=root):
                host.service_write(state, service)
            value = plistlib.loads(service.read_bytes())
            self.assertTrue(value['RunAtLoad'])
            self.assertEqual(value['KeepAlive'], {'SuccessfulExit': False})
            self.assertIn('--managed', value['ProgramArguments'])
            self.assertEqual(value['ExitTimeOut'], 30)
            self.assertTrue(Path(value['StandardErrorPath']).parent.is_dir())
            self.assertFalse((root / 'data').exists())

    def test_incompatible_management_api_is_rejected_before_creating_version_directory(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            info = {'name':'p2p-agent','build_sha':'a'*40,'target':'fixture','version':'1','source_state':'clean','api_version':99}
            with patch.object(host, 'run', return_value=subprocess.CompletedProcess([], 0, json.dumps(info), '')):
                with self.assertRaisesRegex(RuntimeError,'same build'):
                    host.install(root,root/'installation',root/'data',False)
            self.assertFalse((root/'installation').exists())

    def test_dirty_install_requires_explicit_opt_in(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            info = {'build_sha':'a'*40,'target':'fixture','version':'1','source_state':'dirty','api_version':1}
            with patch.object(host,'run',return_value=subprocess.CompletedProcess([],0,json.dumps(info),'')):
                with self.assertRaisesRegex(RuntimeError,'dirty build'):
                    host.install(root,root/'installation',root/'data',False)

    def test_stop_requires_lock_proof_and_never_calls_forceful_platform_stop(self):
        calls=[]
        with patch.object(host,'response',side_effect=lambda state,args,check=False: calls.append(args)), patch.object(host,'wait_released') as released:
            host.stop({'fixture':True},None)
            released.assert_called_once()
        self.assertEqual(calls,[['stop']])

    def test_existing_immutable_version_cannot_be_overwritten_or_completed_silently(self):
        with tempfile.TemporaryDirectory() as root:
            root=Path(root); source=root/'source';source.mkdir()
            suffix='.exe' if os.name=='nt' else ''
            for name in ['p2p-agent','p2p-node']:
                (source/(name+suffix)).write_bytes(b'candidate')
            info={'build_sha':'b'*40,'target':'fixture','version':'1','source_state':'clean','api_version':1}
            with patch.object(host,'run',return_value=subprocess.CompletedProcess([],0,json.dumps(info),'')):
                installed=host.install(source,root/'install',root/'data',False)
                host.install(source,root/'install',root/'data',False)
                destination=Path(installed['bin_dir'])/('p2p-node'+suffix)
                destination.write_bytes(b'changed')
                with self.assertRaisesRegex(RuntimeError,'differs'):
                    host.install(source,root/'install',root/'data',False)
                self.assertEqual(destination.read_bytes(),b'changed')
                destination.unlink()
                with self.assertRaisesRegex(RuntimeError,'partial'):
                    host.install(source,root/'install',root/'data',False)
            self.assertFalse((root/'data').exists(),'install initialized identity/data')

    def test_same_sha_editions_install_separately_and_mixed_helpers_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            suffix = '.exe' if os.name == 'nt' else ''
            roles = ('p2p-agent', 'p2p-node')
            for edition in ('standard', 'files', 'mixed'):
                source = root / edition
                source.mkdir()
                for name in roles:
                    (source / (name + suffix)).write_text(f'{edition}:{name}', encoding='utf-8')
            legacy = {'build_sha': 'b' * 40, 'target': 'fixture', 'version': '1',
                      'source_state': 'clean', 'api_version': 1}

            def metadata(argv):
                binary = argv[0]
                name = binary.stem
                edition = binary.parent.name
                if edition == 'mixed':
                    edition = 'files' if name == 'p2p-agent' else 'standard'
                info = dict(legacy, name=name, edition=edition, files=edition == 'files')
                return subprocess.CompletedProcess(argv, 0, json.dumps(info), '')

            base = root / 'install'
            old = base / legacy['build_sha']
            old.mkdir(parents=True)
            data = root / 'data'
            with patch.object(host, 'run', side_effect=metadata):
                installed = [host.install(root / edition, base, data, False)
                             for edition in ('standard', 'files')]
                with self.assertRaisesRegex(RuntimeError, 'same build'):
                    host.install(root / 'mixed', base, data, False)
            self.assertEqual([Path(record['bin_dir']).name for record in installed],
                             [f"{legacy['build_sha']}-standard", f"{legacy['build_sha']}-files"])
            self.assertTrue(old.is_dir())
            self.assertFalse(data.exists())

    def test_config_preflight_rejection_leaves_previous_installation_running(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            data = root / 'node'
            data.mkdir()
            config = data / 'node.json'
            config.write_text('{"settings":{"enable_files":true}}', encoding='utf-8')
            old = {'bin_dir': str(root / 'old'), 'data_dir': str(data)}
            candidate = {'bin_dir': str(root / 'new'), 'data_dir': str(data),
                         'build': {'edition': 'standard'}}
            host.atomic_json(root / 'installed.json', old)
            argv = ['node-host.py', 'upgrade', '--bin-dir', str(root / 'candidate')]
            with patch.object(sys, 'argv', argv), \
                    patch.object(host.platform, 'system', return_value='Linux'), \
                    patch.object(host, 'locations', return_value=(root, None)), \
                    patch.object(host, 'response', return_value={'revision': 1}), \
                    patch.object(host, 'install', return_value=candidate), \
                    patch.object(host, 'run', side_effect=RuntimeError('candidate rejected config')) as run, \
                    patch.object(host, 'stop') as stop, \
                    patch.object(host, 'disable') as disable:
                with self.assertRaisesRegex(RuntimeError, 'candidate rejected config'):
                    host.main()
            run.assert_called_once_with([root / 'new/p2p-agent', '--data-dir', data, '--check-config'])
            stop.assert_not_called()
            disable.assert_not_called()
            self.assertEqual(json.loads((root / 'installed.json').read_text(encoding='utf-8')), old)
            self.assertFalse((root / 'previous-install.json').exists())

    def test_windows_upgrade_selects_unique_existing_task_and_keeps_extra_arguments(self):
        old={'bin_dir':r'C:\Users\glory\AppData\Local\p2p-file\bin\abc123',
             'data_dir':r'C:\Users\glory\AppData\Local\p2p-file\node'}
        preflight={'current_sid':'S-1-5-21-1','tasks':[{
            'name':'Veytrane-Agent-Boot','path':'\\','principal_sid':'S-1-5-21-1',
            'logon_type':'Password',
            'actions':[{'execute':r'C:\Users\glory\AppData\Local\p2p-file\bin\abc123\p2p-agent.exe',
                        'arguments':r'--data-dir "C:\Users\glory\AppData\Local\p2p-file\node" --managed',
                        'working_directory':r'C:\Users\glory\AppData\Local\p2p-file\bin\abc123'}]}]}
        selected=host.select_windows_startup_task(preflight,old)
        self.assertEqual(selected,{'name':'Veytrane-Agent-Boot','path':'\\','action_index':0})

    def test_windows_task_preflight_accepts_principal_sid(self):
        snapshot={'current_sid':'S-1-5-21-1','tasks':[]}
        with patch.object(host.platform,'system',return_value='Windows'), \
                patch.object(host,'powershell',return_value=subprocess.CompletedProcess(
                    [],0,json.dumps(snapshot),'')) as shell:
            self.assertEqual(host.windows_task_preflight({}),snapshot)
        script=shell.call_args.args[0]
        self.assertIn("if($principalUser -match '^S-\\d-\\d+(-\\d+)+$')",script)
        self.assertIn('New-Object System.Security.Principal.NTAccount($principalUser)',script)

    def test_windows_upgrade_discovers_exact_task_and_preserves_other_working_directory(self):
        old={'bin_dir':r'C:\install\old','data_dir':r'C:\data\node'}
        action={'execute':r'C:\install\old\p2p-agent.exe','arguments':r'--data-dir "C:\data\node"',
                'working_directory':None}
        base={'principal_sid':'S-1-5-21-1','logon_type':'Password','actions':[action]}
        with self.assertRaisesRegex(RuntimeError,'no Windows startup task'):
            host.select_windows_startup_task({'current_sid':'S-1-5-21-1','tasks':[]},old)
        ambiguous=[dict(base,name='first',path='\\'),dict(base,name='second',path='\\')]
        with self.assertRaisesRegex(RuntimeError,'ambiguous Windows startup task'):
            host.select_windows_startup_task({'current_sid':'S-1-5-21-1','tasks':ambiguous},old)
        selected=host.select_windows_startup_task(
            {'current_sid':'S-1-5-21-1','tasks':[dict(base,name='boot',path='\\')]},old)
        self.assertEqual(selected,{'name':'boot','path':'\\','action_index':0})

    def test_windows_upgrade_updates_only_existing_task_action(self):
        old={'bin_dir':r'C:\install\old','data_dir':r'C:\data\node'}
        candidate={'bin_dir':r'C:\install\new'}
        task={'name':'Veytrane-Agent-Boot','path':'\\','action_index':0}
        with patch.object(host,'powershell',return_value=subprocess.CompletedProcess(
                [],0,json.dumps({'name':'Veytrane-Agent-Boot','path':'\\','logon_type':'Password'}),'')) as shell:
            result=host.update_windows_task(task,old,candidate)
        self.assertEqual(result['logon_type'],'Password')
        script,settings=shell.call_args.args
        self.assertIn('Set-ScheduledTask -TaskName $s.name -TaskPath $s.path -Action $actions',script)
        self.assertNotIn('Register-ScheduledTask',script)
        self.assertNotIn('New-ScheduledTaskAction',script)
        self.assertNotIn('New-ScheduledTaskPrincipal',script)
        self.assertNotIn('-Password',script)
        self.assertNotIn('-Principal',script)
        self.assertNotIn('AtLogOn',script)
        self.assertNotIn('Interactive',script)
        self.assertNotIn('.Arguments =',script)
        self.assertIn('Export-ScheduledTask -TaskName $s.name -TaskPath $s.path',script)
        self.assertIn('$after.Task.Principals.OuterXml -ne $beforePrincipal',script)
        self.assertIn('$after.Task.Triggers.OuterXml -ne $beforeTriggers',script)
        self.assertIn('$after.Task.Settings.OuterXml -ne $beforeSettings',script)
        self.assertIn('another task action changed',script)
        self.assertIn('$updateWorkingDirectory=(Test-SamePath $actions[$actionIndex].WorkingDirectory $s.old_working_directory)',script)
        self.assertEqual(settings['new_agent'],r'C:\install\new\p2p-agent.exe')
        self.assertEqual(settings['new_working_directory'],r'C:\install\new')

    def test_windows_upgrade_update_failure_happens_before_stop(self):
        with tempfile.TemporaryDirectory() as root:
            root=Path(root);base=root/'bin';base.mkdir();data=root/'node';source=root/'candidate';source.mkdir()
            old={'schema':1,'bin_dir':str(base/'old'),'data_dir':str(data),
                 'build':{'build_sha':'a'*40,'source_state':'clean','api_version':1}}
            candidate={'schema':1,'bin_dir':str(base/'new'),'data_dir':str(data),
                       'build':{'build_sha':'b'*40,'source_state':'clean','api_version':1}}
            host.atomic_json(base/'installed.json',old)
            preflight={'current_sid':'S-1-5-21-1','tasks':[{
                'name':'Veytrane-Agent-Boot','path':'\\','principal_sid':'S-1-5-21-1','logon_type':'Password',
                'actions':[{'execute':str(Path(old['bin_dir'])/'p2p-agent.exe'),
                            'arguments':'--data-dir "'+old['data_dir']+'"',
                            'working_directory':old['bin_dir']}]}]}
            argv=['node-host.py','upgrade','--bin-dir',str(source)]
            events=[]
            def installed(*args):
                events.append('candidate')
                return candidate
            def previous_metadata(path,value):
                self.assertEqual(path,base/'previous-install.json')
                self.assertEqual(value,{'installation':old,'status':{'node':{'node_id':'node'},'revision':1}})
                events.append('previous')
            def blocked_update(*args):
                events.append('update')
                raise RuntimeError('task update blocked')
            with patch.object(sys,'argv',argv),patch.object(host,'locations',return_value=(base,None)), \
                    patch.object(host.platform,'system',return_value='Windows'), \
                    patch.object(host,'windows_task_preflight',return_value=preflight), \
                    patch.object(host,'install',side_effect=installed), \
                    patch.object(host,'response',return_value={'node':{'node_id':'node'},'revision':1}), \
                    patch.object(host,'atomic_json',side_effect=previous_metadata), \
                    patch.object(host,'update_windows_task',side_effect=blocked_update) as update, \
                    patch.object(host,'stop') as stop:
                with self.assertRaisesRegex(RuntimeError,'task update blocked'):
                    host.main()
            self.assertEqual(events,['candidate','previous','update'])
            update.assert_called_once()
            stop.assert_not_called()


def live_windows(binary_dir):
    if os.name!='nt':
        raise RuntimeError('--windows-live requires Windows')
    name=host.task_name()
    existing=host.powershell('$t=Get-ScheduledTask -TaskName $s.name -ErrorAction SilentlyContinue;if($t){"present"}else{"absent"}',{'name':name}).stdout.strip()
    if existing!='absent':
        raise RuntimeError('existing user startup task must be preserved; isolated live test refused')
    with tempfile.TemporaryDirectory(prefix='p2p-host-live-') as directory:
        root=Path(directory); data=root/'node';base=root/'bin'
        state=host.install(binary_dir.resolve(),base,data,True)
        node=Path(state['bin_dir'])/'p2p-node.exe'
        host.run([node,'--data-dir',data,'init'])
        enabled=False
        try:
            host.enable(state,None);enabled=True
            host.start(state,None)
            until=time.monotonic()+30
            status=None
            while time.monotonic()<until:
                status=host.response(state,['status'],check=False)
                if status:break
                time.sleep(.1)
            assert status and status['core_running'], 'scheduled Agent did not become ready'
            identity=status['node']['node_id']
            host.stop(state,None)
            host.start(state,None)
            until=time.monotonic()+30
            status=None
            while time.monotonic()<until:
                status=host.response(state,['status'],check=False)
                if status:break
                time.sleep(.1)
            assert status and status['node']['node_id']==identity
            host.stop(state,None)
            print(json.dumps({'result':'PASS','platform':'Windows','scenarios':['private installation','current-user Limited task','start','IPC stop and 3 locks','restart same identity'],'login_event':'not simulated'}))
        finally:
            host.stop(state,None)
            if enabled:
                host.disable(state,None)
                host.powershell('Unregister-ScheduledTask -TaskName $s.name -Confirm:$false',{'name':name})
            # Only disposable migration/task files inside our own temporary root.
            for path in root.rglob('*'):
                if path.is_file():path.chmod(0o600)


def upgrade_windows(old_dir, new_dir):
    """Exercise the shipped command-line installer, without touching user data."""
    if os.name != 'nt':
        raise RuntimeError('--windows-upgrade requires Windows')
    name = host.task_name()
    existing = host.powershell('$t=Get-ScheduledTask -TaskName $s.name -ErrorAction SilentlyContinue;if($t){"present"}else{"absent"}', {'name': name}).stdout.strip()
    if existing != 'absent':
        raise RuntimeError('existing user startup task must be preserved; isolated upgrade refused')
    script = Path(__file__).with_name('node-host.py').resolve()
    with tempfile.TemporaryDirectory(prefix='p2p-host-upgrade-') as directory:
        root = Path(directory)
        env = os.environ.copy()
        env['LOCALAPPDATA'] = str(root / 'appdata')
        data = root / 'node'
        def command(*args):
            return host.run([sys.executable, '-X', 'utf8', script, *args], env=env)
        state_file = root / 'appdata/p2p-file/bin/installed.json'
        state = None
        try:
            command('install', '--bin-dir', old_dir.resolve(), '--data-dir', data, '--allow-dirty')
            state = json.loads(state_file.read_text(encoding='utf-8'))
            host.run(host.cli(state) + ['init'])
            # Persist a nondefault setting before starting/upgrading.
            config_path = data / 'node.json'
            before_config = json.loads(config_path.read_text(encoding='utf-8'))
            candidate = root / 'candidate.json'
            settings = before_config['settings']
            settings.pop('remote_auth', None)
            settings['send_concurrency'] = 2
            candidate.write_text(json.dumps(settings), encoding='utf-8')
            host.run(host.cli(state) + ['offline-apply', '--revision', str(before_config['revision']), str(candidate)])
            command('enable')
            command('start')
            until = time.monotonic() + 30
            before = None
            while time.monotonic() < until:
                before = host.response(state, ['status'], check=False)
                if before:
                    break
                time.sleep(.1)
            assert before, 'old Agent did not become ready'
            config_bytes = config_path.read_bytes()
            result = json.loads(command('upgrade', '--bin-dir', new_dir.resolve()).stdout)
            state = json.loads(state_file.read_text(encoding='utf-8'))
            after = host.response(state, ['status'])
            assert result['upgraded'] and before['build']['build_sha'] != after['build']['build_sha']
            assert state['build']['source_state'] == 'clean', 'upgrade candidate was not a clean build'
            assert before['node']['node_id'] == after['node']['node_id']
            assert before['revision'] == after['revision']
            assert config_path.read_bytes() == config_bytes
            previous = json.loads(state_file.with_name('previous-install.json').read_text(encoding='utf-8'))
            assert previous['installation']['build']['build_sha'] == before['build']['build_sha']
            assert Path(previous['installation']['bin_dir']).is_dir()
            command('uninstall')
            assert config_path.read_bytes() == config_bytes, 'uninstall removed node data'
            assert not state_file.exists()
            assert Path(state['bin_dir']).is_dir(), 'uninstall removed immutable version binaries'
            print(json.dumps({'result': 'PASS', 'old_sha': before['build']['build_sha'],
                'new_sha': after['build']['build_sha'], 'scenarios': ['CLI install', 'scheduled start',
                'graceful upgrade', 'same identity/revision/config', 'previous version retained',
                'uninstall preserves node data and versions']}))
        finally:
            if state:
                host.stop(state, None)
            if host.powershell('$t=Get-ScheduledTask -TaskName $s.name -ErrorAction SilentlyContinue;if($t){"present"}else{"absent"}', {'name': name}).stdout.strip() == 'present':
                host.disable(state, None)
                host.powershell('Unregister-ScheduledTask -TaskName $s.name -Confirm:$false', {'name': name})
            for path in root.rglob('*'):
                if path.is_file():
                    path.chmod(0o600)


if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--windows-live',type=Path)
    parser.add_argument('--windows-upgrade', nargs=2, type=Path, metavar=('OLD_DIR', 'NEW_DIR'))
    args,remaining=parser.parse_known_args()
    if args.windows_upgrade:
        upgrade_windows(*args.windows_upgrade)
    elif args.windows_live:
        live_windows(args.windows_live)
    else:
        unittest.main(argv=[sys.argv[0]]+remaining)
