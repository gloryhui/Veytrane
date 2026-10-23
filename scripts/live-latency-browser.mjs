// Operator-only Playwright measurement, never included in the viewer package.
// npm install --prefix <test-tools> playwright-core
// LIVE_PLAYWRIGHT_MODULE=<absolute playwright-core/index.mjs> node this-file.mjs
import fs from 'node:fs/promises';
import { pathToFileURL } from 'node:url';

const modulePath = process.env.LIVE_PLAYWRIGHT_MODULE;
if (!modulePath) throw new Error('Set LIVE_PLAYWRIGHT_MODULE to playwright-core/index.mjs');
const { chromium } = await import(pathToFileURL(modulePath).href);
const browser = await chromium.launch({
  executablePath: process.env.LIVE_CHROME || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  headless: true,
  args: ['--autoplay-policy=no-user-gesture-required', '--no-proxy-server'],
});
try {
  const page = await browser.newPage();
  await page.addInitScript(() => {
    const Original = window.RTCPeerConnection;
    window.RTCPeerConnection = class extends Original {
      constructor(...args) {
        super(...args);
        window.liveTestPeer = this;
      }
    };
  });
  const messages = [];
  page.on('pageerror', e => messages.push(e.message));
  await page.goto(process.env.LIVE_URL || 'http://127.0.0.1:18889/');
  await page.waitForFunction(() => document.querySelector('video').videoWidth === 640, null, { timeout: 60000 });
  await page.waitForTimeout(3000);
  const measurement = await page.evaluate(async () => {
    const video = document.querySelector('video');
    const canvas = document.createElement('canvas');
    canvas.width = 640;
    canvas.height = 360;
    const context = canvas.getContext('2d', { willReadFrequently: true });
    const samples = [];
    let rejected = 0;
    let previousTimestamp;
    const started = performance.now();
    await new Promise(resolve => {
      const frame = () => {
        context.drawImage(video, 0, 0, 640, 360);
        const pixels = context.getImageData(0, 16, 640, 1).data;
        const bytes = [];
        for (let byte = 0; byte < 5; byte++) {
          let value = 0;
          for (let bit = 0; bit < 8; bit++) {
            const x = (byte * 8 + bit) * 16 + 8;
            value = value * 2 + (pixels[x * 4] > 128 ? 1 : 0);
          }
          bytes.push(value);
        }
        const valid = ((bytes[0] + bytes[1] + bytes[2] + bytes[3]) & 255) === bytes[4];
        const timestamp = bytes.slice(0, 4).reduce((a, b) => a * 256 + b, 0);
        const delay = ((Date.now() % 4294967296) - timestamp + 4294967296) % 4294967296;
        if (valid && delay < 10000 && timestamp !== previousTimestamp) {
          samples.push(delay);
          previousTimestamp = timestamp;
        } else { rejected++; }
        if (performance.now() - started >= 20000) resolve();
        else video.requestVideoFrameCallback(frame);
      };
      video.requestVideoFrameCallback(frame);
      setTimeout(resolve, 25000);
    });
    const report = await window.liveTestPeer.getStats();
    const transport = [...report.values()].find(s => s.type === 'transport' && s.selectedCandidatePairId);
    const pair = transport ? report.get(transport.selectedCandidatePairId) : null;
    const remote = pair ? report.get(pair.remoteCandidateId) : null;
    const inbound = [...report.values()].filter(s => s.type === 'inbound-rtp').map(s => ({
      kind: s.kind, framesDecoded: s.framesDecoded, framesDropped: s.framesDropped,
      bytesReceived: s.bytesReceived, jitter: s.jitter,
    }));
    samples.sort((a, b) => a - b);
    const percentile = q => samples.length ? samples[Math.min(samples.length - 1, Math.floor(samples.length * q))] : null;
    return {
      samples: samples.length, rejected,
      min_ms: samples[0] ?? null, median_ms: percentile(0.5), p95_ms: percentile(0.95), max_ms: samples.at(-1) ?? null,
      selected_remote_candidate: remote ? { address: remote.address, port: remote.port, protocol: remote.protocol, tcpType: remote.tcpType } : null,
      inbound, status: document.querySelector('#status').textContent,
    };
  });
  measurement.page_errors = messages;
  measurement.scope = 'same-machine source -> FFmpeg WHIP -> official MediaMTX -> Veytrane QUIC/TCP -> Chrome WebRTC playout; shared host clock';
  if (measurement.samples < 100 || measurement.selected_remote_candidate?.protocol !== 'tcp' || measurement.selected_remote_candidate?.port !== 18189) {
    throw new Error(`Latency test did not validate TCP video path: ${JSON.stringify(measurement)}`);
  }
  if (process.env.LIVE_RECOVERY_CONTROL) {
    const previousSessions = await page.evaluate(() => window.liveTestPeer.remoteDescription.sdp);
    const started = Date.now();
    await fs.writeFile(process.env.LIVE_RECOVERY_CONTROL, 'restart-publisher\n');
    await page.waitForFunction(old => {
      const pc = window.liveTestPeer;
      const video = document.querySelector('video');
      return pc?.connectionState === 'connected' && pc.remoteDescription?.sdp !== old && video.videoWidth === 640 && video.currentTime > 0;
    }, previousSessions, { timeout: 45000 });
    measurement.publisher_restart_recovery_ms = Date.now() - started;
    measurement.recovery_without_page_reload = true;
  }
  if (process.env.LIVE_RESULT) await fs.writeFile(process.env.LIVE_RESULT, JSON.stringify(measurement, null, 2) + '\n');
  console.log(JSON.stringify(measurement, null, 2));
} finally { await browser.close(); }
