(function () {
  'use strict';

  var data = JSON.parse(document.getElementById('layer-review-data').textContent);
  var root = document.documentElement;
  var select = document.getElementById('theme-select');
  var iframe = document.getElementById('stage');
  var lightButton = document.getElementById('appearance-light');
  var darkButton = document.getElementById('appearance-dark');
  var download = document.getElementById('download');
  var status = document.getElementById('status');
  var state = { theme: 'terminal', appearance: 'dark' };

  function isAppearance(value) {
    return value === 'light' || value === 'dark';
  }

  function hasTheme(theme) {
    return typeof theme === 'string' && Object.hasOwn(data.docs, theme);
  }

  function labelFor(theme) {
    return Object.hasOwn(data.labels, theme) ? data.labels[theme] : theme;
  }

  function docFor(theme, appearance) {
    var html = data.docs[theme];
    if (typeof html !== 'string') return '';
    return html.replace(
      /(<html\b[^>]*\bdata-appearance=")(?:dark|light)(")/i,
      '$1' + appearance + '$2'
    );
  }

  function applyPalette(palette) {
    var map = {
      bg: '--ui-bg',
      panel: '--ui-panel',
      text: '--ui-text',
      head: '--ui-head',
      dim: '--ui-dim',
      faint: '--ui-faint',
      blue: '--ui-accent'
    };
    for (var key in map) {
      if (palette && palette[key]) root.style.setProperty(map[key], palette[key]);
    }
  }

  function sync() {
    var appearanceLabel = state.appearance === 'light' ? '亮色' : '暗色';
    var label = labelFor(state.theme) + ' · ' + appearanceLabel;
    root.setAttribute('data-theme', state.theme);
    root.setAttribute('data-appearance', state.appearance);
    select.value = state.theme;
    lightButton.setAttribute('aria-pressed', String(state.appearance === 'light'));
    darkButton.setAttribute('aria-pressed', String(state.appearance === 'dark'));
    status.textContent = label;
    iframe.title = label + ' 架构图';
    var png = data.png[state.theme] && data.png[state.theme][state.appearance];
    download.href = png || '#';
    download.download = 'layer-review-' + state.theme + '-' + state.appearance + '.png';
    download.title = '导出 ' + label + ' 的 2× PNG';
    try {
      localStorage.setItem('layer-review-theme', state.theme);
      localStorage.setItem('layer-review-appearance', state.appearance);
    } catch (error) {}
  }

  function render() {
    sync();
    iframe.srcdoc = docFor(state.theme, state.appearance);
  }

  function setTheme(theme) {
    if (!hasTheme(theme)) return;
    state.theme = theme;
    render();
  }

  function setAppearance(appearance) {
    if (!isAppearance(appearance)) return;
    state.appearance = appearance;
    render();
  }

  select.addEventListener('change', function () {
    setTheme(select.value);
  });
  lightButton.addEventListener('click', function () {
    setAppearance('light');
  });
  darkButton.addEventListener('click', function () {
    setAppearance('dark');
  });

  window.addEventListener('message', function (event) {
    var message = event.data;
    if (!message || message.type !== 'lr-state') return;
    if (event.source !== iframe.contentWindow) return;
    if (message.theme !== state.theme || message.appearance !== state.appearance) return;
    var height = Number(message.height);
    if (isFinite(height)) {
      iframe.style.height = Math.max(240, Math.min(40000, Math.round(height))) + 'px';
    }
    applyPalette(message.palette);
  });

  var params = new URLSearchParams(location.search);
  var savedTheme = null;
  var savedAppearance = null;
  try {
    savedTheme = localStorage.getItem('layer-review-theme');
    savedAppearance = localStorage.getItem('layer-review-appearance');
  } catch (error) {}

  var requestedTheme = params.get('theme');
  var requestedAppearance = params.get('appearance');
  state.theme = hasTheme(requestedTheme) ? requestedTheme : (hasTheme(savedTheme) ? savedTheme : 'terminal');
  state.appearance = isAppearance(requestedAppearance) ? requestedAppearance : (isAppearance(savedAppearance) ? savedAppearance : 'dark');
  render();
})();
