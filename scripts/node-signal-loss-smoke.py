#!/usr/bin/env python3
"""BUG-01: stop ONLY signaling, keep a Direct TCP stream and new requests alive."""
import argparse
import json
from pathlib import Path
import tempfile
import time
from node_smoke import Fixture, exchange


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', required=True, type=Path)
    parser.add_argument('--mode', required=True, choices=['confirm-bug', 'verify-fix'])
    parser.add_argument('--output', type=Path, default=None)
    parser.add_argument('--path', choices=['direct','relay'], default='direct')
    args = parser.parse_args()
    output = args.output or Path(tempfile.mkdtemp(prefix='signal-loss-evidence-'))
    with Fixture(args.binary, output,relay=args.path=='relay') as fixture:
        fixture.start()
        with fixture.connect(fixture.a) as stream:
            exchange(stream, b'before-signal-stop')
            fixture.signal.terminate()
            fixture.signal.wait(timeout=5)
            count, failure = 0, None
            for seq in range(40):
                try:
                    exchange(stream, f'sequence-{seq:04d}'.encode())
                    fixture.request_echo(fixture.b)
                    count += 1
                except (OSError, ConnectionError) as error:
                    failure = type(error).__name__
                    break
                time.sleep(.2)
            assert fixture.server_process.poll() is None and fixture.client_process.poll() is None
            if args.mode == 'confirm-bug':
                assert failure, 'old failure was NOT observed; this is not baseline evidence'
            else:
                assert failure is None and count == 40, f'healthy Direct business interrupted after {count} samples: {failure}'
                fixture.start_signal('signal-restored')
                fixture.wait_log('server', '信令已使用原身份重新登记', timeout=40)
                exchange(stream, b'after-signal-restored')
                fixture.request_echo(fixture.a)
        if args.mode=='verify-fix':fixture.restart_client()
        result = {'bug':'BUG-01','mode':args.mode,'path':args.path,'result':'PASS','samples_after_signal_stop':count,'observed_failure':failure,'signal_restored':args.mode=='verify-fix','new_session_after_restore':args.mode=='verify-fix'}
        (output / 'result.json').write_text(json.dumps(result, indent=2)+'\n', encoding='utf-8')
        print(json.dumps(result))


if __name__ == '__main__':
    main()
