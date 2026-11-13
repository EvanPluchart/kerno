// Pure helpers for the live window: no DOM, so tools/check.mjs can run them in Node.

export const SAMPLES = [
  {
    id: 'latest',
    label: 'Latest orders',
    hint: 'a join, newest first',
    sql: `select o.id, c.full_name, c.email, o.status, o.total, o.currency, o.placed_at
from shop.orders o
join shop.customers c on c.id = o.customer_id
order by o.placed_at desc
limit 50`,
  },
  {
    id: 'revenue',
    label: 'Revenue by country',
    hint: 'an aggregate',
    sql: `select c.country,
       count(*) as orders,
       sum(o.total) as revenue,
       round(avg(o.total), 2) as average
from shop.orders o
join shop.customers c on c.id = o.customer_id
where o.status in ('paid', 'shipped', 'delivered')
group by c.country
order by revenue desc`,
  },
  {
    id: 'types',
    label: 'Arrays, JSON and NULL',
    hint: 'how values are drawn',
    sql: `select id, name, price, stock, tags, specs
from shop.products
order by id`,
  },
  {
    id: 'explain',
    label: 'Explain a join',
    hint: 'the planner at work',
    sql: `explain analyze
select p.category, sum(i.quantity * i.unit_price) as sold
from shop.order_items i
join shop.products p on p.id = i.product_id
group by p.category`,
  },
  {
    id: 'update',
    label: 'Raise some prices',
    hint: 'a write: try it on Production',
    sql: `update shop.products
set price = round(price * 1.10, 2)
where category = 'lighting'
returning id, name, price`,
  },
  {
    id: 'delete',
    label: 'Delete without WHERE',
    hint: 'Kerno always asks first',
    sql: `delete from shop.order_items`,
  },
];

// ponytail: statements are split on ";" without parsing strings or dollar quotes.
// Good enough to decide whether to ask; the real app uses sqlparser.
function statements(sql) {
  return sql
    .replace(/--[^\n]*/g, '')
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .split(';')
    .map((s) => s.trim())
    .filter(Boolean);
}

/** What a script would do, so the window knows which confirmation to show. */
export function classify(sql) {
  const list = statements(sql);
  const writes = (s) =>
    /^(insert|update|delete|drop|truncate|alter|create|grant|revoke|merge|comment|reindex|vacuum)\b/i.test(s) ||
    /^with\b[\s\S]*\b(insert|update|delete)\b/i.test(s);
  const destroys = (s) =>
    /^(drop|truncate)\b/i.test(s) || (/^(delete|update)\b/i.test(s) && !/\bwhere\b/i.test(s));
  return {
    count: list.length,
    write: list.some(writes),
    destructive: list.some(destroys),
  };
}

const KEYWORDS = new Set(
  `select from where join inner left right full outer cross on using group by order having limit offset
   insert into values update set delete returning create table view schema type index drop alter add column
   truncate as and or not null is in exists between like ilike case when then else end distinct union all
   with explain analyze asc desc primary key references default check unique begin commit rollback
   true false cascade enum interval`.split(/\s+/),
);

const escape = (s) => s.replace(/[&<>]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;' })[c]);

/** SQL to HTML with the app's token classes. Unknown text passes through escaped. */
export function highlight(sql) {
  const re = /(--[^\n]*|\/\*[\s\S]*?\*\/)|('(?:[^']|'')*'?)|\b(\d+(?:\.\d+)?)\b|([a-z_][a-z0-9_]*)(?=(\s*\()?)|([\s\S])/gi;
  return sql.replace(re, (all, comment, string, number, word, call) => {
    if (comment) return `<span class="t-comment">${escape(comment)}</span>`;
    if (string) return `<span class="t-string">${escape(string)}</span>`;
    if (number) return `<span class="t-number">${number}</span>`;
    if (word) {
      if (KEYWORDS.has(word.toLowerCase())) return `<span class="t-keyword">${word}</span>`;
      if (call) return `<span class="t-function">${word}</span>`;
      return word;
    }
    return escape(all);
  });
}

// PostgreSQL type OIDs for the built-in types the demo can return.
export const TYPE_NAMES = {
  16: 'bool', 18: 'char', 19: 'name', 20: 'int8', 21: 'int2', 23: 'int4', 25: 'text', 26: 'oid',
  114: 'json', 700: 'float4', 701: 'float8', 1000: 'bool[]', 1007: 'int4[]', 1009: 'text[]',
  1042: 'bpchar', 1043: 'varchar', 1082: 'date', 1083: 'time', 1114: 'timestamp', 1184: 'timestamptz',
  1186: 'interval', 1700: 'numeric', 2950: 'uuid', 3802: 'jsonb',
};
export const NUMERIC_OIDS = new Set([20, 21, 23, 26, 700, 701, 1700]);

// Keep the server's own text for these types instead of JavaScript objects,
// so the grid shows what PostgreSQL sent (as Kerno does).
const raw = (value) => value;
export const PARSERS = Object.fromEntries(
  [20, 114, 1000, 1007, 1009, 1082, 1083, 1114, 1184, 1186, 1700, 3802].map((oid) => [oid, raw]),
);

/** One cell as text; null stays null so the grid can draw NULL its own way. */
export function cellText(value) {
  if (value === null || value === undefined) return null;
  if (typeof value === 'object') return JSON.stringify(value);
  return String(value);
}

/** Turns PGlite results into the plain shape the grid and fallback.json share. */
export function toResult(results, ms) {
  const withRows = [...results].reverse().find((r) => r.fields && r.fields.length);
  const last = withRows || results[results.length - 1] || { fields: [], rows: [] };
  return {
    columns: last.fields.map((f) => ({ name: f.name, oid: f.dataTypeID })),
    rows: last.rows.map((row) => row.map(cellText)),
    affected: results.reduce((n, r) => n + (r.affectedRows || 0), 0),
    statements: results.length,
    ms,
  };
}
