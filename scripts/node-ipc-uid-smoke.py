#!/usr/bin/env python3
"""Actual Unix peer-UID rejection, including a root client that bypasses file modes.

Run as root in an isolated local Linux test environment; Agent runs as nobody.
No accounts, firewall or persistent services are changed.
"""
import argparse
import json
import os
from pathlib import Path
import pwd
import signal
import socket
import struct
import subprocess
import tempfile
import time


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--agent',type=Path,required=True)
    parser.add_argument('--node',type=Path,required=True)
    args=parser.parse_args()
    if os.name!='posix' or os.geteuid()!=0:
        parser.error('requires an isolated Unix root test caller and the existing nobody account')
    account=pwd.getpwnam('nobody')
    with tempfile.TemporaryDirectory(prefix='node-ipc-uid-') as directory:
        parent=Path(directory);os.chown(parent,account.pw_uid,account.pw_gid)
        data=parent/'node'
        prefix=['runuser','-u','nobody','--','env','HOME='+str(parent/'home')]
        node=prefix+[str(args.node.resolve()),'--data-dir',str(data)]
        subprocess.run(node+['init'],check=True,capture_output=True,timeout=10)
        log=open(parent/'agent.log','w',encoding='utf-8')
        agent=subprocess.Popen(prefix+[str(args.agent.resolve()),'--data-dir',str(data)],stdout=log,stderr=log,start_new_session=True)
        try:
            until=time.monotonic()+15
            while True:
                result=subprocess.run(node+['status'],capture_output=True,timeout=5)
                if result.returncode==0:break
                assert time.monotonic()<until and agent.poll() is None,'Agent IPC not ready'
                time.sleep(.05)
            assert data.stat().st_mode & 0o777 == 0o700
            endpoint=data/'agent.sock'
            assert endpoint.stat().st_mode & 0o777 == 0o600
            # root can traverse 0700 and connect to 0600, so a successful rejection
            # here specifically proves the listener's peer-credential check.
            request=json.dumps({'api_version':1,'request_id':'different_uid','timeout_ms':1000,'command':{'method':'status'}}).encode()
            with socket.socket(socket.AF_UNIX,socket.SOCK_STREAM) as client:
                client.settimeout(5);client.connect(str(endpoint))
                try:
                    client.sendall(struct.pack('>I',len(request))+request)
                    received=client.recv(4)
                    assert not received,'cross-UID client received an IPC response'
                except (BrokenPipeError,ConnectionResetError):
                    pass
            subprocess.run(node+['status'],check=True,capture_output=True,timeout=5)
            subprocess.run(node+['stop'],check=True,capture_output=True,timeout=10)
            assert agent.wait(timeout=10)==0
            subprocess.run(node+['ownership-status'],check=True,capture_output=True,timeout=5)
            print(json.dumps({'result':'PASS','agent_uid':account.pw_uid,'rejected_client_uid':0,'directory_mode':'0700','socket_mode':'0600','same_uid_status_after_rejection':True,'graceful_exit_and_locks':True}))
        finally:
            if agent.poll() is None:
                subprocess.run(node+['stop'],capture_output=True,timeout=10)
                try:agent.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(agent.pid,signal.SIGTERM);agent.wait(timeout=5)
            log.close()


if __name__=='__main__':
    main()
