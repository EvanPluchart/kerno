// Page behaviour: theme, the page's own status bar, command palette, tabs, copy, screenshots.
import { initDemo } from './demo.js';
import { initMillion } from './grid.js';

const root = document.documentElement;
window.kernoReady = true;
const $ = (selector, scope = document) => scope.querySelector(selector);
const $$ = (selector, scope = document) => [...scope.querySelectorAll(selector)];
const RELEASES = 'https://github.com/EvanPluchart/kerno/releases/latest';
const SOURCE = 'https://github.com/EvanPluchart/kerno';

// ── theme ──
function setTheme(theme, remember) {
  root.dataset.theme = theme;
  if (remember) {
    try {
      localStorage.setItem('kerno-theme', theme);
    } catch {
      // private mode: the choice lasts for this page view
    }
  }
  $('#theme-toggle').setAttribute('aria-label', `Switch to the ${theme === 'dark' ? 'light' : 'dark'} theme`);
  const swap = (url) => url.replace(/-(dark|light)\.webp$/, `-${theme}.webp`);
  for (const image of $$('img[data-themed]')) image.src = swap(image.getAttribute('src'));
  for (const link of $$('a[data-shot]')) link.href = swap(link.getAttribute('href'));
}
const toggleTheme = () => setTheme(root.dataset.theme === 'dark' ? 'light' : 'dark', true);
setTheme(root.dataset.theme, false);
$('#theme-toggle').addEventListener('click', toggleTheme);
matchMedia('(prefers-color-scheme: light)').addEventListener('change', (event) => {
  let stored = null;
  try {
    stored = localStorage.getItem('kerno-theme');
  } catch {
    // ignore
  }
  if (!stored) setTheme(event.matches ? 'light' : 'dark', false);
});

// Ctrl instead of ⌘ away from Apple keyboards.
if (!/Mac|iPhone|iPad/.test(navigator.platform)) {
  for (const key of $$('.mod')) key.textContent = 'Ctrl ';
}

// ── the live window and the million-row grid ──
const demo = initDemo({
  onStatus({ rows, ms }) {
    $('#sb-rows').textContent = rows;
    $('#sb-ms').textContent = ms;
  },
});
initMillion();

// ── tiles settle into the board as they come into view ──
if ('IntersectionObserver' in window) {
  const reveal = new IntersectionObserver(
    (entries) => {
      entries
        .filter((entry) => entry.isIntersecting)
        .forEach((entry, index) => {
          const tile = entry.target;
          tile.style.setProperty('--i', index);
          tile.classList.add('in');
          reveal.unobserve(tile);
          // Hand the transition back to the tile's own hover styles once it has landed.
          setTimeout(() => tile.removeAttribute('data-reveal'), 1400);
        });
    },
    { rootMargin: '0px 0px -8% 0px' },
  );
  for (const tile of $$('[data-reveal]')) reveal.observe(tile);

  const here = new IntersectionObserver(
    (entries) => {
      const entry = entries.find((candidate) => candidate.isIntersecting);
      if (entry) $('#sb-section').textContent = entry.target.dataset.section;
    },
    { rootMargin: '-45% 0px -45% 0px' },
  );
  for (const section of $$('[data-section]')) here.observe(section);
} else {
  for (const tile of $$('[data-reveal]')) tile.classList.add('in');
}

// ── command palette ──
const palette = $('#palette');
const paletteInput = $('#palette-input');
const paletteList = $('#palette-list');
const go = (hash) => () => {
  $(hash).scrollIntoView();
  history.replaceState(null, '', hash);
};
const COMMANDS = [
  ['Go to the live demo', 'Section', go('#demo')],
  ['Go to the tour', 'Section', go('#tour')],
  ['Go to performance', 'Section', go('#performance')],
  ['Go to privacy', 'Section', go('#privacy')],
  ['Go to install', 'Section', go('#install')],
  ['Run the query in the window', 'Demo', () => { go('#demo')(); demo.run(); }],
  ['Connect to Local', 'Demo', () => demo.setEnv('local')],
  ['Connect to Staging', 'Demo', () => demo.setEnv('staging')],
  ['Connect to Production', 'Demo', () => demo.setEnv('production')],
  ['Switch between light and dark', 'Page', toggleTheme],
  ['Download Kerno', 'GitHub', () => { location.href = RELEASES; }],
  ['Read the source', 'GitHub', () => { location.href = SOURCE; }],
];
let matches = [];
let active = 0;

function renderPalette() {
  const query = paletteInput.value.trim().toLowerCase();
  matches = COMMANDS.filter(([label, group]) => `${label} ${group}`.toLowerCase().includes(query));
  active = Math.min(active, Math.max(matches.length - 1, 0));
  paletteList.replaceChildren(
    ...matches.map(([label, group], index) => {
      const item = document.createElement('li');
      item.id = `cmd-${index}`;
      item.setAttribute('role', 'option');
      item.setAttribute('aria-selected', String(index === active));
      item.append(Object.assign(document.createElement('span'), { textContent: label }));
      item.append(Object.assign(document.createElement('small'), { textContent: group }));
      item.addEventListener('click', () => choose(index));
      return item;
    }),
  );
  if (!matches.length) {
    paletteList.append(Object.assign(document.createElement('li'), { className: 'empty', textContent: 'No command matches. Try "install" or "production".' }));
  }
  paletteInput.setAttribute('aria-activedescendant', matches.length ? `cmd-${active}` : '');
  $(`#cmd-${active}`)?.scrollIntoView({ block: 'nearest' });
}
function choose(index) {
  const command = matches[index];
  if (!command) return;
  palette.close();
  command[2]();
}
function openPalette() {
  if (palette.open) return palette.close();
  for (const other of $$('dialog[open]')) other.close();
  paletteInput.value = '';
  active = 0;
  renderPalette();
  palette.showModal();
}
paletteInput.addEventListener('input', () => {
  active = 0;
  renderPalette();
});
paletteInput.addEventListener('keydown', (event) => {
  if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
    event.preventDefault();
    const step = event.key === 'ArrowDown' ? 1 : -1;
    active = (active + step + matches.length) % Math.max(matches.length, 1);
    renderPalette();
  } else if (event.key === 'Enter') {
    event.preventDefault();
    choose(active);
  }
});
for (const button of $$('[data-palette]')) button.addEventListener('click', openPalette);
document.addEventListener('keydown', (event) => {
  if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === 'k') {
    event.preventDefault();
    openPalette();
  }
});
// A click on a dialog's backdrop closes it (the palette and the screenshot viewer).
for (const dialog of [palette, $('#shot')]) {
  dialog.addEventListener('click', (event) => {
    if (event.target === dialog) dialog.close();
  });
}

// ── screenshots open at full size ──
for (const link of $$('a[data-shot]')) {
  link.addEventListener('click', (event) => {
    if (event.metaKey || event.ctrlKey || event.shiftKey) return; // let "open in a new tab" through
    event.preventDefault();
    const image = $('#shot-img');
    image.src = link.href;
    image.alt = $('img', link).alt;
    $('#shot').showModal();
  });
}

// ── install tabs ──
const tabs = $$('[role="tab"]');
function selectTab(tab, focus) {
  for (const other of tabs) {
    const on = other === tab;
    other.setAttribute('aria-selected', String(on));
    other.tabIndex = on ? 0 : -1;
    $(`#${other.getAttribute('aria-controls')}`).hidden = !on;
  }
  if (focus) tab.focus();
}
tabs.forEach((tab, index) => {
  tab.addEventListener('click', () => selectTab(tab));
  tab.addEventListener('keydown', (event) => {
    const step = { ArrowRight: 1, ArrowLeft: -1 }[event.key];
    if (step) selectTab(tabs[(index + step + tabs.length) % tabs.length], true);
  });
});
// Linux visitors land on the Linux tab.
if (/Linux/.test(navigator.platform) && !/Android/.test(navigator.userAgent)) selectTab($('#tab-linux'));

// ── copy buttons ──
for (const button of $$('.copy')) {
  button.addEventListener('click', async () => {
    const code = $('code', button.parentElement);
    try {
      await navigator.clipboard.writeText(code.textContent);
      button.textContent = 'Copied';
    } catch {
      getSelection().selectAllChildren(code);
      button.textContent = 'Selected: copy it';
    }
    setTimeout(() => {
      button.textContent = 'Copy';
    }, 1600);
  });
}
