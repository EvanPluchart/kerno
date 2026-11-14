// An interactive copy of the Kerno window for the Features section, with sample data.
// Wide screens only: on narrower ones (and without JavaScript) the page keeps its captures.
// Contract with the page: the mount is [data-kui] inside .feats; the feature list is the radio
// group input[name="feat"], whose change events switch the window to that feature. Nothing here touches the network or storage.

const LOGO = new URL('../img/engines/postgresql.svg', import.meta.url).href;
const ENGINE_LOGO = { mysql: new URL('../img/engines/mysql.svg', import.meta.url).href, mariadb: new URL('../img/engines/mariadb.svg', import.meta.url).href };
const esc = (s) => String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]);
const pad = (n) => String(n).padStart(2, '0');
const still = matchMedia('(prefers-reduced-motion: reduce)').matches;

// Icons: 16-unit stroke glyphs drawn for this page.
const cube = (x, y) => `M${x} ${y}l2.6 1.4v2.9L${x} ${y + 5.7}l-2.6-1.4v-2.9zM${x - 2.6} ${y + 1.4}l2.6 1.4 2.6-1.4M${x} ${y + 2.8}v2.9`;
const P = {
  chev: 'M6 3.5l4.5 4.5L6 12.5', down: 'M3.5 6l4.5 4.5L12.5 6', x: 'M4 4l8 8M12 4l-8 8', plus: 'M8 3v10M3 8h10',
  folder: 'M2 4.5a1 1 0 0 1 1-1h3l1.5 1.5H13a1 1 0 0 1 1 1V12a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1z',
  db: 'M3 4c0-1.1 2.2-2 5-2s5 .9 5 2v8c0 1.1-2.2 2-5 2s-5-.9-5-2zM3 4c0 1.1 2.2 2 5 2s5-.9 5-2M3 8c0 1.1 2.2 2 5 2s5-.9 5-2',
  schema: cube(8, 1.5) + cube(4.6, 7.6) + cube(11.4, 7.6),
  table: 'M2.5 2.5h11v11h-11zM2.5 6h11M2.5 9.75h11M6.5 6v7.5',
  column: 'M2.5 2.5h11v11h-11zM6.2 2.5v11M9.8 2.5v11',
  list: 'M3 4.5h.5M3 8h.5M3 11.5h.5M6 4.5h7M6 8h7M6 11.5h7',
  view: 'M1.5 8S4 3.5 8 3.5 14.5 8 14.5 8 12 12.5 8 12.5 1.5 8 1.5 8zM8 6a2 2 0 1 1 0 4 2 2 0 0 1 0-4z',
  fn: 'M11 2.5c-1.8 0-2.6.8-3 2.8l-1.6 7c-.4 1.6-1 2.2-2.4 2.2M5 7h5.5',
  ext: 'M6 2.5h4v3a1.5 1.5 0 1 0 0 3v3H6.5a1.5 1.5 0 1 1-3 0H2.5V8.5a1.5 1.5 0 1 0 0-3V2.5z',
  search: 'M7 2.5a4.5 4.5 0 1 1 0 9 4.5 4.5 0 0 1 0-9zM10.3 10.3L13.5 13.5',
  clock: 'M8 2a6 6 0 1 1 0 12A6 6 0 0 1 8 2zM8 5v3.2l2 1.3', mark: 'M4.5 2.5h7v11L8 11l-3.5 2.5z',
  dbadd: 'M3 4c0-1.1 2-2 4.5-2S12 2.9 12 4v3M3 4v7.5c0 1.1 2 2 4.5 2M3 4c0 1.1 2 2 4.5 2S12 5.1 12 4M3 7.75c0 1.1 2 2 4.5 2M12.5 10v4M10.5 12h4',
  plug: 'M6 2v3M10 2v3M4.5 5h7v2.5a3.5 3.5 0 0 1-7 0zM8 11v3',
  unplug: 'M6 2v3M10 2v3M4.5 5h7v2.5a3.5 3.5 0 0 1-7 0zM8 11v3M2.5 13.5l11-11',
  refresh: 'M13 3v2.8h-2.8M13 5.8A5.5 5.5 0 1 0 13.5 9',
  file: 'M4 1.5h5.5l3 3v10H4zM9.5 1.5v3h3M7.2 8L5.8 9.5 7.2 11M9 8l1.4 1.5L9 11',
  check: 'M3.5 8.5l3 3 6-7', undo: 'M5 4.5L2.5 7 5 9.5M2.5 7H10a3.2 3.2 0 0 1 0 6.5H7',
  log: 'M4 2.5h8v11H4zM6 5.5h4M6 8h4M6 10.5h2.5', lock: 'M4 7.5h8v6H4zM5.5 7.5V5a2.5 2.5 0 0 1 4.9-.7',
  dash: 'M2.5 2.5h4.5v4.5h-4.5zM9 2.5h4.5v4.5H9zM2.5 9h4.5v4.5h-4.5zM9 9h4.5v4.5H9z',
  wrench: 'M10.5 2a3.2 3.2 0 0 0-3 4.3l-5 5a1.3 1.3 0 0 0 1.9 1.9l5-5A3.2 3.2 0 0 0 13.8 5.4l-1.9 1.8-2-.3-.3-2L11.4 3a3 3 0 0 0-.9-1z',
  sort: 'M5.5 6.2L8 3.7l2.5 2.5M5.5 9.8L8 12.3l2.5-2.5', eye: 'M1.5 8S4 3.5 8 3.5 14.5 8 14.5 8 12 12.5 8 12.5 1.5 8 1.5 8zM8 6.2a1.8 1.8 0 1 1 0 3.6 1.8 1.8 0 0 1 0-3.6z',
};
const ic = (n, c = '') => `<svg class="ki ${c}" viewBox="0 0 16 16" aria-hidden="true"><path d="${P[n]}"/></svg>`;
const logo = (c = 'k-logo') => `<img class="${c}" src="${LOGO}" alt="" aria-hidden="true">`;

// ---------------------------------------------------------------- Sample data: schema shop.
const col = (n, t, d, f = '') => ({ n, t, d, pk: f === 'pk', nn: f !== '' });
const NAMES = ['Grace Lovelace', 'Ada Perlman', 'Frances Hamilton', 'Edsger Lovelace', 'Radia Perlman', 'Ken Ritchie', 'Barbara Hopper', 'Dennis Dijkstra', 'Alan Kay', 'Margaret Hamilton', 'Donald Knuth', 'Barbara Liskov', 'Ken Thompson', 'Niklaus Wirth', 'Leslie Lamport', 'Tony Hoare', 'John McCarthy', 'Hedy Lamarr', 'Katherine Johnson', 'Linus Torvalds'];
const PRODUCTS = [['Pour-over kettle', 64, 12], ['Burr grinder', 189, 4], ['Espresso cups, set of 2', 24.9, 36], ['Paper filters, 100', 6.5, 220], ['Glass carafe', 32, 15], ['Coffee scale', 45, 9], ['Cold brew jar', 28, 18], ['Milk jug', 19.5, 27], ['Tamper', 39, 6], ['Knock box', 27, 11], ['Travel mug', 22, 40], ['Ethiopia Guji, 250 g', 14.5, 64], ['Colombia Huila, 250 g', 13, 58], ['Decaf blend, 250 g', 12, 30], ['Cleaning tablets', 11.9, 75]];
const STATUS = ['pending', 'paid', 'shipped', 'delivered', 'cancelled', 'refunded'];
// The first eight orders are the ones of the reference capture.
const ST0 = ['shipped', 'paid', 'pending', 'refunded', 'cancelled', 'delivered', 'delivered', 'shipped'];
const TOT0 = [19.36, 167.1, 351.55, 254.61, 146.64, 285.5, 501.03, 478.09], CUR0 = ['EUR', 'GBP', 'USD', 'EUR', 'EUR', 'GBP', 'USD', 'EUR'];
const stamp = (d, tz) => d.toISOString().slice(0, 19).replace('T', ' ') + (tz ? '+00' : '');
const T = {
  customers: { size: '16 kB', cols: [col('id', 'int8', 'bigint', 'pk'), col('email', 'text', 'text', 'nn'), col('full_name', 'text', 'text', 'nn'), col('phone', 'varchar', 'character varying(32)'), col('is_active', 'bool', 'boolean', 'nn'), col('tags', 'text[]', 'text[]', 'nn'), col('created_at', 'timestamptz', 'timestamp with time zone', 'nn')],
    rows: NAMES.map((name, i) => [i + 1, `customer${i + 1}@example.com`, name, i % 3 === 2 ? null : `+33 6 ${12 + i} ${40 + i} ${10 + 2 * i} ${71 - i}`, i % 7 !== 6, ['{vip}', '{}', '{newsletter}', '{vip,wholesale}', '{wholesale}'][i % 5], `2024-0${1 + (i % 6)}-${pad(3 + i)} 09:${pad((i * 7) % 60)}:00+00`]) },
  orders: { size: '24 kB', cols: [col('id', 'int8', 'bigint', 'pk'), col('customer_id', 'int8', 'bigint', 'nn'), col('status', 'order_status', 'shop.order_status', 'nn'), col('placed_at', 'timestamptz', 'timestamp with time zone', 'nn'), col('shipped_at', 'timestamp', 'timestamp without time zone'), col('total', 'numeric', 'numeric(10,2)', 'nn'), col('currency', 'bpchar', 'character(3)', 'nn'), col('notes', 'text', 'text')],
    rows: Array.from({ length: 25 }, (_, k) => {
      const placed = new Date(Date.UTC(2024, 6, 3, 0, 40) - k * 53 * 60000), status = ST0[k] ?? STATUS[(k * 5) % 6];
      return [5000 - k, (k % 20) + 1, status, stamp(placed, true), /shipped|delivered/.test(status) ? stamp(new Date(+placed + 864e5)) : null,
        TOT0[k] ?? Math.round(2000 + ((k * 7919) % 48000)) / 100, CUR0[k] ?? ['EUR', 'EUR', 'USD', 'GBP'][k % 4], k % 6 === 4 ? 'Leave at the door' : null];
    }) },
  order_items: { size: '16 kB', cols: [col('id', 'int8', 'bigint', 'pk'), col('order_id', 'int8', 'bigint', 'nn'), col('product_id', 'int8', 'bigint', 'nn'), col('quantity', 'int4', 'integer', 'nn'), col('unit_price', 'numeric', 'numeric(10,2)', 'nn')],
    rows: Array.from({ length: 25 }, (_, i) => [i + 1, 5000 - (i % 15), ((i * 4) % 15) + 1, (i % 3) + 1, PRODUCTS[(i * 4) % 15][1]]) },
  products: { size: '16 kB', cols: [col('id', 'int8', 'bigint', 'pk'), col('sku', 'text', 'text', 'nn'), col('name', 'text', 'text', 'nn'), col('price', 'numeric', 'numeric(10,2)', 'nn'), col('stock', 'int4', 'integer', 'nn'), col('is_active', 'bool', 'boolean', 'nn')],
    rows: PRODUCTS.map(([name, price, stock], i) => [i + 1, `KR-${1001 + i}`, name, price, stock, i !== 9]) },
};
const NUM = new Set(['int2', 'int4', 'int8', 'numeric', 'float8']);
const fmt = (v, t) => v === null ? 'NULL' : t === 'numeric' && typeof v === 'number' ? v.toFixed(2) : String(v);
const lit = (v, t) => v === null ? 'NULL' : typeof v === 'number' || typeof v === 'boolean' ? fmt(v, t) : `'${String(v).replace(/'/g, "''")}'`;

// ---------------------------------------------------------------- A small SELECT interpreter.
class SqlErr extends Error { constructor(msg, code) { super(msg); this.code = code; } }
const DEMO = 'This demo window only runs simple SELECTs on the sample data: columns or *, FROM shop.<table>, JOIN … ON, WHERE with AND, ORDER BY and LIMIT.';
const nope = () => { throw new SqlErr(DEMO); };
const RESERVED = new Set('select from where join inner left outer on and or not order by limit offset group having as union is null like ilike in desc asc'.split(' '));

function lex(s) {
  const out = [], re = /\s+|--[^\n]*|'((?:[^']|'')*)'|"([^"]*)"|(\d+(?:\.\d+)?)|([A-Za-z_]\w*)|(<>|!=|<=|>=|[(),.*=<>;])|(.)/g;
  for (let m; (m = re.exec(s));) {
    if (m[1] !== undefined) out.push({ k: 'str', v: m[1].replace(/''/g, "'") });
    else if (m[2] !== undefined) out.push({ k: 'id', v: m[2], q: true });
    else if (m[3]) out.push({ k: 'num', v: +m[3] });
    else if (m[4]) out.push({ k: 'id', v: m[4].toLowerCase() });
    else if (m[5]) out.push({ k: 'p', v: m[5] });
    else if (m[6]) nope();
  }
  return out;
}

function parse(sql) {
  const tk = lex(sql); let i = 0;
  const at = (v, k = 'id') => tk[i] && tk[i].k === k && tk[i].v === v && !tk[i].q;
  const eat = (v, k) => (at(v, k) ? ++i : 0);
  const need = (v, k) => eat(v, k) || nope();
  const free = () => tk[i]?.k === 'id' && (tk[i].q || !RESERVED.has(tk[i].v));
  const ident = () => (free() ? tk[i++].v : nope());
  const ref = () => { const a = ident(); return eat('.', 'p') ? { q: a, c: ident() } : { c: a }; };
  const operand = () => {
    const t = tk[i];
    if (t && (t.k === 'num' || t.k === 'str')) return i++, { v: t.v };
    if (eat('true') || at('false') && ++i) return { v: tk[i - 1].v === 'true' };
    if (eat('-', 'p') && tk[i]?.k === 'num') return { v: -tk[i++].v };
    const r = ref(); if (at('(', 'p')) nope(); return r;
  };
  const cond = () => {
    const a = operand();
    if (eat('is')) { const n = eat('not'); need('null'); return { a, op: n ? 'nn' : 'n' }; }
    const neg = !!eat('not');
    if (eat('in')) { need('(', 'p'); const list = []; do list.push(operand()); while (eat(',', 'p')); need(')', 'p'); return { a, op: 'in', list, neg }; }
    for (const op of ['like', 'ilike']) if (eat(op)) return { a, op, b: operand(), neg };
    const t = tk[i];
    if (neg || !t || t.k !== 'p' || !['=', '<>', '!=', '<', '<=', '>', '>='].includes(t.v)) nope();
    i++; return { a, op: t.v, b: operand() };
  };
  const conds = () => { const l = [cond()]; while (eat('and')) l.push(cond()); if (at('or')) nope(); return l; };
  const table = () => {
    let name = ident();
    if (eat('.', 'p')) { const t = ident(); if (name !== 'shop' || !T[t]) throw new SqlErr(`relation "${name}.${t}" does not exist`, '42P01'); name = t; }
    else if (!T[name]) throw new SqlErr(`relation "${name}" does not exist`, '42P01');
    eat('as'); return { name, alias: free() ? ident() : name };
  };
  need('select'); if (at('distinct')) nope();
  const items = [];
  do {
    if (eat('*', 'p')) items.push({ star: true });
    else if (tk[i + 1]?.v === '.' && tk[i + 2]?.v === '*') { const q = ident(); i += 2; items.push({ star: true, q }); }
    else { const r = ref(); if (at('(', 'p')) nope(); eat('as'); items.push({ r, as: free() ? ident() : null }); }
  } while (eat(',', 'p'));
  need('from');
  const from = [table()];
  for (;;) {
    let left = false;
    if (eat('left')) { left = true; eat('outer'); need('join'); } else if (eat('inner')) need('join'); else if (!eat('join')) break;
    const t = table(); need('on'); from.push({ ...t, left, on: conds() });
  }
  const where = eat('where') ? conds() : [], order = [];
  if (eat('order')) {
    need('by');
    do { const r = tk[i]?.k === 'num' ? { pos: tk[i++].v } : ref(), desc = !!eat('desc'); if (!desc) eat('asc'); order.push({ r, desc }); } while (eat(',', 'p'));
  }
  let limit = Infinity;
  if (eat('limit')) { if (tk[i]?.k !== 'num') nope(); limit = tk[i++].v; }
  eat(';', 'p'); if (i < tk.length) nope();
  return { items, from, where, order, limit };
}

const cmp = (a, b) => {
  if (typeof a === 'number' && typeof b === 'string' && b.trim() && !isNaN(b)) b = +b;
  if (typeof b === 'number' && typeof a === 'string' && a.trim() && !isNaN(a)) a = +a;
  if (typeof a === 'number' && typeof b === 'number') return a - b;
  a = String(a); b = String(b); return a < b ? -1 : a > b ? 1 : 0;
};

// Runs one SELECT; `extra` is the result panel's WHERE box, ANDed to the statement's own WHERE.
function execSql(sql, extra = '') {
  const p = parse(sql);
  if (extra.trim()) p.where.push(...parse(`select * from ${p.from[0].name} where ${extra}`).where);
  const sc = p.from;
  const col = (r) => {
    if (r.q && !sc.some((s) => s.alias === r.q)) throw new SqlErr(`missing FROM-clause entry for table "${r.q}"`, '42P01');
    const hits = [];
    sc.forEach((s, si) => { const ci = r.q && r.q !== s.alias ? -1 : T[s.name].cols.findIndex((c) => c.n === r.c); if (ci >= 0) hits.push([si, ci]); });
    if (!hits.length) throw new SqlErr(`column ${r.q ? `${r.q}.${r.c}` : `"${r.c}"`} does not exist`, '42703');
    if (hits.length > 1) throw new SqlErr(`column reference "${r.c}" is ambiguous`, '42702');
    return hits[0];
  };
  const val = (o, env) => ('v' in o ? o.v : env[(o.k ??= col(o))[0]]?.[o.k[1]] ?? null);
  const test = (c, env) => {
    const a = val(c.a, env);
    if (c.op === 'n' || c.op === 'nn') return (a === null) === (c.op === 'n');
    if (a === null) return false;
    if (c.op === 'in') return c.list.some((o) => cmp(a, val(o, env)) === 0) !== c.neg;
    const b = val(c.b, env); if (b === null) return false;
    if (c.op === 'like' || c.op === 'ilike') {
      const re = '^' + String(b).replace(/[.+?^${}()|[\]\\*]/g, '\\$&').replace(/%/g, '.*').replace(/_/g, '.') + '$';
      return new RegExp(re, c.op === 'ilike' ? 'is' : 's').test(String(a)) !== c.neg;
    }
    const d = cmp(a, b);
    return { '=': d === 0, '<>': d !== 0, '!=': d !== 0, '<': d < 0, '<=': d <= 0, '>': d > 0, '>=': d >= 0 }[c.op];
  };
  const outs = [];
  for (const it of p.items) {
    if (it.star) {
      if (it.q && !sc.some((s) => s.alias === it.q)) throw new SqlErr(`missing FROM-clause entry for table "${it.q}"`, '42P01');
      sc.forEach((s, si) => { if (!it.q || it.q === s.alias) T[s.name].cols.forEach((c, ci) => outs.push({ n: c.n, t: c.t, k: [si, ci] })); });
    } else { const k = col(it.r); outs.push({ n: it.as || it.r.c, t: T[sc[k[0]].name].cols[k[1]].t, k }); }
  }
  let envs = T[sc[0].name].rows.map((r) => [r]);
  for (const j of sc.slice(1)) {
    const next = [];
    for (const e of envs) {
      let hit = false;
      for (const r of T[j.name].rows) { const env = [...e, r]; if (j.on.every((c) => test(c, env))) next.push(env), (hit = true); }
      if (!hit && j.left) next.push([...e, null]);
    }
    envs = next;
  }
  envs = envs.filter((e) => p.where.every((c) => test(c, e)));
  if (p.order.length) {
    const keys = p.order.map(({ r, desc }) => {
      if (r.pos) { const x = outs[r.pos - 1]; if (!x) throw new SqlErr(`ORDER BY position ${r.pos} is not in select list`, '42P10'); return { k: x.k, desc }; }
      const x = !r.q && outs.find((o) => o.n === r.c);
      return { k: x ? x.k : col(r), desc };
    });
    envs.sort((a, b) => {
      for (const { k, desc } of keys) {
        const x = a[k[0]]?.[k[1]] ?? null, y = b[k[0]]?.[k[1]] ?? null; // PostgreSQL puts NULLs last ascending, first descending.
        if (x === y) continue;
        const d = x === null ? 1 : y === null ? -1 : cmp(x, y);
        if (d) return desc ? -d : d;
      }
      return 0;
    });
  }
  return { cols: outs.map(({ n, t }) => ({ n, t })), rows: envs.slice(0, p.limit).map((e) => outs.map((o) => e[o.k[0]]?.[o.k[1]] ?? null)), multi: sc.length > 1 };
}

// ---------------------------------------------------------------- Queries and the plan.
const Q1 = 'SELECT\n    o.id,\n    c.full_name,\n    c.email,\n    o.status,\n    o.total,\n    o.currency,\n    o.placed_at\nFROM shop.orders o\nJOIN shop.customers c ON c.id = o.customer_id\nORDER BY o.placed_at DESC\nLIMIT 50\n';
const QX = 'SELECT\n    c.full_name,\n    count(*) AS orders,\n    sum(o.total) AS revenue\nFROM shop.orders o\nJOIN shop.customers c ON c.id = o.customer_id\nJOIN shop.order_items i ON i.order_id = o.id\nGROUP BY c.full_name\nORDER BY revenue DESC\nLIMIT 20\n';
const Q2 = 'select id, full_name, email, tags from shop.customers limit 20\n';
const norm = (s) => s.replace(/\s+/g, ' ').trim().replace(/;$/, '').toLowerCase();
// The EXPLAIN ANALYZE of QX as the app drew it (reference capture explain-plan, dark): depth, step, relation, cost, est. rows, rows, own ms, total ms, details.
const PLAN = [
  [0, 'Limit', '', 648.4, 20, 20, 0.03, 16.3, ''],
  [1, 'Sort', '', 648.6, 100, 20, 0.08, 16.2, 'Sort Key: (sum(o.total)) DESC · Sort Method: top-N heapsort · Sort Space Type: Memory · Sort Space Used: 27'],
  [2, 'Aggregate', '', 645.7, 100, 100, 3.71, 16.2, 'Strategy: Hashed · Group Key: c.full_name · HashAgg Batches: 1 · Partial Mode: Simple'],
  [3, 'Hash Join', '', 550.7, 12500, 12500, 2.62, 12.5, 'Join Type: Inner · Hash Cond: (o.customer_id = c.id)'],
  [4, 'Hash Join', '', 490.3, 12500, 12500, 3.57, 9.41, 'Join Type: Inner · Hash Cond: (i.order_id = o.id)'],
  [5, 'Seq Scan', 'order_items', 230, 12500, 12500, 3.11, 3.11, 'Alias: i'],
  [5, 'Hash', '', 165, 5000, 5000, 0.82, 2.74, 'Hash Batches: 1 · Hash Buckets: 8192 · Original Hash Batches: 1 · Original Hash Buckets: 8192'],
  [6, 'Seq Scan', 'orders', 165, 5000, 5000, 1.92, 1.92, 'Alias: o'],
  [4, 'Hash', '', 21, 500, 500, 0.09, 0.42, 'Hash Batches: 1 · Hash Buckets: 1024 · Original Hash Batches: 1 · Original Hash Buckets: 1024'],
  [5, 'Seq Scan', 'customers', 21, 500, 500, 0.33, 0.33, 'Alias: c'],
];
const SLOW = PLAN.reduce((m, r, i) => (r[6] > PLAN[m][6] ? i : m), 0);
const kids = (i) => { const out = []; for (let j = i + 1; j < PLAN.length && PLAN[j][0] > PLAN[i][0]; j++) if (PLAN[j][0] === PLAN[i][0] + 1) out.push(j); return out; };
// Raw output: the same figures in EXPLAIN's JSON shape (only the fields the capture shows).
const rawPlan = (i = 0) => {
  const [, step, rel, cost, est, rows, , total] = PLAN[i], k = kids(i);
  return { 'Node Type': step, ...(rel && { 'Relation Name': rel }), 'Total Cost': cost, 'Plan Rows': est, 'Actual Rows': rows, 'Actual Total Time': total, ...(k.length && { Plans: k.map(rawPlan) }) };
};

// ---------------------------------------------------------------- Navigator tree.
const leaf = (id, label, icon, meta = '') => ({ id, label, icon, meta });
const folder = (id, label, kidsList, icon = 'folder') => ({ id, label, icon, meta: kidsList.length, kids: kidsList });
const FK = { orders: ['orders_customer_id_fkey'], order_items: ['order_items_order_id_fkey', 'order_items_product_id_fkey'] };
const REFS = { customers: ['orders_customer_id_fkey'], orders: ['order_items_order_id_fkey'], products: ['order_items_product_id_fkey'] };
const tableNode = (t) => ({ id: `t:${t}`, label: t, icon: 'table', table: t, meta: `${T[t].rows.length} · ${T[t].size}`, kids: [
  folder(`t:${t}:cols`, 'Columns', T[t].cols.map((c) => leaf(`t:${t}:c:${c.n}`, c.n, 'column', `${c.d}${c.pk ? ' · key' : c.nn ? ' · not null' : ''}`))),
  folder(`t:${t}:con`, 'Constraints', [leaf(`t:${t}:pk`, `${t}_pkey`, 'list', 'primary key')]),
  folder(`t:${t}:fk`, 'Foreign keys', (FK[t] || []).map((n) => leaf(`t:${t}:fk:${n}`, n, 'list'))),
  folder(`t:${t}:ix`, 'Indexes', [leaf(`t:${t}:ix:pk`, `${t}_pkey`, 'list', 'unique')]),
  folder(`t:${t}:rf`, 'References', (REFS[t] || []).map((n) => leaf(`t:${t}:rf:${n}`, n, 'list'))),
  folder(`t:${t}:tg`, 'Triggers', []),
] });
const TREE_ROOT = [folder('local', 'Local', [{ id: 'conn', label: 'Shop (demo)', conn: true, meta: 'connected', kids: [
  { id: 'db', label: 'kerno', icon: 'db', badge: 'default', meta: '8.2 MB', kids: [
    { id: 's:analytics', label: 'analytics', icon: 'schema', kids: [folder('s:analytics:t', 'Tables', [leaf('a:t1', 'daily_revenue', 'table', '30 · 8 kB')]), folder('s:analytics:v', 'Views', [])] },
    { id: 's:public', label: 'public', icon: 'schema', kids: [folder('s:public:t', 'Tables', []), folder('s:public:v', 'Views', [])] },
    { id: 's:shop', label: 'shop', icon: 'schema', kids: [
      folder('shop:t', 'Tables', ['customers', 'order_items', 'orders', 'products'].map(tableNode)),
      folder('shop:v', 'Views', [leaf('v1', 'order_totals', 'view')]),
      folder('shop:mv', 'Materialized views', [leaf('mv1', 'monthly_revenue', 'view')]),
      folder('shop:f', 'Functions', [leaf('f1', 'customer_lifetime_value(p_customer_id bigint)', 'fn')]),
      folder('shop:sq', 'Sequences', ['customers', 'order_items', 'orders', 'products'].map((t) => leaf(`sq:${t}`, `${t}_id_seq`, 'list'))),
      folder('shop:ty', 'Types', [{ id: 'ty:os', label: 'order_status', icon: 'schema', meta: 'enum', kids: STATUS.map((s) => leaf(`ty:os:${s}`, s, 'list')) }]),
    ] },
    folder('ext', 'Extensions', [leaf('ext:pg', 'plpgsql', 'ext', '1.0')], 'ext'),
  ] },
  { id: 'db2', label: 'postgres', icon: 'db', meta: '7.3 MB', kids: [{ id: 'db2:s', label: 'public', icon: 'schema', kids: [folder('db2:t', 'Tables', [])] }] },
] }])];
const NODES = new Map();
const index = (ns, parent) => ns.forEach((n) => { n.parent = parent; NODES.set(n.id, n); if (n.kids) index(n.kids, n); });
index(TREE_ROOT, null);
const TREE = { exp: new Set(), sel: 't:customers', filter: '' };
const treeReset = () => { TREE.exp = new Set(['local', 'conn', 'db', 's:shop', 'shop:t']); TREE.sel = 't:customers'; TREE.filter = ''; };

// ---------------------------------------------------------------- Per-feature models.
const dataModel = (table, pend = {}) => ({ kind: 'data', table, pend, del: [], add: [], where: '', err: '', sort: null });
const queryModel = (sql, plan = false) => { const m = { kind: 'query', sql, where: '', plan, raw: false, shut: new Set(), sort: null }; run(m); return m; };
const designModel = (table) => {
  const cols = T[table].cols.map((c, i) => ({ id: i, n: c.n, d: c.d, nul: !c.nn, pk: c.pk, auto: false, def: '', cm: '' }));
  return { kind: 'design', table, name: table, comment: '', orig: cols.map((c) => ({ ...c })), cols, seq: cols.length };
};
const INIT = {
  navigator: () => { treeReset(); root.querySelector('[data-a="filter"]').value = ''; return { view: dataModel('customers') }; },
  editor: () => ({ view: queryModel(Q1) }),
  edits: () => ({ view: dataModel('products', { '3|stock': 24 }) }),
  explain: () => ({ view: queryModel(QX, true) }),
  structure: () => { const v = designModel('products'); v.cols.push({ id: v.seq++, n: 'weight_kg', d: 'numeric(6,2)', nul: true, pk: false, auto: false, def: '', cm: '' }); return { view: v }; },
  export: () => ({ view: dataModel('orders'), dlg: exportModel('orders') }),
  backup: () => ({ view: queryModel(Q2), dlg: backupModel() }),
  ssh: () => ({ view: queryModel(Q2), dlg: connModel('ssh') }),
};
const exportModel = (table) => ({ kind: 'export', table, fmt: 'csv', delim: ',', nul: '', header: true, eol: 'lf', enc: 'utf8', note: '' });
const backupModel = () => ({ kind: 'backup', step: 1, complete: false, views: false, schemas: { analytics: false, public: false, shop: true },
  tables: Object.fromEntries(Object.keys(T).map((t) => [t, true])), fmt: 'c', compress: '', enc: '', content: 'all',
  o: { inserts: false, nopriv: false, noowner: false, clean: false, ifexists: false, create: false },
  folder: '/Users/Shared/kerno-demo/out', pattern: '${database}-${timestamp}', extra: '', log: [], running: false, done: false });
const connModel = (tab) => ({ kind: 'conn', tab, engine: 'pg', search: '', note: '',
  g: { url: '', name: '', folder: 'none', colour: 0, env: 'local', host: 'localhost', port: '5432', user: 'postgres', password: '', show: false, remember: true, db: '', ro: false },
  ssl: { mode: 'prefer', ca: '', cert: '', key: '' },
  ssh: { on: true, host: 'bastion.example.com', port: '22', user: '', auth: 'agent', socket: '', password: '', key: '', pass: '', keep: '30' },
  adv: { opts: '', timeout: '10' } });

const S = {}; let cur = 'navigator';
const st = () => S[cur];
treeReset();
const focusDlg = () => (root.querySelector('.k-dlg .k-dbody input:not(:disabled), .k-dlg .k-wbody input:not(:disabled)') || root.querySelector('.k-dlg [data-a]'))?.focus({ preventScroll: true });

function run(m) {
  if (!norm(m.sql)) { m.res = { info: 'Nothing to run.' }; return; }
  try { m.res = execSql(m.sql, m.where); } catch (e) { if (!(e instanceof SqlErr)) throw e; m.res = { err: e.message + (e.code ? ` [${e.code}]` : '') }; }
  m.sort = null;
}
// No duration is shown anywhere: a time measured in the visitor's browser would say nothing about Kerno.

// ---------------------------------------------------------------- Shell.
let root, win, side, main, layer, status, k = 1;
const TOOL = `<span>${ic('dbadd')}${ic('down', 's')}</span><span class="off g">${ic('plug')}</span><span>${ic('refresh')}</span><span>${ic('unplug')}</span><span class="g">${ic('file')}${ic('down', 's')}</span>
  <span class="off g">${ic('check')}</span><span class="off">${ic('undo')}</span><span>Auto${ic('down', 's')}</span><span class="g">${ic('log')}</span><span>${ic('lock')}</span>
  <span class="g">${logo()}Shop (demo)${ic('down', 's')}</span><span>kerno@Shop (demo)${ic('down', 's')}</span><span class="g">${ic('dash')}</span>`;

function build(mount) {
  root = document.createElement('div');
  root.className = 'kui'; root.dataset.ui = '';
  root.setAttribute('role', 'region'); root.setAttribute('aria-label', 'Interactive demo of the Kerno window, with sample data');
  root.innerHTML = `<div class="k-win"><div class="k-title" aria-hidden="true"><i></i><i></i><i></i><span>Kerno</span></div>
    <div class="k-tool"><div style="display:contents" aria-hidden="true">${TOOL}</div><button type="button" data-a="tools" title="Tools: backup, restore…">${ic('wrench')}${ic('down', 's')}</button><span aria-hidden="true">${ic('search')}</span></div>
    <div class="k-body"><aside class="k-side"><div class="k-sh">CONNECTIONS<button type="button" data-a="newconn" title="New connection" aria-label="New connection">${ic('plus')}</button></div>
      <div class="k-search">${ic('search')}<input type="search" data-a="filter" placeholder="Filter connections and objects" aria-label="Filter connections and objects"></div>
      <div class="k-tree" role="tree" aria-label="Connections"></div></aside>
      <div class="k-main"><div class="k-tabs" aria-hidden="true"></div><div class="k-view"></div></div></div>
    <div class="k-status"></div></div><div class="k-layer"></div>`;
  mount.append(root);
  [win, side, main, layer, status] = ['.k-win', '.k-tree', '.k-view', '.k-layer', '.k-status'].map((s) => root.querySelector(s));
  wire();
  new ResizeObserver(() => fit(mount)).observe(mount);
  fit(mount);
}

// One zoom factor fits the window in its box while it is drawn at the app's real sizes.
function fit(mount) {
  const w = mount.clientWidth, h = mount.clientHeight; if (!w) return;
  k = Math.min(1, Math.max(0.8, w / 1012));
  Object.assign(root.style, { zoom: k, width: `${w / k}px`, height: `${h / k}px` });
}
// Client coordinates to window coordinates, whatever the engine reports for zoomed boxes.
const local = (x, y) => { const r = root.getBoundingClientRect(), s = r.width / parseFloat(root.style.width); return [(x - r.left) / s, (y - r.top) / s]; };

function render() {
  renderTree(); renderView(); renderLayer(); renderStatus();
}
function keep(fn) { // re-render while keeping focus and caret on the "same" control
  const a = document.activeElement, key = root.contains(a) && keyOf(a);
  const range = a && typeof a.selectionStart === 'number' ? [a.selectionStart, a.selectionEnd] : null;
  fn();
  const b = key && root.querySelector(key);
  if (b && b !== document.activeElement) { b.focus({ preventScroll: true }); if (range) try { b.setSelectionRange(...range); } catch { /* not a text field */ } }
}
const keyOf = (el) => {
  const d = el.dataset, q = (v) => CSS.escape(v);
  if (d.m) return `[data-m="${q(d.m)}"]${el.type === 'radio' ? `[value="${q(el.value)}"]` : ''}`;
  if (d.a) return `[data-a="${q(d.a)}"]${d.v ? `[data-v="${q(d.v)}"]` : ''}`;
  if (d.id) return `[data-id="${q(d.id)}"]`;
  if (d.cell) return `[data-cell="${d.cell}"]`;
  return el.classList.contains('k-ta') ? '.k-ta' : null;
};
let noteTimer;
function note(msg) {
  const n = status.querySelector('.k-note'); if (!n) return;
  n.textContent = msg; clearTimeout(noteTimer); noteTimer = setTimeout(() => { n.textContent = ''; }, 5000);
}
function renderStatus() {
  const v = st().view, items = ['kerno'];
  if (v.kind === 'data') { const n = pendCount(v); if (n) items.push(`<span class="warn">${n} pending change${n > 1 ? 's' : ''}</span>`); items.push(`${rowsOf(v).length} rows`); }
  if (v.kind === 'query' && !v.plan && v.res?.rows) items.push(`${v.res.rows.length} rows`);
  status.innerHTML = `<span><i class="k-dot"></i>${logo()}Shop (demo)</span>${items.map((x) => `<span>${x}</span>`).join('')}<span class="k-note" aria-live="polite"></span>`;
}

// ---------------------------------------------------------------- Tree.
function visible() {
  const out = [], f = TREE.filter.toLowerCase();
  const hit = (n) => n.label.toLowerCase().includes(f) || n.kids?.some(hit);
  const walk = (ns, d) => { for (const n of ns) { if (f && !hit(n)) continue; out.push([n, d]); if (n.kids?.length && (f || TREE.exp.has(n.id))) walk(n.kids, d + 1); } };
  walk(TREE_ROOT, 0); return out;
}
function renderTree() {
  const rows = visible(); if (!rows.some(([n]) => n.id === TREE.sel)) TREE.sel = rows[0]?.[0].id;
  side.innerHTML = rows.map(([n, d]) => {
    const open = n.kids?.length ? (TREE.filter || TREE.exp.has(n.id)) : null, s = n.id === TREE.sel;
    return `<div class="k-ti${s ? ' sel' : ''}" role="treeitem" data-id="${esc(n.id)}" aria-level="${d + 1}" aria-selected="${s}" ${open === null ? '' : `aria-expanded="${!!open}"`} tabindex="${s ? 0 : -1}" style="--d:${d}">`
      + `<span class="k-chev"${open === null ? '' : ` data-a="toggle" data-v="${esc(n.id)}"`}>${open === null ? '' : ic(open ? 'down' : 'chev')}</span>`
      + (n.conn ? `<i class="k-dot"></i>${logo()}` : ic(n.icon)) + `<span class="k-tl">${esc(n.label)}</span>`
      + (n.badge ? `<span class="k-badge">${n.badge}</span>` : '') + (n.meta !== undefined && n.meta !== '' ? `<span class="k-tm${n.conn ? ' ok' : ''}">${esc(n.meta)}</span>` : '') + '</div>';
  }).join('');
}
const toggle = (id) => { const n = NODES.get(id); if (!n?.kids?.length || TREE.filter) return; TREE.exp.has(id) ? TREE.exp.delete(id) : TREE.exp.add(id); keep(renderTree); };
function openNode(id) {
  const n = NODES.get(id);
  if (n?.table) return openTable(n.table);
  toggle(id);
}
function openTable(t) {
  if (['navigator', 'edits'].includes(cur) && st().view.kind === 'data') { if (st().view.table !== t) st().view = dataModel(t); keep(renderView); renderStatus(); }
  else go('navigator', { view: dataModel(t) });
}
function treeKey(e, el) {
  const rows = visible(), i = rows.findIndex(([n]) => n.id === el.dataset.id), [n] = rows[i], open = TREE.exp.has(n.id) && n.kids?.length;
  const sel = (j) => { if (!rows[j]) return; TREE.sel = rows[j][0].id; renderTree(); side.querySelector('[aria-selected="true"]').focus(); };
  const act = {
    ArrowDown: () => sel(i + 1), ArrowUp: () => sel(i - 1), Home: () => sel(0), End: () => sel(rows.length - 1),
    ArrowRight: () => (n.kids?.length ? (open ? sel(i + 1) : toggle(n.id)) : null),
    ArrowLeft: () => (open ? toggle(n.id) : n.parent && sel(rows.findIndex(([m]) => m === n.parent))),
    Enter: () => openNode(n.id),
  }[e.key];
  if (act) { e.preventDefault(); act(); }
  else if ((e.key === 'F10' && e.shiftKey) || e.key === 'ContextMenu') { e.preventDefault(); const r = el.getBoundingClientRect(); menu(n, ...local(r.left + 60, r.bottom)); }
}

// ---------------------------------------------------------------- Context menu.
const MENU = [['Open data', 'data'], ['Open structure', 'structure'], ['Properties'], ['New query here', 'query'], ['Read data in SQL console', 'query'], 0,
  ['Export...', 'export'], ['Import CSV...'], 0, ['Generate SELECT', 'select'], ['Generate INSERT'], ['Generate UPDATE'], ['Generate DELETE'], 0,
  ['Add column...', 'addcol'], 0, ['Show DDL'], ['Copy name', 'copy'], ['Copy qualified name', 'copyq'], 0, ['Rename...'], ['Drop...'], ['Drop with cascade...'], 0,
  ['Tools: backup...', 'tools'], ['Filter', null, 1], 0, ['Refresh', 'refresh']];
let menuFor = null, menuReturn = null;
function menu(n, x, y, quiet) {
  if (!n?.table) return;
  menuFor = n.table; menuReturn = document.activeElement;
  layer.querySelector('.k-menu')?.remove();
  layer.insertAdjacentHTML('beforeend', `<div class="k-menu" role="menu" aria-label="${esc(n.table)}" style="left:${Math.min(x, parseFloat(root.style.width) - 210)}px;top:${y}px">`
    + MENU.filter((m, i, a) => (m ? m[1] : a.slice(0, i).reverse().find((x) => !x || x[1]) && a.slice(i + 1).some((x) => x && x[1]))).map((m) => (m ? `<button type="button" role="menuitem" tabindex="-1"${m[1] ? ` data-a="menu" data-v="${m[1]}"` : ' aria-disabled="true"'}>${m[0]}${m[2] ? ic('chev') : ''}</button>` : '<hr>')).join('') + '</div>');
  const el = layer.querySelector('.k-menu'); // keep it inside the window, under the title bar
  el.style.top = `${Math.max(36, Math.min(y, parseFloat(root.style.height) - el.offsetHeight - 8))}px`;
  if (!quiet) el.querySelector('[data-a]').focus({ preventScroll: true });
}
function closeMenu(refocus) { const m = layer.querySelector('.k-menu'); if (!m) return; m.remove(); if (refocus && root.contains(menuReturn)) menuReturn.focus({ preventScroll: true }); }
function menuAct(v) {
  const t = menuFor; closeMenu(true);
  const cols = T[t].cols.map((c) => c.n), sel = `SELECT\n    ${cols.join(',\n    ')}\nFROM shop.${t}\nLIMIT 200\n`;
  ({
    data: () => openTable(t),
    structure: () => go('structure', { view: designModel(t) }),
    query: () => go('editor', { view: queryModel(`select * from shop.${t}\n`) }, '.k-ta'),
    select: () => go('editor', { view: queryModel(sel) }, '.k-ta'),
    export: () => go('export', { view: dataModel(t), dlg: exportModel(t) }),
    addcol: () => { const v = designModel(t); v.cols.push({ id: v.seq++, n: '', d: 'text', nul: true, pk: false, auto: false, def: '', cm: '' }); go('structure', { view: v }, `[data-m="cols.${v.cols.length - 1}.n"]`); },
    copy: () => copy(t), copyq: () => copy(`shop.${t}`),
    tools: () => go('backup', { view: st().view.kind === 'query' ? st().view : queryModel(Q2), dlg: backupModel() }),
    refresh: () => note('Refreshed. The sample data lives in this page.'),
  })[v]();
}
const copy = (s) => { navigator.clipboard?.writeText(s).then(() => note(`Copied “${s}”.`), () => note('The browser did not allow copying.')); };

// Switch feature from inside the window: the list follows, and the model given is kept.
function go(name, model, focusSel) {
  S[name] = { ...model, touched: true };
  const r = document.querySelector(`input[name="feat"][value="${name}"]`);
  if (r) { r.checked = true; r.dispatchEvent(new Event('change', { bubbles: true })); } else show(name, false);
  if (focusSel) root.querySelector(focusSel)?.focus({ preventScroll: true }); else if (st().dlg) focusDlg();
}

// ---------------------------------------------------------------- Views.
function renderView() {
  const v = st().view;
  const tabs = v.kind === 'data' ? ['Query 1', v.table + (pendCount(v) ? ' •' : '')] : v.kind === 'design' ? ['Query 1', `${v.table} · edit •`] : [v.plan || v.sql === QX ? 'Query 2' : 'Query 1'];
  root.querySelector('.k-tabs').innerHTML = tabs.map((t, i) => `<div class="k-tab${i === tabs.length - 1 ? ' on' : ''}"><i class="k-dot"></i>${logo()}<b>${esc(t)}</b>${ic('x', 'x')}</div>`).join('') + ic('plus', 'add');
  main.innerHTML = { data: dataHTML, query: queryHTML, design: designHTML }[v.kind](v);
  if (v.kind === 'query') syncEditor(v);
}
const bar = (left, right = '') => `<div class="k-bar"><span class="grp">${left}</span><span class="sp"></span>${right}</div>`;
const gb = (label, a, dis, v = '') => `<button type="button" class="k-gbtn" ${a ? `data-a="${a}"` : ''}${v !== '' ? ` data-v="${v}"` : ''}${dis ? ' disabled' : ''}>${label}</button>`;

// A grid of rows; `ed` (a data model) makes it editable and marks pending changes.
function gridHTML(cols, rows, { ed, label, sort } = {}) {
  const w = cols.map((c, ci) => Math.min(300, Math.max(c.n.length * 7.4 + c.t.length * 6.4 + 40, ...rows.slice(0, 30).map((r) => fmt(r.v[ci], c.t).length * 7.55 + 12))));
  const head = cols.map((c, ci) => {
    const s = sort?.[0] === ci ? (sort[1] ? 'descending' : 'ascending') : 'none';
    return `<div role="columnheader" aria-sort="${s}"><button type="button" data-a="sort" data-v="${ci}" tabindex="-1"><span>${esc(c.n)}</span><small>${esc(c.t)}</small>${ic('sort')}</button></div>`;
  }).join('');
  const body = rows.map((r, ri) => {
    const cls = r.del ? ' del' : r.isNew ? ' new' : '';
    return `<div class="k-gr${cls}" role="row"><div class="k-rn" role="rowheader">${r.isNew ? '+' : ri + 1}</div>` + cols.map((c, ci) => {
      const v = r.v[ci], dflt = r.isNew && v === undefined, mod = r.mod?.has(ci);
      return `<div role="gridcell" tabindex="${ri === 0 && ci === 0 ? 0 : -1}" data-cell="${ri}:${ci}" class="${NUM.has(c.t) ? 'r' : ''}${mod ? ' mod' : ''}"${ed ? ' aria-readonly="false"' : ''}>${dflt ? '<span class="k-null">default</span>' : v === null ? '<span class="k-null">NULL</span>' : esc(fmt(v, c.t))}</div>`;
    }).join('') + '<div></div></div>';
  }).join('');
  return `<div class="k-grid" role="grid" aria-label="${esc(label)}" aria-rowcount="${rows.length + 1}" style="--cols:44px ${w.map((x) => `${Math.round(x)}px`).join(' ')} minmax(0,1fr)">`
    + `<div class="k-gr k-gh" role="row"><div></div>${head}<div></div></div>${body}</div>`;
}
const sorted = (rows, sort) => !sort ? rows : [...rows].sort((a, b) => { const x = a.v[sort[0]], y = b.v[sort[0]]; const d = x === y ? 0 : x === null ? 1 : y === null ? -1 : cmp(x, y); return sort[1] ? -d : d; });

// Data tab: the table's rows with the visitor's pending edits on top.
const pendCount = (m) => Object.keys(m.pend).length + m.del.length + m.add.length;
function rowsOf(m) {
  const t = T[m.table];
  let base = t.rows;
  if (m.where.trim()) { try { const ids = new Set(execSql(`select id from shop.${m.table}`, m.where).rows.map((r) => r[0])); base = base.filter((r) => ids.has(r[0])); m.err = ''; } catch (e) { m.err = e.message; } }
  const rows = base.map((r) => {
    const v = [...r], mod = new Set();
    t.cols.forEach((c, ci) => { const key = `${r[0]}|${c.n}`; if (key in m.pend) { v[ci] = m.pend[key]; mod.add(ci); } });
    return { id: r[0], v, mod, del: m.del.includes(r[0]) };
  });
  return sorted(rows, m.sort).concat(m.add.map((a, i) => ({ id: `n${i}`, isNew: true, v: t.cols.map((c) => (c.n in a ? a[c.n] : undefined)), mod: new Set() })));
}
function dataHTML(m) {
  const n = pendCount(m), rows = rowsOf(m), cols = T[m.table].cols;
  return bar(`<span class="lbl">${logo()}<span class="k-mono">shop.${m.table}</span></span>${gb('Refresh', 'refresh')}${gb('Add row', 'addrow')}${gb('Delete', 'delrow')}${gb('Set NULL', 'setnull')}${gb('Duplicate', 0, 1)}${gb('Set default', 0, 1)}`,
    n ? `<span class="k-pend">${n} pending change${n > 1 ? 's' : ''}</span>${gb('Discard', 'discard')}${gb('Review SQL', 'review')}<button type="button" class="k-run" data-a="apply">Apply</button>` : '')
    + `<div class="k-filter">${gb('Add filter', 0, 1)}<span class="w">WHERE</span><input data-a="where" value="${esc(m.where)}" placeholder="SQL condition, for example: stock > 10 AND is_active = true" aria-label="Filter rows: a SQL condition"><span class="k-gbtn k-mut" aria-hidden="true">History${ic('down', 's')}</span><button type="button" class="k-bbtn" data-a="filterrun">Filter</button></div>`
    + gridHTML(cols, rows, { ed: m, label: `shop.${m.table}, editable`, sort: m.sort })
    + `<div class="k-foot"><span>Rows 1–${rows.length} of ${rows.length}</span>${m.err ? `<span style="color:var(--k-dan)">${esc(m.err)}</span>` : '<span class="k-mut">Double-click a cell to edit it.</span>'}<span class="sp" style="flex:1"></span>${gb('Export...', 'openexport')}</div>`;
}
const dataRow = (m, ri) => rowsOf(m)[ri];
function startEdit(cell) {
  const v = st().view; if (v.kind !== 'data') return;
  const [ri, ci] = cell.dataset.cell.split(':').map(Number), r = dataRow(v, ri), c = T[v.table].cols[ci];
  if (r.del) return note('This row is marked for deletion.');
  const val = r.v[ci];
  cell.innerHTML = `<input value="${val === null || val === undefined ? '' : esc(fmt(val, c.t))}" aria-label="${esc(c.n)}, row ${ri + 1}" data-edit="${ri}:${ci}">`;
  const inp = cell.firstChild; inp.focus({ preventScroll: true }); inp.select();
}
function commitEdit(inp, save) {
  const v = st().view, [ri, ci] = inp.dataset.edit.split(':').map(Number), r = dataRow(v, ri), c = T[v.table].cols[ci];
  inp.removeAttribute('data-edit');
  if (save) {
    let x = inp.value;
    if (NUM.has(c.t) && /^-?\d+(\.\d+)?$/.test(x.trim())) x = +x;
    else if (c.t === 'bool' && /^(true|false)$/i.test(x.trim())) x = x.trim().toLowerCase() === 'true';
    if (r.isNew) v.add[+r.id.slice(1)][c.n] = x;
    else { const key = `${r.id}|${c.n}`, orig = T[v.table].rows.find((o) => o[0] === r.id)[ci]; if (x === orig || fmt(x, c.t) === fmt(orig, c.t) && typeof x === typeof orig) delete v.pend[key]; else v.pend[key] = x; }
  }
  renderView(); renderStatus();
  root.querySelector(`[data-cell="${ri}:${ci}"]`)?.focus({ preventScroll: true });
}
function stmts(m) {
  const t = T[m.table], ty = (n) => t.cols.find((c) => c.n === n).t, out = [], rows = {};
  for (const [key, v] of Object.entries(m.pend)) { const [id, c] = key.split('|'); if (!m.del.includes(+id)) (rows[id] ??= []).push(`${c} = ${lit(v, ty(c))}`); }
  for (const [id, sets] of Object.entries(rows)) out.push(`UPDATE shop.${m.table} SET ${sets.join(', ')} WHERE id = ${id};`);
  for (const id of m.del) out.push(`DELETE FROM shop.${m.table} WHERE id = ${id};`);
  for (const a of m.add) { const ks = Object.keys(a); out.push(ks.length ? `INSERT INTO shop.${m.table} (${ks.join(', ')}) VALUES (${ks.map((c) => lit(a[c], ty(c))).join(', ')});` : `INSERT INTO shop.${m.table} DEFAULT VALUES;`); }
  return out;
}
function applyEdits(m) {
  const t = T[m.table], n = pendCount(m);
  for (const [key, v] of Object.entries(m.pend)) { const [id, c] = key.split('|'); const r = t.rows.find((o) => o[0] === +id); if (r) r[t.cols.findIndex((x) => x.n === c)] = v; }
  t.rows = t.rows.filter((r) => !m.del.includes(r[0]));
  for (const a of m.add) t.rows.push(t.cols.map((c, i) => (i === 0 ? Math.max(0, ...t.rows.map((r) => r[0])) + 1 : c.n in a ? a[c.n] : null)));
  m.pend = {}; m.del = []; m.add = [];
  note(`Applied ${n} change${n > 1 ? 's' : ''} in one transaction, to this page's sample data.`);
}

// Query tab: editor, then results (or the plan).
function queryHTML(m) {
  return `<div class="k-bar"><span class="k-pick">${logo()}Shop (demo)${ic('down', 's')}</span><span class="k-pick">kerno${ic('down', 's')}</span><span class="k-pick">default schema${ic('down', 's')}</span><span class="k-pick">Auto-commit${ic('down', 's')}</span><span class="sp"></span>`
    + `<span class="k-pick k-mut" aria-hidden="true">${ic('clock')}</span><span class="k-pick k-mut" aria-hidden="true">${ic('mark')}</span>${gb(`Explain${ic('down', 's')}`, 'explain')}${gb('Run all', 'run')}<button type="button" class="k-run" data-a="run" title="Run (⌘↵ or Ctrl+Enter)">Run</button><span class="k-pick">${ic('down', 's')}</span></div>`
    + `<div class="k-ed${m.plan ? ' short' : ''}"><div class="k-gut" aria-hidden="true"></div><div class="k-code"><div class="k-al"></div><pre class="k-hl" aria-hidden="true"></pre>`
    + `<textarea class="k-ta" spellcheck="false" autocapitalize="off" autocomplete="off" aria-label="SQL editor. Ctrl+Enter or Command+Enter runs it." aria-autocomplete="list" aria-controls="kui-pop">${esc(m.sql)}</textarea>`
    + '<ul class="k-pop" id="kui-pop" role="listbox" aria-label="Completions" hidden></ul></div></div>'
    + `<div class="k-res">${resHTML(m)}</div>`;
}
function resHTML(m) {
  if (m.plan) return planHTML(m);
  const r = m.res, foot = (left) => `<div class="k-foot">${left}<span class="sql">${esc(m.sql.replace(/\s+/g, ' ').trim())}</span></div>`;
  if (r.err || r.info) return `<div class="k-rt"><span>${r.err ? 'Error 1' : 'Result 1'}</span></div><div class="k-msg${r.err ? ' err' : ' k-mut'}" role="alert" style="flex:1">${esc(r.err || r.info)}</div>${r.err ? foot('') : ''}`;
  const rows = sorted(r.rows.map((v) => ({ v })), m.sort);
  return `<div class="k-rt"><span>Result 1</span></div><div class="k-filter"><span class="w">WHERE</span><input data-a="rwhere" value="${esc(m.where)}" placeholder="Filter the result on the server: a SQL condition, for example total > 100" aria-label="Filter the result: a SQL condition, Enter runs it"><span class="k-gbtn k-mut" aria-hidden="true">History${ic('down', 's')}</span><button type="button" class="k-bbtn" data-a="run">Filter</button></div>`
    + gridHTML(r.cols, rows, { label: 'Query result', sort: m.sort })
    + foot(`<span>${rows.length} row${rows.length === 1 ? '' : 's'}</span><span class="k-mut">Auto-refresh</span><span class="k-mut">Record</span>${r.multi ? '<span class="k-mut">Read-only result: its columns come from several tables.</span>' : ''}`);
}
function planHTML(m) {
  const head = '<div class="k-rt"><span>Plan 1</span></div>'
    + `<div class="k-seg" role="group" aria-label="Plan view"><button type="button" data-a="raw" data-v="0" aria-pressed="${!m.raw}">Plan</button><button type="button" data-a="raw" data-v="1" aria-pressed="${m.raw}">Raw output</button></div>`;
  if (m.raw) return head + `<pre class="k-praw">${esc(JSON.stringify([{ Plan: rawPlan(), 'Planning Time': 1.42, 'Execution Time': 16.3 }], null, 2))}</pre>`;
  const hidden = new Set(), exec = 16.3; // bars: own time against the execution time
  const rows = PLAN.map((r, i) => {
    if (hidden.has(i)) return '';
    const [d, step, rel, cost, est, n, own, total, det] = r, kk = kids(i), shut = m.shut.has(i);
    if (shut) { for (let j = i + 1; j < PLAN.length && PLAN[j][0] > d; j++) hidden.add(j); }
    return `<div class="k-pr${i === SLOW ? ' slow' : ''}" style="--d:${d}" role="row"><span class="st" role="cell">${kk.length ? `<button type="button" data-a="fold" data-v="${i}" aria-expanded="${!shut}" aria-label="${shut ? 'Expand' : 'Collapse'} ${step}">${ic(shut ? 'chev' : 'down')}</button>` : ''}<span>${step}</span>${rel ? `<code>${rel}</code>` : ''}${i === SLOW ? '<em>slowest step</em>' : ''}</span>`
      + [cost, est.toLocaleString('en-US'), n.toLocaleString('en-US'), `${own} ms`, `${total} ms`].map((x) => `<span role="cell">${x}</span>`).join('')
      + `<span class="bar" role="cell" aria-label="${Math.round((own / exec) * 100)}% of the execution time"><i style="width:${(own / exec) * 100}%"></i></span>`
      + (det ? `<span class="det" role="cell">${esc(det)}</span>` : '') + '</div>';
  }).join('');
  return head + `<div class="k-plan" role="table" aria-label="Query plan"><div class="k-ph" role="row"><span role="columnheader">Step</span><span role="columnheader">Est. cost</span><span role="columnheader">Est. rows</span><span role="columnheader">Rows</span><span role="columnheader">Own time</span><span role="columnheader">Total time</span><span></span></div>${rows}</div>`
    + `<div class="k-ptot">planning 1.42 ms · execution 16.3 ms</div><div class="k-foot"><span class="sql">${esc(norm(m.sql) === norm(QX) ? QX.replace(/\s+/g, ' ').trim() : '')}</span></div>`;
}
const refreshRes = (m) => { keep(() => { main.querySelector('.k-res').innerHTML = resHTML(m); }); renderStatus(); };

// ---------------------------------------------------------------- Editor: highlight and completion.
const KW = new Set('select from where join inner left right outer full cross on and or not null is in like ilike between order by group having limit offset as distinct case when then else end union all exists true false insert into values update set delete returning with'.split(' '));
function hl(src) {
  let out = '';
  for (const m of src.matchAll(/--[^\n]*|'(?:[^']|'')*'?|\d+(?:\.\d+)?|[A-Za-z_]\w*|[^A-Za-z_\d'-]+|./g)) {
    const s = m[0], i = m.index, l = s.toLowerCase(); let c = '';
    if (s.startsWith('--')) c = 'cm';
    else if (s[0] === "'") c = 'st';
    else if (/^\d/.test(s)) c = 'nu';
    else if (/^[A-Za-z_]/.test(s)) {
      if (l === 'asc' || l === 'desc') c = 'ty';
      else if (l === 'true' || l === 'false') c = 'nu';
      else if (KW.has(l)) c = 'kw';
      else if (/^\s*\(/.test(src.slice(i + s.length))) c = 'fn';
      else if ((src[i + s.length] === '.' && l !== 'shop') || /shop\.$/i.test(src.slice(Math.max(0, i - 5), i))) c = 'ty';
    }
    out += c ? `<span class="k-${c}">${esc(s)}</span>` : esc(s);
  }
  return out + '\n'; // keeps the height of a last empty line
}
const ta = () => main.querySelector('.k-ta');
function syncEditor(m) {
  const t = ta(); if (!t) return;
  main.querySelector('.k-hl').innerHTML = hl(t.value);
  const lines = t.value.split('\n').length, line = t.value.slice(0, t.selectionStart).split('\n').length - 1;
  const gut = main.querySelector('.k-gut');
  if (gut.childElementCount !== lines || gut.dataset.on !== String(line)) { gut.innerHTML = Array.from({ length: lines }, (_, i) => `<div${i === line ? ' class="on"' : ''}>${i + 1}</div>`).join(''); gut.dataset.on = line; }
  scrollSync(m);
}
function scrollSync() {
  const t = ta(); if (!t) return;
  const h = main.querySelector('.k-hl'); h.scrollTop = t.scrollTop; h.scrollLeft = t.scrollLeft;
  main.querySelector('.k-gut').style.transform = `translateY(${-t.scrollTop}px)`;
  const line = +main.querySelector('.k-gut').dataset.on || 0;
  main.querySelector('.k-al').style.top = `${6 + line * 19 - t.scrollTop}px`;
  const pop = main.querySelector('.k-pop'); if (!pop.hidden) place(pop);
}
let comp = { items: [], i: 0, from: 0 };
function bindings(sql) { // alias -> table, from the FROM and JOIN clauses
  const b = {};
  for (const m of sql.matchAll(/\b(?:from|join)\s+(?:shop\.)?([a-z_]\w*)(?:\s+(?:as\s+)?([a-z_]\w*))?/gi)) {
    const t = m[1].toLowerCase(); if (!T[t]) continue;
    b[t] = t; if (m[2] && !RESERVED.has(m[2].toLowerCase())) b[m[2].toLowerCase()] = t;
  }
  return b;
}
function complete(force) {
  const t = ta(), pop = main.querySelector('.k-pop'), before = t.value.slice(0, t.selectionStart);
  const m = /(?:([A-Za-z_]\w*)\.)?([A-Za-z_]\w*)?$/.exec(before), q = m[1]?.toLowerCase(), pre = m[2] || '';
  if (!force && !q && !pre) return closePop();
  const b = bindings(t.value), lp = pre.toLowerCase();
  let items;
  if (q === 'shop') items = Object.keys(T).map((n) => [n, 'table']);
  else if (q) items = b[q] ? T[b[q]].cols.map((c) => [c.n, c.t]) : [];
  else {
    const up = /^[A-Z]/.test(pre);
    items = [...new Set(Object.values(b))].flatMap((tb) => T[tb].cols.map((c) => [c.n, `${tb}.${c.t}`]))
      .concat([['shop', 'schema']], [...KW].map((w) => [up ? w.toUpperCase() : w, 'keyword']));
  }
  items = items.filter(([n]) => n.toLowerCase().startsWith(lp) && n.toLowerCase() !== lp).filter((x, i, a) => a.findIndex((y) => y[0] === x[0]) === i).slice(0, 40);
  if (!items.length) return closePop();
  comp = { items, i: 0, from: t.selectionStart - pre.length };
  pop.innerHTML = items.map(([n, d], i) => `<li role="option" id="kui-o${i}" aria-selected="${i === 0}" data-a="pick" data-v="${i}">${esc(n)}<span>${esc(d)}</span></li>`).join('');
  pop.hidden = false; t.setAttribute('aria-expanded', 'true'); t.setAttribute('aria-activedescendant', 'kui-o0');
  place(pop);
}
function place(pop) { // monospace: the caret's column and line give the popup's place
  const t = ta(), before = t.value.slice(0, comp.from), line = before.split('\n').length - 1, colN = before.length - before.lastIndexOf('\n') - 1;
  pop.style.left = `calc(${colN}ch + ${9 - t.scrollLeft}px)`;
  pop.style.top = `${6 + (line + 1) * 19 - t.scrollTop + 2}px`;
}
function closePop() { const pop = main.querySelector('.k-pop'); if (pop) pop.hidden = true; ta()?.setAttribute('aria-expanded', 'false'); ta()?.removeAttribute('aria-activedescendant'); }
function pick(i) {
  const t = ta(); t.focus({ preventScroll: true });
  t.setRangeText(comp.items[i][0], comp.from, t.selectionStart, 'end');
  closePop(); edited();
}
function moveSel(d) {
  const pop = main.querySelector('.k-pop'); comp.i = (comp.i + d + comp.items.length) % comp.items.length;
  pop.querySelectorAll('li').forEach((li, j) => li.setAttribute('aria-selected', j === comp.i));
  pop.children[comp.i].scrollIntoView({ block: 'nearest' }); ta().setAttribute('aria-activedescendant', `kui-o${comp.i}`);
}
function edited() { const m = st().view; m.sql = ta().value; syncEditor(m); }
function runNow() { const m = st().view; closePop(); m.plan = false; m.where = main.querySelector('[data-a="rwhere"]')?.value ?? m.where; run(m); refreshRes(m); }

// ---------------------------------------------------------------- Table designer.
const qi = (s) => `"${s.replace(/"/g, '""')}"`, qs = (s) => `'${s.replace(/'/g, "''")}'`;
function ddl(m) {
  const tb = `ALTER TABLE ${qi('shop')}.${qi(m.table)}`, out = [], at = (c) => `${qi('shop')}.${qi(m.table)}.${qi(c.n)}`;
  for (const o of m.orig) {
    const c = m.cols.find((x) => x.id === o.id);
    if (!c) { out.push(`${tb} DROP COLUMN ${qi(o.n)};`); continue; }
    if (!c.n.trim()) { out.push(`-- Column ${o.n} needs a name.`); continue; }
    if (c.n !== o.n) out.push(`${tb} RENAME COLUMN ${qi(o.n)} TO ${qi(c.n)};`);
    if (c.d !== o.d) out.push(`${tb} ALTER COLUMN ${qi(c.n)} TYPE ${c.d.trim() || 'text'};`);
    if (c.nul !== o.nul) out.push(`${tb} ALTER COLUMN ${qi(c.n)} ${c.nul ? 'DROP' : 'SET'} NOT NULL;`);
    if (c.def !== o.def) out.push(`${tb} ALTER COLUMN ${qi(c.n)} ${c.def.trim() ? `SET DEFAULT ${c.def.trim()}` : 'DROP DEFAULT'};`);
    if (c.auto !== o.auto) out.push(`${tb} ALTER COLUMN ${qi(c.n)} ${c.auto ? 'ADD GENERATED BY DEFAULT AS IDENTITY' : 'DROP IDENTITY'};`);
    if (c.cm !== o.cm) out.push(`COMMENT ON COLUMN ${at(c)} IS ${c.cm ? qs(c.cm) : 'NULL'};`);
  }
  for (const c of m.cols.filter((x) => !m.orig.some((o) => o.id === x.id))) {
    if (!c.n.trim()) { out.push('-- The new column needs a name.'); continue; }
    out.push(`${tb} ADD COLUMN ${qi(c.n)} ${c.d.trim() || 'text'}${c.auto ? ' GENERATED BY DEFAULT AS IDENTITY' : ''}${c.nul ? '' : ' NOT NULL'}${c.def.trim() ? ` DEFAULT ${c.def.trim()}` : ''};`);
    if (c.cm) out.push(`COMMENT ON COLUMN ${at(c)} IS ${qs(c.cm)};`);
  }
  const pk0 = m.orig.filter((c) => c.pk).map((c) => c.id).join(), pk1 = m.cols.filter((c) => c.pk);
  if (pk0 !== pk1.map((c) => c.id).join()) {
    if (pk0) out.push(`${tb} DROP CONSTRAINT ${qi(`${m.table}_pkey`)};`);
    if (pk1.length) out.push(`${tb} ADD PRIMARY KEY (${pk1.map((c) => qi(c.n)).join(', ')});`);
  }
  if (m.comment) out.push(`COMMENT ON TABLE ${qi('shop')}.${qi(m.table)} IS ${qs(m.comment)};`);
  if (m.name.trim() && m.name !== m.table) out.push(`${tb} RENAME TO ${qi(m.name.trim())};`);
  return out.join('\n') || '-- No changes.';
}
const TYPES = ['bigint', 'integer', 'smallint', 'numeric(10,2)', 'numeric(6,2)', 'real', 'text', 'character varying(255)', 'character(3)', 'boolean', 'date', 'timestamp with time zone', 'timestamp without time zone', 'jsonb', 'uuid', 'text[]'];
function designHTML(m) {
  const ck = (i, f, label) => `<label class="k-ck"><input type="checkbox" data-m="cols.${i}.${f}"${m.cols[i][f] ? ' checked' : ''}>${label}</label>`;
  const rows = m.cols.map((c, i) => `<div class="k-dr"><button type="button" data-a="dropcol" data-v="${i}" aria-label="Remove column ${esc(c.n || i + 1)}">${ic('x')}</button>`
    + `<input class="k-in" data-m="cols.${i}.n" value="${esc(c.n)}" aria-label="Column ${i + 1} name" placeholder="name"><input class="k-in" data-m="cols.${i}.d" value="${esc(c.d)}" list="kui-types" aria-label="Column ${i + 1} type">${ic('down')}`
    + `${ck(i, 'nul', 'NULL')}<input class="k-in" data-m="cols.${i}.def" value="${esc(c.def)}" placeholder="default" aria-label="Column ${i + 1} default">${ck(i, 'pk', 'PK')}${ck(i, 'auto', 'Auto')}`
    + `<input class="k-in" data-m="cols.${i}.cm" value="${esc(c.cm)}" placeholder="comment" aria-label="Column ${i + 1} comment"></div>`).join('');
  return `<div class="k-bar"><span class="lbl">${logo()}<span class="k-mono">shop.</span></span><input class="k-in" style="width:180px" data-m="name" value="${esc(m.name)}" aria-label="Table name">`
    + `<input class="k-in" style="flex:1;margin-left:6px" data-m="comment" value="${esc(m.comment)}" placeholder="Comment" aria-label="Table comment">`
    + `${gb('Add column', 'addcolumn')}${gb('Add index', 0, 1)}${gb('Add foreign key', 0, 1)}<button type="button" class="k-run" data-a="applyddl">Apply</button></div>`
    + `<div class="k-des"><h4>Columns</h4><div class="k-dh"><span></span><span>Name</span><span>Type</span><span>Default</span><span>Comment</span></div>${rows}`
    + `<p class="k-mut">Columns keep their order on the server; new ones are added at the end.</p>`
    + `<h4>Indexes</h4><p>No index. “Add index” adds one.</p><p>The indexes of the primary key and of unique constraints, and partial, expression or non-B-tree indexes, are not listed and stay as they are.</p>`
    + `<h4>Foreign keys</h4><p>No foreign key. “Add foreign key” adds one.</p><p>Check constraints and triggers are not in this form and stay as they are.</p>`
    + `</div><div class="k-ddlbox"><h4 style="margin:10px 0 6px;font-size:12px;font-weight:600">DDL preview</h4><pre class="k-ddl" aria-live="polite">${esc(ddl(m))}</pre></div>`
    + `<datalist id="kui-types">${TYPES.map((t) => `<option value="${t}">`).join('')}</datalist>`;
}

// ---------------------------------------------------------------- Dialogs.
const chips = (name, path, value, opts, cls = '', dis = false) => `<div class="k-chips ${cls}" role="radiogroup" aria-label="${name}">${opts.map(([v, l]) => `<label class="k-chip"><input type="radio" name="kui-${path}" data-m="${path}" value="${v}"${v === value ? ' checked' : ''}${dis ? ' disabled' : ''}><span>${l}</span></label>`).join('')}</div>`;
const field = (path, value, label, extra = '') => `<input class="k-in" data-m="${path}" value="${esc(value)}" aria-label="${label}" ${extra}>`;
const ckb = (path, on, label, extra = '') => `<label class="k-ck"><input type="checkbox" data-m="${path}"${on ? ' checked' : ''}${extra}>${label}</label>`;
const dlg = (id, title, body, w) => `<div class="k-dim"></div><div class="k-dlg" role="dialog" aria-modal="true" aria-labelledby="${id}" style="width:${w}px"><button type="button" class="k-close" data-a="close" aria-label="Close">${ic('x')}</button><h3 id="${id}">${title}</h3>${body}</div>`;
function renderLayer() {
  const d = st().dlg;
  layer.innerHTML = d ? { export: exportHTML, backup: backupHTML, conn: connHTML, review: reviewHTML }[d.kind](d) : '';
  const log = layer.querySelector('.k-log.out'); if (log) log.scrollTop = log.scrollHeight;
  win.inert = !!d;
}

// Export: format and options, with a quiet preview of the first lines.
function exportText(m) {
  const t = T[m.table], rows = t.rows.slice(0, 3), names = t.cols.map((c) => c.n), eol = '\n';
  const cell = (v, c) => (v === null ? m.nul : fmt(v, c.t));
  const json = (r) => JSON.stringify(Object.fromEntries(t.cols.map((c, i) => [c.n, r[i]])));
  if (m.fmt === 'csv' || m.fmt === 'tsv') {
    const d = m.fmt === 'tsv' ? '\t' : m.delim || ',', q = (s) => (s.includes(d) || /["\n\r]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s);
    return (m.header ? [names.join(d)] : []).concat(rows.map((r) => r.map((v, i) => q(cell(v, t.cols[i]))).join(d))).join(eol) + `${eol}…`;
  }
  if (m.fmt === 'json') return `[${eol}${rows.map((r) => `  ${json(r)}`).join(`,${eol}`)},${eol}  …${eol}]`;
  if (m.fmt === 'ndjson') return rows.map(json).join(eol) + `${eol}…`;
  if (m.fmt === 'sql') return rows.map((r) => `INSERT INTO shop.${m.table} (${names.join(', ')}) VALUES (${r.map((v, i) => lit(v, t.cols[i].t)).join(', ')});`).join(eol) + `${eol}…`;
  return [`| ${names.join(' | ')} |`, `|${names.map(() => ' --- ').join('|')}|`, ...rows.map((r) => `| ${r.map((v, i) => cell(v, t.cols[i]).replace(/\|/g, '\\|')).join(' | ')} |`)].join(eol) + `${eol}…`;
}
function exportHTML(m) {
  const csv = m.fmt === 'csv';
  return dlg('kui-dt', `Export ${m.table}`, `<p>All rows of shop.${m.table}.</p><div class="k-dbody"><div class="k-form">`
    + `<span>Format</span>${chips('Format', 'fmt', m.fmt, [['csv', 'CSV'], ['tsv', 'TSV'], ['json', 'JSON array'], ['ndjson', 'NDJSON'], ['sql', 'SQL INSERT'], ['md', 'Markdown']])}`
    + `<span>Delimiter</span><div>${field('delim', csv ? m.delim : m.fmt === 'tsv' ? 'tab' : '', 'Delimiter', `style="width:200px"${csv ? '' : ' disabled'}`)}</div>`
    + `<span>NULL as</span><div>${field('nul', m.nul, 'NULL as', 'style="width:200px" placeholder="empty"')}</div>`
    + `<span>Lines</span><div class="k-chips">${ckb('header', m.header, 'Header with the column names', /csv|tsv/.test(m.fmt) ? '' : ' disabled')}${chips('Line ending', 'eol', m.eol, [['lf', 'LF'], ['crlf', 'CRLF']], 'sm')}</div>`
    + `<span>Encoding</span>${chips('Encoding', 'enc', m.enc, [['utf8', 'UTF-8'], ['bom', 'UTF-8 with BOM'], ['utf16', 'UTF-16 LE'], ['latin1', 'Latin-1']], 'sm')}`
    + `<span class="k-cap" style="align-self:start;padding-top:8px">Preview</span><pre class="k-prev" aria-label="Preview of the first lines">${esc(exportText(m))}</pre></div></div>`
    + `<div class="k-dfoot"><span class="k-note2" aria-live="polite">${esc(m.note)}</span><button type="button" class="k-btn3" data-a="close">Cancel</button><button type="button" class="k-btn" data-a="choosefile">Choose file...</button></div>`, 640);
}

// Backup wizard: objects, settings, then a replay of pg_dump's log.
const PGDUMP = '/opt/homebrew/Cellar/libpq/18.6_2/bin/pg_dump';
const backupTables = (m) => (m.schemas.shop ? Object.keys(T) : []).concat(m.schemas.analytics ? ['daily_revenue'] : [], m.views && m.schemas.shop ? ['order_totals'] : []);
const tableSchema = (t) => (t === 'daily_revenue' ? 'analytics' : 'shop');
function backupPath(m) {
  const d = new Date(), ts = `${d.getFullYear()}${pad(d.getMonth() + 1)}${pad(d.getDate())}${pad(d.getHours())}${pad(d.getMinutes())}`;
  const vars = { database: 'kerno', schema: Object.keys(m.schemas).find((s) => m.schemas[s]) || 'kerno', table: backupTables(m).find((t) => m.tables[t]) || '', host: '127.0.0.1', timestamp: ts, date: ts.slice(0, 8), year: ts.slice(0, 4), month: ts.slice(4, 6), day: ts.slice(6, 8), hour: ts.slice(8, 10), minute: ts.slice(10), 'connection.type': 'local' };
  return `${m.folder.replace(/\/$/, '')}/${m.pattern.replace(/\$\{([\w.]+)\}/g, (s, v) => vars[v] ?? s)}${{ p: '.sql', c: '.dump', d: '', t: '.tar' }[m.fmt]}`;
}
function backupCmd(m) {
  const picked = backupTables(m).filter((t) => m.tables[t]), args = [`PGSSLMODE=disable PGPASSWORD=••• ${PGDUMP} --no-password --host=127.0.0.1`, '--port=55432 --username=kerno --verbose --dbname=kerno', `--file=${backupPath(m)}`, `--format=${m.fmt}`];
  if (m.compress) args.push(`--compress=${m.compress}`); if (m.enc) args.push(`--encoding=${m.enc}`);
  if (m.content !== 'all') args.push(m.content === 'schema' ? '--schema-only' : '--data-only');
  [['inserts', '--inserts'], ['nopriv', '--no-privileges'], ['noowner', '--no-owner'], ['clean', '--clean'], ['ifexists', '--if-exists'], ['create', '--create']].forEach(([o, a]) => m.o[o] && (o !== 'ifexists' || m.o.clean) && args.push(a));
  if (!m.complete) {
    args.push('--strict-names');
    for (const s of Object.keys(m.schemas).filter((s) => m.schemas[s])) {
      const all = backupTables(m).filter((t) => tableSchema(t) === s);
      if (all.every((t) => m.tables[t])) args.push(`'--schema="${s}"'`); else all.filter((t) => m.tables[t]).forEach((t) => args.push(`'--table="${s}"."${t}"'`));
    }
  }
  if (m.extra.trim()) args.push(m.extra.trim());
  return args.join(' ');
}
function backupHTML(m) {
  const steps = ['1. Objects', '2. Settings', '3. Progress'].map((s, i) => `<li${m.step === i + 1 ? ' aria-current="step"' : ''}>${s}</li>`).join('');
  let body, foot;
  if (m.step === 1) {
    const ts = backupTables(m);
    body = `<div class="k-chips" style="gap:18px">${ckb('complete', m.complete, 'Complete backup (the whole database)')}${ckb('views', m.views, 'Show views')}</div>
      <div class="k-cols2"><div><div class="k-lh">Schemas<span class="sp"></span>${gb('All', 'allschemas', m.complete, 1)}${gb('None', 'allschemas', m.complete, 0)}</div><div class="k-box" role="group" aria-label="Schemas">${Object.keys(m.schemas).map((s) => ckb(`schemas.${s}`, m.complete || m.schemas[s], s, m.complete ? ' disabled' : '')).join('')}</div></div>
      <div><div class="k-lh">Tables<span class="sp"></span>${gb('All', 'alltables', m.complete, 1)}${gb('None', 'alltables', m.complete, 0)}</div><div class="k-box" role="group" aria-label="Tables">${ts.length ? ts.map((t) => ckb(`tables.${t}`, m.complete || m.tables[t] !== false, t, m.complete ? ' disabled' : '')).join('') : '<p class="k-mut" style="margin:4px">Check a schema to list its tables.</p>'}</div></div></div>`;
    foot = `<button type="button" class="k-btn2" disabled>Local client...</button><span class="sp"></span><button type="button" class="k-btn3" data-a="close">Cancel</button><button type="button" class="k-btn2" disabled>Back</button><button type="button" class="k-btn" data-a="step" data-v="2">Next</button>`;
  } else if (m.step === 2) {
    body = `<div class="k-form" style="grid-template-columns:clamp(96px, 22%, 150px) minmax(0, 1fr)"><span>Client</span><span>PostgreSQL 18.6 · Homebrew · ${PGDUMP}</span>
      <b class="full">Settings</b><span>Format</span>${chips('Format', 'fmt', m.fmt, [['p', 'Plain SQL'], ['c', 'Custom'], ['d', 'Directory'], ['t', 'Tar']])}
      <span>Compression</span><div class="k-row">${field('compress', m.compress, 'Compression', 'placeholder="default" style="width:110px"')}<span style="margin-left:10px">Encoding</span>${field('enc', m.enc, 'Encoding', 'placeholder="the server\'s" style="width:170px"')}</div>
      <span>Content</span>${chips('Content', 'content', m.content, [['all', 'Definitions and data'], ['schema', 'Definitions only'], ['data', 'Data only']])}
      <div class="full" style="display:grid;gap:7px">${ckb('o.inserts', m.o.inserts, 'Use SQL INSERT instead of COPY')}${ckb('o.nopriv', m.o.nopriv, 'Do not back up privileges (GRANT / REVOKE)')}${ckb('o.noowner', m.o.noowner, 'Discard objects owner')}${ckb('o.clean', m.o.clean, 'Add DROP statements before each CREATE (clean)')}<span style="padding-left:22px">${ckb('o.ifexists', m.o.ifexists, '... with IF EXISTS', m.o.clean ? '' : ' disabled')}</span>${ckb('o.create', m.o.create, 'Add the CREATE DATABASE statement')}</div>
      <b class="full">Output</b><span>Folder</span><div class="k-row">${field('folder', m.folder, 'Output folder', 'style="flex:1"')}<button type="button" class="k-btn2" disabled>Choose...</button></div>
      <span>File name</span>${field('pattern', m.pattern, 'File name pattern')}<span></span><span class="k-cap">Variables: \${database} \${schema} \${table} \${host} \${timestamp} \${date} \${year} \${month} \${day} \${hour} \${minute} \${connection.type}. The format's extension is added.</span>
      <span>Writes</span><span class="k-mono" data-writes>${esc(backupPath(m))}</span>
      <b class="full">Advanced</b><span>Extra arguments</span>${field('extra', m.extra, 'Extra arguments', 'placeholder="--name=value ..."')}</div>`;
    foot = `<button type="button" class="k-btn2" disabled>Local client...</button><span class="sp"></span><button type="button" class="k-btn3" data-a="close">Cancel</button><button type="button" class="k-btn2" data-a="step" data-v="1">Back</button><button type="button" class="k-btn2" disabled>Save as task...</button><button type="button" class="k-btn" data-a="startbackup">Start</button>`;
  } else {
    body = `${m.done ? `<p class="k-ok" style="margin:0">${m.done === 'cancel' ? 'Cancelled.' : `Demo run finished: nothing was written. In Kerno, pg_dump writes ${esc(backupPath(m))} and its exit status, size and time show here.`}</p>` : ''}
      <pre class="k-log">${esc(backupCmd(m))}</pre><pre class="k-log out" aria-live="polite" aria-label="pg_dump log">${esc(m.log.join('\n'))}</pre>`;
    foot = `<span class="sp"></span>${m.running ? '<button type="button" class="k-btn2" data-a="cancelbackup">Cancel</button>' : `<button type="button" class="k-btn2" data-a="copylog">Copy output</button><button type="button" class="k-btn2" disabled>Show in Finder</button><button type="button" class="k-btn2" data-a="step" data-v="2">Back</button><button type="button" class="k-btn" data-a="close">Close</button>`}`;
  }
  return dlg('kui-bt', 'Back up kerno', `<p>Back up the database “kerno” of “Shop (demo)” with the engine's dump tool, run on this computer.</p><div class="k-wiz"><ol class="k-steps">${steps}</ol><div class="k-wbody">${body}</div></div><div class="k-dfoot">${foot}</div>`, 860);
}
let replay;
function startBackup(m) {
  m.step = 3; m.running = true; m.done = false;
  const picked = m.content === 'schema' ? [] : backupTables(m).filter((t) => m.complete || m.tables[t]);
  const lines = ['-- Demo run: nothing is dumped. These lines replay the log of a real pg_dump run.', 'pg_dump: reading subscriptions', 'pg_dump: reading subscription membership of tables', 'pg_dump: reading dependency data', 'pg_dump: saving encoding = UTF8',
    'pg_dump: saving "standard_conforming_strings = on"', 'pg_dump: saving "search_path = "', 'pg_dump: saving database definition', ...picked.map((t) => `pg_dump: dumping contents of table "${tableSchema(t)}.${t}"`)];
  m.log = [];
  const tick = () => {
    if (!m.running) return;
    m.log.push(lines[m.log.length]);
    if (m.log.length >= lines.length) { m.running = false; m.done = 'ok'; }
    if (cur === 'backup' && st().dlg === m) keep(renderLayer);
    if (m.running) replay = setTimeout(tick, 160);
  };
  if (still) { m.log = lines; m.running = false; m.done = 'ok'; keep(renderLayer); } else tick();
}

// New connection, opened on the SSH tunnel tab.
// The app's own explanations (crates/kerno/src/connections/dialog.rs).
const SSL_TXT = { disable: 'Never use TLS.', prefer: 'Use TLS when the server offers it. The certificate is not checked.', require: 'Require TLS. The certificate is not checked.', 'verify-ca': 'Require TLS and a certificate signed by a trusted authority.', 'verify-full': 'As verify-ca, and the certificate must match the host name.' };
const COLOURS = ['transparent', '#E5484D', '#F2994A', '#E2B93B', '#4CC38A', '#3DB9A8', '#5AA9F0', '#9B7CF2', '#E879B6'];
function connHTML(m) {
  const lab = (t, f) => `<label class="k-fl">${t}${f}</label>`, row = (...x) => `<div class="k-row">${x.join('')}</div>`;
  const inp = (path, v, ph = '', ex = '') => `<input class="k-in" data-m="${path}" value="${esc(v)}" placeholder="${esc(ph)}" ${ex}>`;
  const L = (t, path, v, ph, ex = '', cls = 'grow') => `<div class="${cls}"><label class="k-fl" for="kui-${path}">${t}</label>${inp(path, v, ph, `id="kui-${path}" ${ex}`)}</div>`;
  const radios = (path, v, opts, dis = '') => `<div class="k-chips" role="radiogroup" style="gap:14px">${opts.map(([x, l]) => `<label class="k-ck"><input type="radio" name="kui-${path}" data-m="${path}" value="${x}"${x === v ? ' checked' : ''}${dis}>${l}</label>`).join('')}</div>`;
  const s = m.ssh, off = s.on ? '' : ' disabled', g = m.g;
  const panes = {
    general: row(L('Fill from a connection URL', 'g.url', g.url, 'postgres://user:password@host:5432/database'), `<button type="button" class="k-btn2" data-a="fillurl" style="align-self:end">Fill</button>`)
      + row(L('Name', 'g.name', g.name, 'A name for the sidebar'), `<div style="width:300px"><label class="k-fl" for="kui-folder">Folder</label><select class="k-in" id="kui-folder" data-m="g.folder"><option value="none">No folder</option><option value="local"${g.folder === 'local' ? ' selected' : ''}>Local</option></select></div>`)
      + row(`<div><span class="k-fl">Colour</span><div class="k-chips" role="radiogroup" aria-label="Colour" style="gap:7px">${COLOURS.map((c, i) => `<input type="radio" class="k-swatch" name="kui-colour" data-m="g.colour" value="${i}" style="--c:${i ? c : 'var(--k-bg)'};${i ? '' : 'border-color:var(--k-ibd)'}" aria-label="${i ? `Colour ${i}` : 'No colour'}"${+g.colour === i ? ' checked' : ''}>`).join('')}</div></div>`,
        `<div style="margin-left:24px"><span class="k-fl">Environment</span>${chips('Environment', 'g.env', g.env, [['local', 'Local'], ['staging', 'Staging'], ['production', 'Production']])}</div>`)
      + row(L('Host', 'g.host', g.host), L('Port', 'g.port', g.port, '', 'inputmode="numeric"', '" style="width:180px'))
      + row(L('User', 'g.user', g.user), `<div class="grow"><label class="k-fl" for="kui-pw">Password</label><div class="k-row"><input class="k-in grow" id="kui-pw" type="${g.show ? 'text' : 'password'}" data-m="g.password" value="${esc(g.password)}"><button type="button" class="k-btn3" data-a="showpw" aria-pressed="${g.show}" aria-label="Show password">${ic('eye')}</button></div></div>`)
      + `<div style="margin-top:10px">${ckb('g.remember', g.remember, 'Save passwords in the system keychain')}</div>`
      + row(L('Database', 'g.db', g.db, "the server's default (often postgres)"), `<button type="button" class="k-btn2" disabled style="align-self:end">List...</button>`)
      + `<label class="k-ck" style="margin-top:12px"><input type="checkbox" class="k-sw" data-m="g.ro"${g.ro ? ' checked' : ''}>Read-only: block writes here and open sessions read-only on the server</label>`,
    ssl: `<span class="k-fl">Mode</span>${radios('ssl.mode', m.ssl.mode, Object.keys(SSL_TXT).map((x) => [x, x]))}<p style="margin:10px 0 0">${SSL_TXT[m.ssl.mode]}</p>`
      + ['CA certificate:ca:System trust store when empty', 'Client certificate:cert:Client certificate (optional)', 'Client key:key:Client key (optional)'].map((x) => { const [t, f, ph] = x.split(':'); return row(L(t, `ssl.${f}`, m.ssl[f], ph), '<button type="button" class="k-btn2" disabled style="align-self:end">Browse...</button>'); }).join(''),
    ssh: `<label class="k-ck"><input type="checkbox" class="k-sw" data-m="ssh.on"${s.on ? ' checked' : ''}>Reach the database through an SSH server</label>`
      + row(L('SSH host', 'ssh.host', s.host, 'bastion.example.com', off), L('Port', 'ssh.port', s.port, '', `inputmode="numeric"${off}`, '" style="width:180px'))
      + L('SSH user', 'ssh.user', s.user, 'User on the SSH server', off, '')
      + `<span class="k-fl">Authentication</span>${radios('ssh.auth', s.auth, [['agent', 'SSH agent'], ['password', 'Password'], ['key', 'Key file']], off)}`
      + (s.auth === 'agent' ? L('Agent socket', 'ssh.socket', s.socket, 'SSH_AUTH_SOCK when empty', off, '')
        : s.auth === 'password' ? L('SSH password', 'ssh.password', s.password, '', `type="password"${off}`, '')
          : row(L('Private key file', 'ssh.key', s.key, '~/.ssh/id_ed25519', off), '<button type="button" class="k-btn2" disabled style="align-self:end">Browse...</button>') + L('Key passphrase', 'ssh.pass', s.pass, 'Asked at connect time when empty', `type="password"${off}`, ''))
      + L('Keep-alive (seconds, 0 for none)', 'ssh.keep', s.keep, '', `inputmode="numeric" style="width:240px"${off}`, ''),
    adv: L('Extra driver options', 'adv.opts', m.adv.opts, 'application_name=kerno', '', '') + L('Connect timeout (seconds)', 'adv.timeout', m.adv.timeout, '', 'inputmode="numeric" style="width:240px"', ''),
  };
  const engines = [['pg', 'PostgreSQL'], ['mysql', 'MySQL'], ['mariadb', 'MariaDB']].filter(([, n]) => n.toLowerCase().includes(m.search.toLowerCase()));
  const tabs = [['general', 'General'], ['ssl', 'SSL / TLS'], ['ssh', 'SSH tunnel'], ['adv', 'Advanced']];
  return dlg('kui-ct', 'New connection', `<div class="k-dbody k-cn"><input class="k-in" data-m="search" value="${esc(m.search)}" placeholder="Search engines" aria-label="Search engines" style="height:24px">`
    + `<div class="k-eng" role="radiogroup" aria-label="Engine">${engines.map(([v, n]) => `<label><input type="radio" name="kui-engine" data-m="engine" value="${v}"${m.engine === v ? ' checked' : ''}>${v === 'pg' ? logo('') : `<img class="k-eng-${v}" src="${ENGINE_LOGO[v]}" alt="" aria-hidden="true">`}${n}</label>`).join('') || '<p class="k-mut">No engine matches.</p>'}</div>`
    + `<div class="k-dtabs" role="tablist" aria-label="Connection settings">${tabs.map(([v, l]) => `<button type="button" role="tab" id="kui-tab-${v}" aria-controls="kui-pane" aria-selected="${m.tab === v}" tabindex="${m.tab === v ? 0 : -1}" data-a="ctab" data-v="${v}">${l}</button>`).join('')}</div>`
    + `<div id="kui-pane" role="tabpanel" aria-labelledby="kui-tab-${m.tab}">${panes[m.tab]}</div></div>`
    + `<div class="k-dfoot"><button type="button" class="k-btn2" data-a="testconn">Test connection</button><span class="k-note2" aria-live="polite">${esc(m.note)}</span><button type="button" class="k-btn3" data-a="close">Cancel</button><button type="button" class="k-btn2" data-a="saveconn">Save</button><button type="button" class="k-btn" data-a="saveconn">Save and connect</button></div>`, 720);
}
function reviewHTML(m) {
  const v = st().view;
  const n = pendCount(v);
  return dlg('kui-rt', `Apply ${n} change${n === 1 ? '' : 's'} to ${v.table}`, `<p>These statements run in one transaction: all of them take effect, or none does.</p><div class="k-dbody"><pre class="k-log" style="margin:0">${esc(stmts(v).join('\n'))}</pre></div>`
    + '<div class="k-dfoot"><span class="sp"></span><button type="button" class="k-btn3" data-a="close">Cancel</button><button type="button" class="k-btn" data-a="apply">Apply</button></div>', 620);
}

// ---------------------------------------------------------------- Events.
const ACT = {
  toggle: (el) => toggle(el.dataset.v),
  tools: () => { const s = st(); if (cur === 'backup') { s.dlg = backupModel(); render(); focusDlg(); } else go('backup', { view: s.view.kind === 'query' ? s.view : queryModel(Q2), dlg: backupModel() }); },
  newconn: () => { if (cur === 'ssh') { st().dlg = connModel('general'); render(); focusDlg(); } else go('ssh', { view: queryModel(Q2), dlg: connModel('general') }); },
  menu: (el) => menuAct(el.dataset.v),
  sort: (el) => { const v = st().view, c = +el.dataset.v; v.sort = v.sort?.[0] === c ? (v.sort[1] ? null : [c, 1]) : [c, 0]; v.kind === 'query' ? refreshRes(v) : keep(renderView); },
  refresh: () => { keep(renderView); note('Refreshed. The sample data lives in this page.'); },
  addrow: () => { const v = st().view; v.add.push({}); renderView(); renderStatus(); const r = rowsOf(v).length - 1; root.querySelector(`[data-cell="${r}:1"]`)?.focus({ preventScroll: true }); },
  delrow: () => { const v = st().view, c = cellAt(); if (!c) return note('Select a cell of the row to delete.'); const r = dataRow(v, c[0]); if (r.isNew) v.add.splice(+r.id.slice(1), 1); else v.del = v.del.includes(r.id) ? v.del.filter((x) => x !== r.id) : [...v.del, r.id]; keep(renderView); renderStatus(); },
  setnull: () => { const v = st().view, c = cellAt(); if (!c) return note('Select a cell first.'); const r = dataRow(v, c[0]), col = T[v.table].cols[c[1]]; if (col.nn) return note(`${col.n} is NOT NULL.`); if (r.isNew) v.add[+r.id.slice(1)][col.n] = null; else v.pend[`${r.id}|${col.n}`] = null; renderView(); renderStatus(); root.querySelector(`[data-cell="${c.join(':')}"]`)?.focus({ preventScroll: true }); },
  discard: () => { const v = st().view; v.pend = {}; v.del = []; v.add = []; renderView(); renderStatus(); note('Discarded the pending changes.'); },
  review: () => { const s = st(); s.dlg = { kind: 'review', prev: s.dlg }; renderLayer(); root.querySelector('.k-dlg [data-a="apply"]').focus({ preventScroll: true }); },
  apply: () => { const s = st(); applyEdits(s.view); if (s.dlg?.kind === 'review') s.dlg = s.dlg.prev; render(); },
  filterrun: () => { const v = st().view; v.where = root.querySelector('[data-a="where"]').value; keep(renderView); renderStatus(); },
  openexport: () => { const s = st(); s.dlg = exportModel(s.view.table); s.touched = true; renderLayer(); focusDlg(); },
  run: runNow,
  explain: () => { const m = st().view; closePop(); if (norm(m.sql) === norm(QX)) { m.plan = true; refreshRes(m); } else { m.plan = false; m.res = { info: 'This demo window explains one sample query: the one of “Query plans” in the list.' }; refreshRes(m); } },
  pick: (el) => pick(+el.dataset.v),
  raw: (el) => { st().view.raw = el.dataset.v === '1'; refreshRes(st().view); },
  fold: (el) => { const m = st().view, i = +el.dataset.v; m.shut.has(i) ? m.shut.delete(i) : m.shut.add(i); refreshRes(m); },
  dropcol: (el) => { const v = st().view; v.cols.splice(+el.dataset.v, 1); keep(renderView); },
  addcolumn: () => { const v = st().view; v.cols.push({ id: v.seq++, n: '', d: 'text', nul: true, pk: false, auto: false, def: '', cm: '' }); renderView(); root.querySelector(`[data-m="cols.${v.cols.length - 1}.n"]`).focus({ preventScroll: true }); },
  applyddl: () => { const v = st().view; if (/^-- No changes/.test(ddl(v))) return note('Nothing to apply.'); v.orig = v.cols.map((c) => ({ ...c })); if (v.name.trim()) v.table = v.name.trim(); keep(renderView); note('Applied to this demo form. Kerno runs the statements in one transaction.'); },
  close: () => closeDlg(),
  choosefile: () => { st().dlg.note = 'In Kerno this opens a save dialog, then streams every row to the file you choose.'; keep(renderLayer); },
  step: (el) => { const m = st().dlg; m.step = +el.dataset.v; renderLayer(); root.querySelector('.k-dlg .k-btn, .k-dlg [data-a="step"]')?.focus({ preventScroll: true }); },
  allschemas: (el) => { const m = st().dlg; for (const s in m.schemas) m.schemas[s] = el.dataset.v === '1'; keep(renderLayer); },
  alltables: (el) => { const m = st().dlg; for (const t of backupTables(m)) m.tables[t] = el.dataset.v === '1'; keep(renderLayer); },
  startbackup: () => { startBackup(st().dlg); root.querySelector('.k-dlg [data-a="cancelbackup"], .k-dlg .k-btn')?.focus({ preventScroll: true }); },
  cancelbackup: () => { const m = st().dlg; clearTimeout(replay); m.running = false; m.done = 'cancel'; renderLayer(); root.querySelector('.k-dlg .k-btn').focus({ preventScroll: true }); },
  copylog: () => copy([backupCmd(st().dlg), ...st().dlg.log].join('\n')),
  ctab: (el) => { st().dlg.tab = el.dataset.v; keep(renderLayer); root.querySelector(`#kui-tab-${el.dataset.v}`).focus({ preventScroll: true }); },
  testconn: () => { st().dlg.note = 'This demo window does not connect anywhere.'; keep(renderLayer); },
  saveconn: () => { st().dlg.note = 'Nothing is saved in this demo. Kerno keeps the connection, and the password in your keychain.'; keep(renderLayer); },
  showpw: () => { const g = st().dlg.g; g.show = !g.show; keep(renderLayer); },
  fillurl: () => {
    const g = st().dlg.g, m = /^postgres(?:ql)?:\/\/(?:([^:@/]*)(?::([^@/]*))?@)?([^:/?]+)?(?::(\d+))?(?:\/([^?]*))?/.exec(g.url.trim());
    if (!m) { st().dlg.note = 'That is not a postgres:// URL.'; } else { Object.assign(g, { user: m[1] ? decodeURIComponent(m[1]) : g.user, password: m[2] ? decodeURIComponent(m[2]) : g.password, host: m[3] || g.host, port: m[4] || g.port, db: m[5] ? decodeURIComponent(m[5]) : g.db }); st().dlg.note = 'Filled from the URL.'; }
    keep(renderLayer);
  },
};
const cellAt = () => { const c = main.querySelector('[data-cell]:focus, [data-cell][tabindex="0"]'); return c && c.dataset.cell.split(':').map(Number); };
function closeDlg() {
  const s = st(), d = s.dlg; if (!d) return;
  if (d.kind === 'backup') { clearTimeout(replay); d.running = false; }
  s.dlg = d.kind === 'review' ? d.prev : null; renderLayer();
  (main.querySelector('[data-a="review"], [data-a="openexport"]') || main.querySelector('.k-ta') || side.querySelector('[tabindex="0"]'))?.focus({ preventScroll: true });
}
function setPath(o, path, v) { const ks = path.split('.'), last = ks.pop(); for (const key of ks) o = o[key]; o[last] = v; }
function bound(el) { // a data-m control changed: write the model, then refresh what depends on it
  const inDlg = !!el.closest('.k-dlg'), m = inDlg ? st().dlg : st().view;
  let v = el.type === 'checkbox' ? el.checked : el.value;
  if (el.type === 'radio' && el.name === 'kui-colour') v = +v;
  setPath(m, el.dataset.m, v);
  if (m.kind === 'backup' && /^schemas\./.test(el.dataset.m) && v) backupTables(m).forEach((t) => { if (m.tables[t] === undefined) m.tables[t] = true; });
  const text = el.type !== 'checkbox' && el.type !== 'radio' && el.tagName !== 'SELECT';
  if (!text) return keep(inDlg ? renderLayer : renderView);
  if (m.kind === 'design') main.querySelector('.k-ddl').textContent = ddl(m);
  if (m.kind === 'export') layer.querySelector('.k-prev').textContent = exportText(m);
  if (m.kind === 'backup') { const w = layer.querySelector('[data-writes]'); if (w) w.textContent = backupPath(m); }
  if (m.kind === 'conn' && el.dataset.m === 'search') keep(renderLayer);
}

function wire() {
  const touch = () => { if (S[cur]) S[cur].touched = true; };
  ['pointerdown', 'keydown', 'input'].forEach((ev) => root.addEventListener(ev, touch, true));
  root.addEventListener('pointerdown', (e) => { if (!e.target.closest('.k-menu')) closeMenu(false); });
  root.addEventListener('mousedown', (e) => { if (e.target.closest('.k-pop')) e.preventDefault(); }); // keep the caret in the editor
  root.addEventListener('click', (e) => {
    const a = e.target.closest('[data-a]');
    if (a && a.tagName !== 'INPUT' && !a.disabled && ACT[a.dataset.a]) return ACT[a.dataset.a](a, e);
    const ti = e.target.closest('[role="treeitem"]');
    if (ti) { TREE.sel = ti.dataset.id; keep(renderTree); side.querySelector(`[data-id="${CSS.escape(ti.dataset.id)}"]`).focus({ preventScroll: true }); }
  });
  root.addEventListener('dblclick', (e) => {
    const ti = e.target.closest('[role="treeitem"]'); if (ti && !e.target.closest('[data-a]')) return openNode(ti.dataset.id);
    const cell = e.target.closest('[data-cell]'); if (cell && cell.closest('[aria-label*="editable"]') && !cell.querySelector('input')) startEdit(cell);
  });
  root.addEventListener('contextmenu', (e) => {
    const ti = e.target.closest('[role="treeitem"]'), n = ti && NODES.get(ti.dataset.id);
    if (n?.table) { e.preventDefault(); TREE.sel = n.id; renderTree(); menu(n, ...local(e.clientX, e.clientY)); }
  });
  root.addEventListener('input', (e) => {
    const el = e.target;
    if (el.classList.contains('k-ta')) { edited(); complete(false); return; }
    if (el.dataset.a === 'filter') { TREE.filter = el.value; renderTree(); return; }
    if (el.dataset.m && el.type !== 'checkbox' && el.type !== 'radio' && el.tagName !== 'SELECT') bound(el);
  });
  root.addEventListener('change', (e) => { const el = e.target; if (el.dataset.m && (el.type === 'checkbox' || el.type === 'radio' || el.tagName === 'SELECT')) bound(el); });
  root.addEventListener('scroll', (e) => { if (e.target.classList?.contains('k-ta')) scrollSync(); }, true);
  root.addEventListener('focusout', (e) => { if (e.target.dataset?.edit) commitEdit(e.target, true); if (e.target.classList?.contains('k-ta') && !e.relatedTarget?.closest?.('.k-pop')) setTimeout(() => { if (document.activeElement !== ta()) closePop(); }, 0); });
  ['keyup', 'click'].forEach((ev) => root.addEventListener(ev, (e) => { if (e.target.classList?.contains('k-ta')) syncEditor(st().view); }));
  root.addEventListener('keydown', onKey);
}
function onKey(e) {
  const el = e.target, mod = e.metaKey || e.ctrlKey;
  if (el.closest?.('.k-menu')) {
    const items = [...layer.querySelectorAll('.k-menu [data-a]')], i = items.indexOf(el);
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') { e.preventDefault(); items[(i + (e.key === 'ArrowDown' ? 1 : -1) + items.length) % items.length].focus(); }
    else if (e.key === 'Escape' || e.key === 'Tab') { e.preventDefault(); closeMenu(true); }
    return;
  }
  if (el.classList.contains('k-ta')) {
    const open = !main.querySelector('.k-pop').hidden;
    if (mod && e.key === 'Enter') { e.preventDefault(); runNow(); }
    else if (e.ctrlKey && e.key === ' ') { e.preventDefault(); complete(true); }
    else if (open && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) { e.preventDefault(); moveSel(e.key === 'ArrowDown' ? 1 : -1); }
    else if (open && (e.key === 'Enter' || e.key === 'Tab')) { e.preventDefault(); pick(comp.i); }
    else if (open && e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); closePop(); }
    return;
  }
  if (el.dataset.edit) { if (e.key === 'Enter') { e.preventDefault(); el.blur(); } else if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); commitEdit(el, false); } return; }
  if (e.key === 'Enter' && (el.dataset.a === 'where' || el.dataset.a === 'rwhere')) { e.preventDefault(); return el.dataset.a === 'where' ? ACT.filterrun() : runNow(); }
  if (e.key === 'Escape' && st().dlg) { e.preventDefault(); return closeDlg(); }
  if (el.getAttribute('role') === 'treeitem') return treeKey(e, el);
  if (el.getAttribute('role') === 'tab') {
    const tabs = [...el.parentNode.children], i = tabs.indexOf(el), d = { ArrowRight: 1, ArrowLeft: -1 }[e.key];
    if (d) { e.preventDefault(); ACT.ctab(tabs[(i + d + tabs.length) % tabs.length]); }
    return;
  }
  if (el.dataset.cell) {
    let [r, c] = el.dataset.cell.split(':').map(Number);
    const step = { ArrowDown: [1, 0], ArrowUp: [-1, 0], ArrowRight: [0, 1], ArrowLeft: [0, -1] }[e.key];
    if (step) { e.preventDefault(); const n = main.querySelector(`[data-cell="${r + step[0]}:${c + step[1]}"]`); if (n) { el.tabIndex = -1; n.tabIndex = 0; n.focus(); } }
    else if ((e.key === 'Enter' || e.key === 'F2') && el.closest('[aria-label*="editable"]')) { e.preventDefault(); startEdit(el); }
  }
}

// ---------------------------------------------------------------- Feature switching and mounting.
function show(name, byVisitor) {
  if (!INIT[name]) return;
  if (S[cur]?.dlg?.kind === 'backup' && name !== cur) { clearTimeout(replay); S[cur].dlg.running = false; }
  if (!S[name] || !S[name].touched) S[name] = INIT[name]();
  cur = name; closeMenu(false);
  const v = st().view; if (v.table && T[v.table]) TREE.sel = `t:${v.table}`;
  render();
  if (byVisitor && st().dlg) focusDlg();
  if (!st().touched) requestAnimationFrame(() => { if (cur === name && !st().touched) pose(name); });
}

// A state the visitor has not touched shows what its caption promises: the table's menu open in
// the navigator, the completion list open in the editor. Nothing takes the focus; a click closes it.
function pose(name) {
  if (name === 'navigator') {
    const el = side.querySelector('[aria-selected="true"]'), n = el && NODES.get(el.dataset.id);
    if (n?.table) { const r = el.getBoundingClientRect(), edge = side.closest('.k-side').getBoundingClientRect().right; menu(n, ...local(edge - 4, r.top - 4), true); }
  } else if (name === 'editor') {
    const t = ta(), at = t ? t.value.indexOf('o.status') + 3 : 0; // after "o.s": the columns of orders in s
    if (at > 2) { t.setSelectionRange(at, at); complete(true); }
  }
}

export function selfCheck() {
  const O = T.orders.rows, n = (q) => execSql(q).rows.length;
  const cases = [
    ['SELECT * FROM shop.customers', T.customers.rows.length],
    ["select id from shop.orders where status = 'shipped'", O.filter((r) => r[2] === 'shipped').length],
    [Q1, Math.min(50, O.length)],
    ["SELECT o.id FROM shop.orders o WHERE o.total > 100 AND o.currency = 'EUR'", O.filter((r) => r[5] > 100 && r[6] === 'EUR').length],
    ['SELECT * FROM shop.products ORDER BY price DESC LIMIT 5', 5],
    ['SELECT i.id, p.name FROM shop.order_items i JOIN shop.products p ON p.id = i.product_id WHERE i.quantity >= 2', T.order_items.rows.filter((r) => r[3] >= 2).length],
    ["select name from shop.products where name ilike '%mug%'", 1],
  ];
  for (const [q, want] of cases) { const got = n(q); if (got !== want) throw new Error(`kui self-check: "${q}" gave ${got} rows, expected ${want}`); }
  if (execSql(Q1).rows[0][1] !== 'Grace Lovelace') throw new Error('kui self-check: ORDER BY placed_at DESC should put Grace Lovelace first');
  for (const q of ['select count(*) from shop.orders', 'select * from shop.nope', 'select id from shop.orders o join shop.customers c on c.id = o.customer_id']) {
    try { execSql(q); throw new Error(`kui self-check: "${q}" should fail`); } catch (e) { if (!(e instanceof SqlErr)) throw e; }
  }
  console.log('kui self-check ok');
}

const mount = document.querySelector('[data-kui]'), feats = mount?.closest('.feats');
if (new URLSearchParams(location.search).has('kui-check')) selfCheck();
if (mount && feats) {
  const wide = matchMedia('(min-width: 861px)');
  const sync = () => {
    if (wide.matches) {
      mount.hidden = false; feats.classList.add('has-kui');
      if (!root) { build(mount); show(feats.querySelector('input[name="feat"]:checked')?.value || 'navigator', false); }
    } else { mount.hidden = true; feats.classList.remove('has-kui'); }
  };
  wide.addEventListener('change', sync); sync();
  feats.addEventListener('change', (e) => { if (e.target.name === 'feat' && root && e.target.checked) show(e.target.value, e.isTrusted); });
}
