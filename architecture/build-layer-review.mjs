#!/usr/bin/env node
// Build eight native theme documents and one offline viewer.
// Each selected theme/appearance is rendered by a fresh iframe srcdoc; the iframe's native
// toolbar is hidden, and this page provides the only theme/appearance/download controls.

import { execFileSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, rmdirSync, writeFileSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const SPEC = join(HERE, 'layer-review.spec.json');
const OUT = join(HERE, 'layer-review.html');
const PNG_OUT = join(HERE, 'layer-review.png');
const CLIENT = readFileSync(join(HERE, 'layer-review-client.js'), 'utf8');
const SHELL = readFileSync(join(HERE, 'layer-review-shell.css'), 'utf8');
const SKILL = [
  join(homedir(), '.config/magpie/library/skills/arch-diagram'),
  join(homedir(), '.codex/skills/arch-diagram'),
].find(dir => existsSync(join(dir, 'scripts/build.mjs')));
if (!SKILL) throw new Error('arch-diagram skill not found');

const THEMES = [
  { id: 'terminal', label: '终端' },
  { id: 'blueprint', label: '蓝图' },
  { id: 'drafting', label: '制图' },
  { id: 'whiteboard', label: '白板' },
  { id: 'brutalism', label: '粗野' },
  { id: 'crt', label: 'CRT' },
  { id: 'saas', label: '柔光' },
  { id: 'isometric', label: '等距' },
];

const run = (script, args) => execFileSync(process.execPath, [script, ...args], { stdio: 'inherit' });
const esc = value => String(value).replace(/[&<>"]/g, ch => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[ch]));
const json = value => JSON.stringify(value).replace(/</g, '\\u003c').replace(/\u2028/g, '\\u2028').replace(/\u2029/g, '\\u2029');
const dataUri = path => 'data:image/png;base64,' + readFileSync(path).toString('base64');
const appearanceOf = html => {
  const match = /<html\b[^>]*\bdata-appearance="(dark|light)"/i.exec(html);
  if (!match) throw new Error('built theme has no data-appearance');
  return match[1].toLowerCase();
};
const withAppearance = (html, appearance) => {
  const pattern = /(<html\b[^>]*\bdata-appearance=")(?:dark|light)(")/i;
  if (!pattern.test(html)) throw new Error('cannot replace data-appearance');
  return html.replace(pattern, '$1' + appearance + '$2');
};

const BRIDGE = `<script>
(function () {
  function send() {
    var fit = document.getElementById('fit');
    if (!fit) return;
    var style = getComputedStyle(document.documentElement);
    function value(name) { return style.getPropertyValue('--' + name).trim(); }
    parent.postMessage({
      type: 'lr-state',
      theme: document.documentElement.dataset.theme,
      appearance: document.documentElement.dataset.appearance,
      height: Math.ceil(fit.getBoundingClientRect().height + 48),
      palette: {
        bg: value('bg'),
        panel: value('panel'),
        text: value('text'),
        head: value('head'),
        dim: value('dim'),
        faint: value('faint'),
        blue: value('blue')
      }
    }, '*');
  }
  addEventListener('load', send);
  addEventListener('resize', send);
  if (window.ResizeObserver) {
    var fit = document.getElementById('fit');
    if (fit) new ResizeObserver(send).observe(fit);
  }
  setTimeout(send, 80);
  setTimeout(send, 500);
})();
</script>`;

function prepareDoc(html) {
  const withStyle = html.replace('</head>', '<style>#diagram-controls{display:none!important}</style></head>');
  if (withStyle === html) throw new Error('could not hide the iframe toolbar');
  const withBridge = withStyle.replace(/<\/body>\s*<\/html>\s*$/i, BRIDGE + '\n</body></html>');
  if (withBridge === withStyle) throw new Error('could not attach the iframe bridge');
  return withBridge;
}

function finalPage(spec, title, docs, png) {
  const options = THEMES.map(theme => `<option value="${theme.id}">${theme.label}</option>`).join('');
  const labels = Object.fromEntries(THEMES.map(theme => [theme.id, theme.label]));
  return `<!doctype html>
<html lang="${esc(spec.lang || 'zh-CN')}" data-theme="terminal" data-appearance="dark">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="color-scheme" content="dark light">
<title>${esc(title)}</title>
<style>${SHELL}</style>
</head>
<body>
<h1 class="sr-only">${esc(title)}</h1>
<header class="toolbar">
  <div class="toolbar-inner">
    <div class="brand"><i aria-hidden="true"></i><span>${esc(title)}</span></div>
    <div class="controls">
      <label class="field" for="theme-select"><span>主题样式</span><select id="theme-select">${options}</select></label>
      <div class="appearance" role="group" aria-label="亮暗配色">
        <button id="appearance-light" type="button" aria-pressed="false">亮色</button>
        <button id="appearance-dark" type="button" aria-pressed="true">暗色</button>
      </div>
      <a id="download" class="download" href="#" download="layer-review-terminal-dark.png" title="导出当前主题与亮暗配色的 2× PNG"><span aria-hidden="true">↓</span>导出 PNG</a>
    </div>
    <p id="status" class="status" role="status" aria-live="polite">终端 · 暗色</p>
  </div>
</header>
<main class="viewer"><iframe id="stage" title="架构图预览" sandbox="allow-scripts" scrolling="no"></iframe></main>
<noscript>此页面需要 JavaScript 才能渲染图表。</noscript>
<script type="application/json" id="layer-review-data">${json({ docs, png, labels })}</script>
<script>${CLIENT}</script>
</body>
</html>`;
}

function main() {
  if (!existsSync(SPEC)) throw new Error(`spec not found: ${SPEC}`);
  const spec = JSON.parse(readFileSync(SPEC, 'utf8'));
  const title = spec.pageTitle || String(spec.title || 'Layer review').replace(/\{\w+:([^{}]*)\}/g, '$1');
  const tmp = mkdtempSync(join(tmpdir(), 'layer-review-'));
  const files = [];
  const docs = {};
  const png = {};

  try {
    for (const theme of THEMES) {
      const base = join(tmp, `${theme.id}.html`);
      files.push(base);
      run(join(SKILL, 'scripts/build.mjs'), [SPEC, base, '--theme', theme.id]);

      const html = readFileSync(base, 'utf8');
      const native = appearanceOf(html);
      const other = native === 'dark' ? 'light' : 'dark';
      const variant = join(tmp, `${theme.id}-${other}.html`);
      const nativePng = join(tmp, `${theme.id}-${native}.png`);
      const otherPng = join(tmp, `${theme.id}-${other}.png`);
      files.push(variant, nativePng, otherPng);

      writeFileSync(variant, withAppearance(html, other));
      run(join(SKILL, 'scripts/export.mjs'), [base, nativePng, '--scale', '2']);
      run(join(SKILL, 'scripts/export.mjs'), [variant, otherPng, '--scale', '2']);

      docs[theme.id] = prepareDoc(html);
      png[theme.id] = {};
      png[theme.id][native] = dataUri(nativePng);
      png[theme.id][other] = dataUri(otherPng);
    }

    if (!png.terminal || !png.terminal.dark) throw new Error('terminal dark PNG is missing');
    const html = finalPage(spec, title, docs, png);
    writeFileSync(OUT, html);
    writeFileSync(PNG_OUT, Buffer.from(png.terminal.dark.split(',')[1], 'base64'));

    console.log(`✓ architecture/layer-review.html (${THEMES.length} themes × 2 appearances, ${(Buffer.byteLength(html) / 1024 / 1024).toFixed(1)} MB)`);
    console.log('✓ architecture/layer-review.png (terminal dark, 2×)');
  } finally {
    for (const file of files) {
      try { if (existsSync(file)) rmSync(file); } catch {}
    }
    try { rmdirSync(tmp); } catch {}
  }
}

try {
  main();
} catch (error) {
  console.error(`✗ ${error.message}`);
  process.exitCode = 1;
}
