// A felt demo of virtual rendering: 1,000,000 generated rows, a few dozen in the DOM.
// It shows the technique Kerno's grid uses; Kerno's own figures come from docs/BENCH.md.

const ROWS = 1_000_000;
const ROW_H = 28;
const HEAD_H = 30;
// ponytail: browsers cap an element's height (Firefox near 17.8M px), so the scroll range is
// capped and mapped onto the rows. One scrolled pixel then moves about 2.3 px of rows.
const MAX_SPACE = 12_000_000;

const FIRST = ['Grace', 'Ada', 'Frances', 'Edsger', 'Radia', 'Ken', 'Barbara', 'Dennis', 'Margaret', 'Linus'];
const LAST = ['Hopper', 'Lovelace', 'Allen', 'Dijkstra', 'Perlman', 'Thompson', 'Liskov', 'Ritchie', 'Hamilton', 'Torvalds', 'Knuth'];
const COUNTRY = ['FR', 'DE', 'US', 'GB', 'ES', 'NL', 'SE'];
const STATUS = ['pending', 'paid', 'shipped', 'delivered', 'cancelled', 'refunded'];
const CURRENCY = ['EUR', 'USD', 'GBP'];
const EPOCH = Date.UTC(2024, 0, 1);

const COLUMNS = [
  ['', 84, 'n', (i) => (i + 1).toLocaleString('en-US')],
  ['id', 96, 'num', (i) => String(i + 1)],
  ['customer', 170, '', (i) => `${FIRST[i % 10]} ${LAST[(i * 7) % 11]}`],
  ['email', 250, '', (i) => `customer${(i * 31) % 50000}@example.com`],
  ['country', 86, '', (i) => COUNTRY[(i * 3) % 7]],
  ['status', 112, '', (i) => STATUS[(i * 5) % 6]],
  ['total', 104, 'num', (i) => (((i * 7919) % 60000) / 100 + 5).toFixed(2)],
  ['currency', 92, '', (i) => CURRENCY[(i * 2) % 3]],
  ['placed_at', 216, '', (i) => `${new Date(EPOCH + i * 53_000).toISOString().slice(0, 19).replace('T', ' ')}+00`],
];

export function initMillion() {
  const scroller = document.querySelector('#million');
  if (!scroller) return;
  const head = scroller.querySelector('.m-head');
  const space = scroller.querySelector('.m-space');
  const windowEl = scroller.querySelector('.m-rows');
  const inner = document.createElement('div');
  windowEl.append(inner);

  for (const [name, width, kind] of COLUMNS) {
    const cell = document.createElement('span');
    cell.textContent = name;
    cell.style.width = `${width}px`;
    if (kind) cell.className = kind;
    head.append(cell);
  }

  let viewH = 0;
  let maxScroll = 1;
  let totalPx = 1;
  let pool = [];
  let drawnFirst = -1;

  function layout() {
    viewH = scroller.clientHeight - HEAD_H;
    const spaceH = Math.min(ROWS * ROW_H, MAX_SPACE);
    space.style.height = `${spaceH}px`;
    windowEl.style.height = `${viewH}px`;
    windowEl.style.overflow = 'hidden';
    maxScroll = spaceH - viewH;
    totalPx = ROWS * ROW_H - viewH;
    const count = Math.ceil(viewH / ROW_H) + 1;
    pool = Array.from({ length: count }, () => {
      const row = document.createElement('div');
      row.className = 'm-row';
      for (const [, width, kind] of COLUMNS) {
        const cell = document.createElement('samp');
        cell.style.width = `${width}px`;
        if (kind) cell.className = kind;
        row.append(cell);
      }
      return row;
    });
    inner.replaceChildren(...pool);
    drawnFirst = -1;
    document.querySelector('#m-dom').textContent = `${count} of 1,000,000`;
    render();
  }

  function render() {
    const position = (Math.min(scroller.scrollTop, maxScroll) / maxScroll) * totalPx;
    const first = Math.floor(position / ROW_H);
    inner.style.transform = `translateY(${first * ROW_H - position}px)`;
    if (first === drawnFirst) return;
    drawnFirst = first;
    pool.forEach((row, offset) => {
      const index = first + offset;
      row.hidden = index >= ROWS;
      if (index >= ROWS) return;
      COLUMNS.forEach(([, , , value], column) => {
        row.children[column].textContent = value(index);
      });
    });
    document.querySelector('#m-top').textContent = (first + 1).toLocaleString('en-US');
  }

  // One loop while the grid is on screen: it renders, drifts until the visitor takes over,
  // and reports the frame rate.
  const calm = matchMedia('(prefers-reduced-motion: reduce)').matches;
  let drifting = !calm;
  let visible = false;
  let last = 0;
  let frames = [];
  let shownAt = 0;
  const frameOut = document.querySelector('#m-frame');

  function frame(now) {
    if (!visible) return;
    if (last) {
      frames.push(now - last);
      if (frames.length > 90) frames.shift();
      if (now - shownAt > 500 && frames.length > 20) {
        shownAt = now;
        frameOut.textContent = `${Math.round((1000 * frames.length) / frames.reduce((a, b) => a + b, 0))} fps`;
      }
    }
    last = now;
    if (drifting) scroller.scrollTop += 1;
    render();
    requestAnimationFrame(frame);
  }

  new IntersectionObserver(([entry]) => {
    const was = visible;
    visible = entry.isIntersecting;
    if (visible && !was) {
      last = 0;
      frames = [];
      requestAnimationFrame(frame);
    }
  }).observe(scroller);

  // The grid scrolls on its own until the visitor selects it; only then does it take the wheel,
  // and it gives the page back on Escape, on blur, or when the mouse leaves.
  const hint = scroller.parentElement.querySelector('.m-hint');
  const takeOver = () => {
    drifting = false;
  };
  const live = (on) => {
    scroller.classList.toggle('is-live', on);
    hint.hidden = on;
    if (on) takeOver();
  };
  hint.addEventListener('click', () => scroller.focus({ preventScroll: true }));
  scroller.addEventListener('focus', () => live(true));
  scroller.addEventListener('blur', () => live(false));
  scroller.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') scroller.blur();
  });
  scroller.parentElement.addEventListener('pointerleave', (event) => {
    if (event.pointerType === 'mouse') scroller.blur();
  });

  document.querySelector('#m-jump').addEventListener('submit', (event) => {
    event.preventDefault();
    takeOver();
    const wanted = Math.min(Math.max(Number(document.querySelector('#m-row').value) || 1, 1), ROWS);
    scroller.scrollTop = Math.min((((wanted - 1) * ROW_H) / totalPx) * maxScroll, maxScroll);
    render();
  });

  new ResizeObserver(layout).observe(scroller);
}
