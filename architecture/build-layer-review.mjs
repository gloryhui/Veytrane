#!/usr/bin/env node
// Build one native terminal diagram, then replace only its PNG action with offline snapshots.
// Appearance switching stays in the skill's own toolbar; the light snapshot is exported from
// a copy whose data-appearance is changed without rebuilding the terminal theme.

import { execFileSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, rmdirSync, writeFileSync } from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const SPEC = join(HERE, 'layer-review.spec.json');
const OUT = join(HERE, 'layer-review.html');
const PNG_OUT = join(HERE, 'layer-review.png');
const SKILL = [
  join(homedir(), '.config/magpie/library/skills/arch-diagram'),
  join(homedir(), '.codex/skills/arch-diagram'),
].find(dir => existsSync(join(dir, 'scripts/build.mjs')));
if (!SKILL) throw new Error('arch-diagram skill not found');

const run = (script, args) => execFileSync(process.execPath, [script, ...args], { stdio: 'inherit' });
const json = value => JSON.stringify(value).replace(/</g, '\\u003c').replace(/\u2028/g, '\\u2028').replace(/\u2029/g, '\\u2029');
const dataUri = path => 'data:image/png;base64,' + readFileSync(path).toString('base64');

const STYLE = `<style>
#diagram-controls a.ctl-png{appearance:none;display:inline-flex;align-items:center;justify-content:center;gap:5px;min-width:32px;height:30px;padding:0 9px;border:1px solid transparent;border-radius:7px;background:transparent;color:var(--text);font:inherit;font-weight:600;cursor:pointer;text-decoration:none}
#diagram-controls a.ctl-png:hover{background:color-mix(in srgb,var(--text) 9%,transparent)}
#diagram-controls a.ctl-png:focus-visible{outline:2px solid var(--blue);outline-offset:1px}
</style>`;

const CLIENT = `<script>
(function () {
  var button = document.querySelector('#diagram-controls .ctl-png');
  // ?check / ?loop hide the native controls; leave the official check path untouched.
  if (!button) return;

  var data = JSON.parse(document.getElementById('layer-review-png').textContent);
  if (!data.dark || !data.light) return;

  var link = document.createElement('a');
  link.className = 'ctl-png';
  link.innerHTML = button.innerHTML;
  link.title = button.title || '导出 PNG';
  link.setAttribute('aria-label', link.title);
  button.replaceWith(link);

  function sync() {
    var mode = document.documentElement.dataset.appearance === 'light' ? 'light' : 'dark';
    link.href = data[mode];
    link.download = 'layer-review-' + mode + '.png';
  }

  new MutationObserver(sync).observe(document.documentElement, {
    attributes: true,
    attributeFilter: ['data-appearance']
  });
  sync();
})();
</script>`;

function addOfflinePng(html, png) {
  const tail = `${STYLE}
<script type="application/json" id="layer-review-png">${json(png)}</script>
${CLIENT}
`;
  const out = html.replace(/<\/body>\s*<\/html>\s*$/i, tail + '</body></html>');
  if (out === html) throw new Error('built HTML has no closing body');
  return out;
}

function main() {
  if (!existsSync(SPEC)) throw new Error(`spec not found: ${SPEC}`);

  const tmp = mkdtempSync(join(tmpdir(), 'layer-review-'));
  const base = join(tmp, 'terminal.html');
  const light = join(tmp, 'terminal-light.html');
  const darkPng = join(tmp, 'dark.png');
  const lightPng = join(tmp, 'light.png');
  const files = [base, light, darkPng, lightPng];

  try {
    run(join(SKILL, 'scripts/build.mjs'), [SPEC, base, '--theme', 'terminal']);

    const baseHtml = readFileSync(base, 'utf8');
    const lightHtml = baseHtml.replace(/(<html\b[^>]*\bdata-appearance=")dark(")/i, '$1light$2');
    if (lightHtml === baseHtml) throw new Error('could not set data-appearance="light"');
    writeFileSync(light, lightHtml);

    run(join(SKILL, 'scripts/export.mjs'), [base, darkPng, '--scale', '2']);
    run(join(SKILL, 'scripts/export.mjs'), [light, lightPng, '--scale', '2']);

    const png = { dark: dataUri(darkPng), light: dataUri(lightPng) };
    const html = addOfflinePng(baseHtml, png);
    writeFileSync(OUT, html);
    writeFileSync(PNG_OUT, Buffer.from(png.dark.split(',')[1], 'base64'));

    console.log(`✓ architecture/layer-review.html (${(Buffer.byteLength(html) / 1024 / 1024).toFixed(2)} MB, offline dark + light PNG)`);
    console.log('✓ architecture/layer-review.png (terminal, 2×)');
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
