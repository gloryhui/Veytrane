#!/usr/bin/env node
// Build eight native theme documents and one offline viewer.

import { execFileSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, rmdirSync, writeFileSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const SPEC = join(HERE, 'layer-review.spec.json');
const OUT = join(HERE, 'layer-review.html');
const CLIENT = readFileSync(join(HERE, 'layer-review-client.js'), 'utf8');
const SHELL = readFileSync(join(HERE, 'layer-review-shell.css'), 'utf8');
const BLUEPRINT_LIGHT_CSS = `<style>
:root[data-theme="blueprint"][data-appearance="light"] {
  --bg: #d4e5fa;
  --panel: #e5effc;
  --dim: #3d587e;
  --faint: #7899c1;
  --cyan: #086577;
  --blue: #214f91;
  --green: #23633c;
  --purple: #593887;
  --amber: #784c0b;
  --orange: #8c4818;
  --red: #972d25;
  --flash: #bfd9fa;
}
</style>`;
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
  new MutationObserver(send).observe(document.documentElement, {
    attributeFilter: ['data-appearance']
  });
  if (window.ResizeObserver) {
    var fit = document.getElementById('fit');
    if (fit) new ResizeObserver(send).observe(fit);
  }
  setTimeout(send, 80);
  setTimeout(send, 500);
})();
</script>`;

function prepareDoc(html, theme) {
  const themeStyle = theme === 'blueprint' ? BLUEPRINT_LIGHT_CSS : '';
  const withStyle = html.replace('</head>', `${themeStyle}<style>#diagram-controls{display:none!important}</style></head>`);
  if (withStyle === html) throw new Error('could not hide the iframe toolbar');
  const withBridge = withStyle.replace(/<\/body>\s*<\/html>\s*$/i, BRIDGE + '\n</body></html>');
  if (withBridge === withStyle) throw new Error('could not attach the iframe bridge');
  return withBridge;
}

function finalPage(spec, title, docs) {
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
      <button id="download" class="download" type="button" disabled title="导出当前主题与亮暗配色的 2× PNG"><span aria-hidden="true">↓</span>导出 PNG</button>
    </div>
    <p id="status" class="status" role="status" aria-live="polite">终端 · 暗色</p>
  </div>
</header>
<main class="viewer"><iframe id="stage" title="架构图预览" scrolling="no"></iframe></main>
<noscript>此页面需要 JavaScript 才能渲染图表。</noscript>
<script type="application/json" id="layer-review-data">${json({ docs, labels })}</script>
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

  try {
    for (const theme of THEMES) {
      const base = join(tmp, `${theme.id}.html`);
      files.push(base);
      run(join(SKILL, 'scripts/build.mjs'), [SPEC, base, '--theme', theme.id]);

      docs[theme.id] = prepareDoc(readFileSync(base, 'utf8'), theme.id);
    }

    const html = finalPage(spec, title, docs);
    writeFileSync(OUT, html);

    console.log(`✓ architecture/layer-review.html (${THEMES.length} themes × 2 appearances, ${(Buffer.byteLength(html) / 1024 / 1024).toFixed(1)} MB)`);
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
