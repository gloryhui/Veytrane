#!/usr/bin/env python3
"""BUG-02: real A/B listeners, occupied C, runtime/API/disk transaction checks.

Windows uses the shared Web coordinator; --trigger sighup is Unix only.
"""
import argparse
import json
import os
from pathlib import Path
import signal
import socket
import tempfile
import time
from node_smoke import Fixture, exchange


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--mode', choices=['confirm-bug', 'verify-fix'], required=True)
    parser.add_argument('--trigger', choices=['web', 'sighup'], default='web')
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    if args.mode == 'confirm-bug' and args.trigger != 'sighup':
        parser.error('BUG-02 baseline is a SIGHUP defect; Web has its own rollback and cannot substitute baseline reproduction')
    if args.trigger == 'sighup' and os.name == 'nt':
        parser.error('SIGHUP is not applicable on Windows; use Unix for actual signal evidence')
    output = args.output or Path(tempfile.mkdtemp(prefix='config-apply-evidence-'))
    with Fixture(args.binary, output) as fixture:
        fixture.start()
        with socket.socket() as occupied, fixture.connect(fixture.a) as old_stream:
            exchange(old_stream, b'old-A-before-update')
            occupied.bind(('127.0.0.1', 0)); occupied.listen()
            c = occupied.getsockname()[1]
            candidate = {'version':1,'rules':[fixture.rule('b', fixture.b), fixture.rule('c', c)]}
            before = (fixture.root / 'client.json').read_bytes()
            if args.trigger == 'web':
                code, error = fixture.api('client', 'config', 'PUT', candidate)
                assert code >= 400, error
            else:
                fixture.write('client.json', candidate)
                os.kill(fixture.client_process.pid, signal.SIGHUP)
                time.sleep(.5)
            _, status = fixture.api('client', 'status')
            listeners = {row['id'] for row in status['listeners']}
            if args.mode == 'confirm-bug':
                assert listeners == {'b'} and {row['id'] for row in status['rules']} == {'a','b'} and not status['last_error'], status
            else:
                assert listeners == {'a','b'} and status['last_error'], status
                fixture.request_echo(fixture.a); fixture.request_echo(fixture.b)
                exchange(old_stream, b'old-A-survives-failed-update')
                if args.trigger == 'web':
                    assert (fixture.root / 'client.json').read_bytes() == before
                occupied.close()
                code, applied = fixture.api('client', 'config', 'PUT', candidate)
                assert code == 200, applied
                fixture.request_echo(fixture.b); fixture.request_echo(c)
                exchange(old_stream, b'removed-listener-does-not-kill-accepted-A')
                _, status = fixture.api('client', 'status')
                assert {r['id'] for r in status['listeners']} == {'b','c'} and status['last_error'] is None, status
                assert json.loads((fixture.root/'client.json').read_text(encoding='utf-8')) == candidate
            result={'bug':'BUG-02','mode':args.mode,'trigger':args.trigger,'result':'PASS','failed_candidate_listeners':sorted(listeners),'sighup_applicable':os.name!='nt'}
            (output/'result.json').write_text(json.dumps(result,indent=2)+'\n',encoding='utf-8')
            print(json.dumps(result))


if __name__ == '__main__':
    main()
