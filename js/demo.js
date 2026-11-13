// The live window: a small Kerno, running real SQL on PGlite (PostgreSQL in WebAssembly).
// PGlite is fetched on the visitor's first interaction; until then, and if it cannot
// load, the window shows results saved by tools/check.mjs from the same seed.
import { SAMPLES, PARSERS, TYPE_NAMES, NUMERIC_OIDS, classify, highlight, toResult } from './sql.js';

const PGLITE = 'https://cdn.jsdelivr.net/npm/@electric-sql/pglite@0.5.16/dist/index.js';
const MAX_ROWS = 500; // drawn rows; the count shown is always the real one
const OPTIONS = { rowMode: 'array', parsers: PARSERS };

const ENVS = {
  local: {
    name: 'Shop (local)',
    note: 'No tint and no questions, except before a statement that would empty a table.',
  },
  staging: {
    name: 'Shop (staging)',
    note: 'The tabs, the frame and the status bar take the staging colour, so you can tell where you are without reading.',
  },
  production: {
    name: 'Shop (production)',
    note: 'The chrome turns red for as long as you are connected. A statement that writes shows its SQL and waits for you. Try the "Raise some prices" sample.',
  },
};

const $ = (selector) => document.querySelector(selector);
const el = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};
const flat = (sql) => sql.replace(/\s+/g, ' ').trim().replace(/;$/, '').toLowerCase();
const formatMs = (ms) => (ms < 10 ? `${ms.toFixed(1)} ms` : `${Math.round(ms)} ms`);
const plural = (n, word) => `${n.toLocaleString('en-US')} ${word}${n === 1 ? '' : 's'}`;

export function initDemo({ onStatus }) {
  const root = document.documentElement;
  const editor = $('#demo-sql');
  const paint = $('#demo-paint');
  const gutter = $('#demo-gutter');
  const out = $('#demo-result');
  const runButton = $('#demo-run');
  const confirmBox = $('#confirm');

  let env = 'local';
  let database; // promise of a seeded PGlite, created on first use
  let seed = '';
  let fallback = {};
  const typeNames = { ...TYPE_NAMES };

  // ── editor: a textarea over a highlighted copy of its text ──
  function repaint() {
    paint.innerHTML = `${highlight(editor.value)}\n`;
    gutter.textContent = editor.value.split('\n').map((_, i) => i + 1).join('\n');
    syncScroll();
  }
  function syncScroll() {
    paint.scrollTop = editor.scrollTop;
    paint.scrollLeft = editor.scrollLeft;
    gutter.style.translate = `0 ${-editor.scrollTop}px`;
  }
  function setSql(sql) {
    editor.value = sql;
    repaint();
    for (const node of document.querySelectorAll('[data-table][aria-current]')) node.removeAttribute('aria-current');
    for (const button of document.querySelectorAll('#samples button')) {
      button.setAttribute('aria-pressed', String(flat(button.dataset.sql) === flat(sql)));
    }
  }
  editor.addEventListener('input', () => setSql(editor.value));
  editor.addEventListener('scroll', syncScroll);
  editor.addEventListener('focus', () => engine().catch(() => {}), { once: true });
  editor.addEventListener('keydown', (event) => {
    if (event.key === 'Enter' && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      run();
    }
  });

  // ── status ──
  function status({ rows = '', ms = '', note = '' }) {
    $('#st-rows').textContent = rows;
    $('#st-ms').textContent = ms;
    $('#st-note').textContent = note;
    onStatus({ rows, ms });
  }
  function busy(on, note) {
    runButton.disabled = on;
    out.classList.toggle('is-busy', on);
    if (on && note) $('#st-note').textContent = note;
  }

  // ── PostgreSQL, on demand ──
  function engine() {
    database ??= (async () => {
      const [{ PGlite }, sql] = await Promise.all([
        import(PGLITE),
        fetch('data/seed.sql').then((response) => {
          if (!response.ok) throw new Error(`seed.sql: HTTP ${response.status}`);
          return response.text();
        }),
      ]);
      seed = sql;
      const pg = new PGlite();
      await pg.exec(seed);
      return pg;
    })();
    return database;
  }

  async function nameTypes(pg, result) {
    const unknown = result.columns.map((column) => column.oid).filter((oid) => !typeNames[oid]);
    if (!unknown.length) return;
    const { rows } = await pg.query('select oid::int, typname from pg_type where oid = any($1::oid[])', [unknown], { rowMode: 'array' });
    for (const [oid, name] of rows) typeNames[oid] = name;
  }

  // ── drawing a result ──
  function message(title, body, extra, isError) {
    const box = el('div', `result-msg${isError ? ' error' : ''}`);
    box.append(el('strong', '', title));
    if (body) box.append(el('span', '', body));
    if (extra) box.append(el('pre', '', extra));
    out.replaceChildren(box);
    out.scrollTop = 0;
  }

  const calm = matchMedia('(prefers-reduced-motion: reduce)').matches;

  function draw(result, note, stream) {
    const { columns, rows, affected } = result;
    const ms = result.ms === undefined ? '' : formatMs(result.ms);
    if (!columns.length) {
      message(affected ? `${plural(affected, 'row')} affected.` : 'Done.', 'The statement returned no rows.');
      status({ rows: affected ? `${plural(affected, 'row')} affected` : '', ms, note });
      return;
    }
    const table = el('table');
    const head = el('tr');
    const corner = el('th', 'n');
    corner.scope = 'col';
    corner.append(el('span', 'sr-only', 'Row'));
    head.append(corner);
    for (const column of columns) {
      const th = el('th', NUMERIC_OIDS.has(column.oid) ? 'num' : '', column.name);
      th.scope = 'col';
      th.append(el('small', '', typeNames[column.oid] || 'unknown'));
      head.append(th);
    }
    const thead = el('thead');
    thead.append(head);
    const body = el('tbody');
    const drawn = rows.slice(0, MAX_ROWS);
    const addRow = (row, index) => {
      const tr = el('tr');
      tr.append(el('td', 'n', String(index + 1)));
      row.forEach((value, i) => {
        const td = el('td', value === null ? 'null' : NUMERIC_OIDS.has(columns[i].oid) ? 'num' : '');
        td.append(el('samp', '', value === null ? 'NULL' : value)); // samp: output of a program
        tr.append(td);
      });
      body.append(tr);
    };
    table.append(thead, body);
    out.replaceChildren(table);
    out.scrollTop = 0;
    out.scrollLeft = 0;
    if (stream && !calm) {
      // The first result arrives a row per frame, the way a streamed result fills Kerno's grid.
      let index = 0;
      const next = () => {
        if (out.firstChild !== table) return; // another result replaced it
        addRow(drawn[index], index);
        index += 1;
        status({ rows: plural(index, 'row'), ms, note });
        if (index < drawn.length) requestAnimationFrame(next);
      };
      next();
      return;
    }
    drawn.forEach(addRow);
    const shown = rows.length > MAX_ROWS ? `, first ${MAX_ROWS} drawn` : '';
    status({ rows: `${plural(rows.length, 'row')}${shown}`, ms, note });
  }

  function drawError(error) {
    const lines = [`ERROR: ${error.message}`];
    if (error.detail) lines.push(`DETAIL: ${error.detail}`);
    if (error.hint) lines.push(`HINT: ${error.hint}`);
    if (error.position) lines.push(`At character ${error.position}`);
    message('PostgreSQL refused the statement.', 'The server said:', lines.join('\n'), true);
    status({ note: 'Error' });
  }

  function drawSaved(sql) {
    const sample = SAMPLES.find((candidate) => flat(candidate.sql) === flat(sql));
    if (sample && fallback[sample.id]) {
      draw(fallback[sample.id], 'Saved result: PostgreSQL could not start in this browser');
    } else {
      message(
        'PostgreSQL could not start in this browser.',
        'The live window needs WebAssembly and one download from cdn.jsdelivr.net. The samples beside it still show their saved results.',
      );
      status({ note: 'Offline' });
    }
  }

  // ── confirmations, worded as the app's ──
  function confirm(sql, kind) {
    const production = env === 'production';
    const name = ENVS[env].name;
    $('#confirm-title').textContent = production
      ? `Run this statement on Production: ${name}?`
      : 'Run this destructive statement?';
    const band = $('#confirm-band');
    band.hidden = !production;
    band.textContent = `Production · ${name}`;
    $('#confirm-why').textContent = kind.destructive
      ? 'It drops or empties data, or it has no WHERE clause, so every row it touches changes. Kerno shows the SQL and asks first on every connection.'
      : "This connection's environment asks before a statement that writes.";
    $('#confirm-sql').textContent = sql;
    return new Promise((resolve) => {
      confirmBox.returnValue = 'cancel';
      confirmBox.addEventListener('close', () => resolve(confirmBox.returnValue === 'run'), { once: true });
      confirmBox.showModal();
    });
  }

  // ── run ──
  async function run() {
    const sql = editor.value.trim();
    if (!sql || runButton.disabled) return;
    const kind = classify(sql);
    if ((kind.destructive || (kind.write && env === 'production')) && !(await confirm(sql, kind))) {
      $('#st-note').textContent = 'Cancelled: nothing ran';
      return;
    }
    busy(true, database ? 'Running…' : 'Starting PostgreSQL in this page…');
    try {
      let pg;
      try {
        pg = await engine();
      } catch {
        drawSaved(sql);
        return;
      }
      const started = performance.now();
      const result = toResult(await pg.exec(sql, OPTIONS), performance.now() - started);
      await nameTypes(pg, result).catch(() => {}); // a type name is a nicety, never a failure
      draw(result, 'PGlite: PostgreSQL in this page');
    } catch (error) {
      drawError(error);
    } finally {
      busy(false);
    }
  }
  runButton.addEventListener('click', run);

  // ── samples, tree, environment, reset ──
  const samples = $('#samples');
  for (const sample of SAMPLES) {
    const button = el('button');
    button.type = 'button';
    button.dataset.sql = sample.sql;
    button.append(el('span', '', sample.label), el('small', '', sample.hint));
    button.addEventListener('click', () => {
      setSql(sample.sql);
      run();
    });
    const item = el('li');
    item.append(button);
    samples.append(item);
  }

  for (const button of document.querySelectorAll('[data-table]')) {
    button.addEventListener('click', () => {
      setSql(`select * from ${button.dataset.table} limit 200`);
      button.setAttribute('aria-current', 'true');
      run();
    });
  }

  function setEnv(next) {
    env = next;
    root.dataset.env = next;
    for (const node of document.querySelectorAll('.js-conn')) node.textContent = ENVS[next].name;
    $('#env-note').textContent = ENVS[next].note;
    const radio = document.querySelector(`input[name="env"][value="${next}"]`);
    if (radio) radio.checked = true;
  }
  for (const radio of document.querySelectorAll('input[name="env"]')) {
    radio.addEventListener('change', () => setEnv(radio.value));
  }

  $('#demo-reset').addEventListener('click', async () => {
    if (!database) return void ($('#st-note').textContent = 'Nothing to reset yet');
    busy(true, 'Resetting…');
    try {
      const pg = await engine();
      await pg.exec('drop schema if exists shop cascade');
      await pg.exec(seed);
      $('#st-note').textContent = 'Demo data reset';
    } catch {
      $('#st-note').textContent = 'Reset failed: reload the page';
    } finally {
      busy(false);
    }
  });

  // ── first frame: the first sample with its saved result ──
  setSql(SAMPLES[0].sql);
  fetch('data/fallback.json')
    .then((response) => response.json())
    .then((saved) => {
      fallback = saved;
      Object.assign(typeNames, saved.types);
      if (!database) draw(saved[SAMPLES[0].id], 'Saved result. Run starts PostgreSQL in this page', true);
    })
    .catch(() => message('Ready.', 'Press Run to start PostgreSQL in this page.'));

  return { run, setEnv };
}
