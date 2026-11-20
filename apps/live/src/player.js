'use strict';
(() => {
  const video = document.querySelector('#video');
  const status = document.querySelector('#status');
  const stats = document.querySelector('#stats');
  const sound = document.querySelector('#sound');
  let current = null;
  let retryTimer;
  let generation = 0;
  let lastBytes = 0;
  let lastSample = 0;
  let lastFrameAt = performance.now();
  let lastFrames = 0;

  function release(session) {
    if (!session) return;
    session.abort.abort();
    session.pc.close();
    if (session.resource) fetch(session.resource, { method: 'DELETE', keepalive: true }).catch(() => {});
  }

  function retry(message) {
    status.textContent = message;
    clearTimeout(retryTimer);
    const session = current;
    current = null;
    generation++;
    release(session);
    video.srcObject = null;
    stats.textContent = '等待画面';
    retryTimer = setTimeout(connect, 2500);
  }

  async function gather(pc, signal) {
    if (pc.iceGatheringState === 'complete') return;
    await new Promise((resolve, reject) => {
      const finish = (err) => {
        clearTimeout(timer);
        pc.removeEventListener('icegatheringstatechange', changed);
        signal.removeEventListener('abort', aborted);
        err ? reject(err) : resolve();
      };
      const changed = () => { if (pc.iceGatheringState === 'complete') finish(); };
      const aborted = () => finish(new Error('cancelled'));
      const timer = setTimeout(() => finish(new Error('媒体连接准备超时')), 5000);
      pc.addEventListener('icegatheringstatechange', changed);
      signal.addEventListener('abort', aborted, { once: true });
    });
  }

  async function connect() {
    clearTimeout(retryTimer);
    const id = ++generation;
    release(current);
    const pc = new RTCPeerConnection({ iceServers: [], bundlePolicy: 'max-bundle' });
    const session = { pc, abort: new AbortController(), resource: null };
    current = session;
    const media = new MediaStream();
    video.srcObject = media;
    status.textContent = '正在连接直播…';
    lastBytes = lastSample = lastFrames = 0;
    lastFrameAt = performance.now();
    pc.addTransceiver('video', { direction: 'recvonly' });
    pc.addTransceiver('audio', { direction: 'recvonly' });
    pc.ontrack = ({ track, receiver }) => {
      if (id !== generation) return;
      media.addTrack(track);
      // Best effort only; support varies by browser. No intentional playout delay.
      if ('jitterBufferTarget' in receiver) {
        try { receiver.jitterBufferTarget = 0; } catch (_) {}
      }
      video.play().catch(() => { status.textContent = '已连接，点击播放器开始播放'; });
    };
    pc.onconnectionstatechange = () => {
      if (id !== generation) return;
      if (pc.connectionState === 'connected') {
        status.textContent = '直播已连接';
        lastFrameAt = performance.now();
      } else if (pc.connectionState === 'failed') {
        retry('连接中断，正在重试…');
      }
    };
    const deadline = setTimeout(() => session.abort.abort(), 15000);
    try {
      await pc.setLocalDescription(await pc.createOffer());
      await gather(pc, session.abort.signal);
      if (id !== generation) return;
      const response = await fetch('/live/whep', {
        method: 'POST', headers: { 'Content-Type': 'application/sdp' },
        body: pc.localDescription.sdp, signal: session.abort.signal,
      });
      const resource = response.headers.get('Location');
      if (resource) {
        const url = new URL(resource, location.href);
        if (url.origin !== location.origin || !/^\/live\/whep\/[a-zA-Z0-9-]+$/.test(url.pathname)) {
          throw new Error('观看服务地址无效');
        }
        session.resource = url.pathname;
      }
      if (id !== generation) { release(session); return; }
      if (response.status !== 201) throw new Error('等待主播开播，正在重试…');
      await pc.setRemoteDescription({ type: 'answer', sdp: await response.text() });
      // Also recover when negotiation succeeds but ICE never establishes.
      retryTimer = setTimeout(() => {
        if (id === generation && pc.connectionState !== 'connected') retry('媒体连接超时，正在重试…');
      }, 15000);
    } catch (err) {
      if (id === generation) retry(err.message === 'cancelled' ? '正在重新连接…' : err.message);
    } finally { clearTimeout(deadline); }
  }

  setInterval(async () => {
    const session = current;
    if (!session || session.pc.connectionState !== 'connected') return;
    try {
      const report = await session.pc.getStats();
      if (current !== session) return;
      for (const entry of report.values()) {
        if (entry.type !== 'inbound-rtp' || entry.kind !== 'video') continue;
        if (entry.framesDecoded > lastFrames) {
          lastFrames = entry.framesDecoded;
          lastFrameAt = performance.now();
        }
        const rate = lastSample ? Math.max(0, (entry.bytesReceived - lastBytes) * 8 / (entry.timestamp - lastSample) / 1000) : 0;
        lastBytes = entry.bytesReceived;
        lastSample = entry.timestamp;
        stats.textContent = `${video.videoWidth} × ${video.videoHeight} · ${Math.round(entry.framesPerSecond || 0)} fps · ${rate.toFixed(1)} Mbps`;
      }
      if (!video.paused && performance.now() - lastFrameAt > 12000) retry('直播画面中断，正在重连…');
    } catch (_) {}
  }, 1000);

  sound.addEventListener('click', () => {
    video.muted = !video.muted;
    sound.textContent = video.muted ? '开启声音' : '关闭声音';
    video.play().catch(() => {});
  });
  document.querySelector('#retry').addEventListener('click', connect);
  window.addEventListener('pagehide', () => {
    generation++;
    clearTimeout(retryTimer);
    release(current);
    current = null;
  });
  connect();
})();
