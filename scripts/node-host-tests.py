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
