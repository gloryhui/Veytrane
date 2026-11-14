#!/usr/bin/env python3
"""Isolated macOS/Linux MediaMTX + WHIP + real P2P/TCP browser smoke test.

Requires debug live-gateway/live-viewer/p2p_file, FFmpeg with WHIP, Chrome and
playwright-core. Never starts or reconfigures installed production services.
"""
import argparse
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--mediamtx', type=Path, required=True)
    parser.add_argument('--playwright-module', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--bin-dir', type=Path)
    parser.add_argument('--chrome', default='/Applications/Google Chrome.app/Contents/MacOS/Google Chrome')
    args = parser.parse_args()
    repo = Path(__file__).resolve().parents[1]
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    binaries = (args.bin_dir or repo / 'target/debug').resolve()
    for port in [18889, 18890, 18891, 18189, 18190, 28900]:
        with socket.socket() as probe:
            probe.bind(('127.0.0.1', port))
    processes = []
    logs = []

    def start(name, command, **kwargs):
        log = open(output / (name + '.log'), 'wb')
        logs.append(log)
        process = subprocess.Popen([str(p) for p in command], cwd=repo,
                                   stdout=log, stderr=log, start_new_session=True, **kwargs)
        processes.append(process)
        return process

    def wait_http(url, expected=None, timeout=45):
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        until = time.monotonic() + timeout
        while time.monotonic() < until:
            for p in processes:
                if p.poll() is not None:
                    raise RuntimeError('A test child exited; inspect logs in ' + str(output))
            try:
                with opener.open(url, timeout=2) as response:
                    body = response.read()
                if expected is None or expected in body:
                    return
            except (OSError, urllib.error.URLError):
                pass
            time.sleep(0.25)
        raise TimeoutError(url)

    try:
        start('mediamtx', [args.mediamtx.resolve(), repo / 'packaging/live/mediamtx.yml'])
        start('signal', [binaries / 'p2p_file', 'signal-server', '--listen', '127.0.0.1:28900',
                         '--short-id-db', output / 'signal.sqlite3'])
        data = output / 'gateway'
        subprocess.run([binaries / 'live-gateway', '--data-dir', data, '--init',
                        '--signal-server', '127.0.0.1:28900'], check=True, stdout=subprocess.DEVNULL)
        start('gateway', [binaries / 'live-gateway', '--data-dir', data, '--loopback-test'])
        start('viewer', [binaries / 'live-viewer', '--profile', data / 'live-viewer.json',
                         '--data-dir', output / 'viewer', '--loopback-test', '--no-browser'])
        wait_http('http://127.0.0.1:18889/healthz', b'Veytrane Live')
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        for method, path in [('POST', '/live/whip'), ('GET', '/v3/paths/list')]:
            request = urllib.request.Request('http://127.0.0.1:18889' + path, method=method)
            try:
                opener.open(request, timeout=5)
                raise AssertionError('Viewer unexpectedly accessed ' + path)
            except urllib.error.HTTPError as e:
                assert e.code == 404, (path, e.code)
        raw = subprocess.Popen([sys.executable, repo / 'scripts/live-latency-source.py'],
                               stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, start_new_session=True)
        processes.append(raw)
        publisher_command = ['ffmpeg', '-hide_banner', '-loglevel', 'warning',
                           '-f', 'rawvideo', '-pixel_format', 'rgb24', '-video_size', '640x360',
                           '-framerate', '30', '-i', 'pipe:0', '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000',
                           '-c:v', 'libx264', '-preset', 'ultrafast', '-tune', 'zerolatency', '-bf', '0',
                           '-g', '30', '-profile:v', 'baseline', '-pix_fmt', 'yuv420p',
                           '-b:v', '2500k', '-minrate', '2500k', '-maxrate', '2500k', '-bufsize', '2500k',
                           '-x264-params', 'nal-hrd=cbr:force-cfr=1',
                           '-c:a', 'libopus', '-ar', '48000', '-ac', '2', '-b:a', '96k',
                           '-f', 'whip', 'http://127.0.0.1:18891/live/whip']
        publisher = start('publisher', publisher_command, stdin=raw.stdout)
        raw.stdout.close()
        time.sleep(2)
        env = {**os.environ, 'LIVE_PLAYWRIGHT_MODULE': str(args.playwright_module.resolve()),
               'LIVE_CHROME': args.chrome, 'LIVE_RESULT': str(output / 'latency.json'),
               'LIVE_RECOVERY_CONTROL': str(output / 'recovery-control.txt')}
        browser = start('browser', ['node', repo / 'scripts/live-latency-browser.mjs'], env=env)
        until = time.monotonic() + 120
        restarted = False
        while browser.poll() is None and time.monotonic() < until:
            if (output / 'recovery-control.txt').exists() and not restarted:
                os.killpg(publisher.pid, signal.SIGINT)
                publisher.wait(timeout=8)
                raw.wait(timeout=8)
                time.sleep(3)
                raw = subprocess.Popen([sys.executable, repo / 'scripts/live-latency-source.py'],
                                       stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, start_new_session=True)
                processes.append(raw)
                start('publisher-restarted', publisher_command, stdin=raw.stdout)
                raw.stdout.close()
                restarted = True
            time.sleep(0.2)
        if browser.poll() is None:
            raise TimeoutError('Browser smoke test timed out')
        if browser.returncode:
            raise RuntimeError('Browser test failed; inspect ' + str(output / 'browser.log'))
        measurement = json.loads((output / 'latency.json').read_text())
        assert restarted and measurement['recovery_without_page_reload']
        print(json.dumps(measurement, indent=2))
        # Transport evidence is independent of Chrome's ICE candidate evidence.
        gateway_log = (output / 'gateway.log').read_text()
        if '已建立 直连' not in gateway_log:
            raise RuntimeError('No direct P2P transport evidence in gateway log')
        print('Confirmed direct P2P + browser ICE/TCP. Test output:', output)
    finally:
        for process in reversed(processes):
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGINT)
                try:
                    process.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=5)
        for log in logs:
            log.close()


if __name__ == '__main__':
    main()
