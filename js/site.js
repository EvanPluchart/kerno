// Kerno landing page. Everything here is an enhancement: the page reads, the environment switch
// and the feature list work without this file (they are radio inputs styled in site.css).

const still = matchMedia('(prefers-reduced-motion: reduce)').matches;
const $ = (s, el = document) => el.querySelector(s);
const $$ = (s, el = document) => [...el.querySelectorAll(s)];

// Scroll reveals: elements that enter together are staggered.
const seen = new IntersectionObserver((entries) => {
  let i = 0;
  for (const e of entries) {
    if (!e.isIntersecting) continue;
    e.target.style.setProperty('--d', `${i++ * 70}ms`);
    e.target.classList.add('in');
    seen.unobserve(e.target);
  }
}, { rootMargin: '0px' });
$$('[data-r]').forEach((el) => seen.observe(el));

// Hero: the window rises a little as the page scrolls. Whole pixels, so the capture stays sharp.
const stage = $('.hero-stage');
if (stage && !still) {
  let queued = false;
  const rise = () => {
    queued = false;
    const p = (stage.getBoundingClientRect().top - innerHeight * 0.2) / (innerHeight * 0.45);
    stage.style.setProperty('--y', `${Math.round(Math.min(1, Math.max(0, p)) * 16)}px`);
  };
  addEventListener('scroll', () => { if (!queued) { queued = true; requestAnimationFrame(rise); } }, { passive: true });
  addEventListener('resize', rise);
  rise();
}

// docs/BENCH.md: 279 ms from launch to a usable window, median of 30 cold starts.
const START_MS = 279;

// Hero: the window appears in that time, and a line under it says so (only when it did).
const heroWindow = $('.hero-window'), heroTag = $('.hero-tag');
if (heroWindow && !still && scrollY < 40) {
  heroWindow.classList.add('opening');
  const open = () => setTimeout(() => {
    heroWindow.classList.add('opened');
    heroWindow.classList.remove('opening');
    setTimeout(() => { heroTag.hidden = false; }, START_MS + 200);
  }, 650);
  const img = $('img', heroWindow);
  if (img.complete) open(); else img.addEventListener('load', open, { once: true });
}

// Fast: a clock that runs for the same time.
const clock = $('.clock'), ms = $('[data-ms]'), replay = $('.replay');
if (clock && !still) {
  let busy = false;
  const play = () => {
    if (busy) return;
    busy = true;
    clock.dataset.state = 'idle';
    ms.textContent = '0';
    setTimeout(() => {
      clock.dataset.state = 'done';
      const t0 = performance.now();
      const tick = (now) => {
        const t = Math.min(START_MS, now - t0);
        ms.textContent = Math.round(t);
        if (t < START_MS) requestAnimationFrame(tick); else busy = false;
      };
      requestAnimationFrame(tick);
    }, 600);
  };
  // At rest the clock shows the measured figure and a full bar.
  replay.hidden = false;
  replay.addEventListener('click', play);
  const once = new IntersectionObserver(([e]) => { if (e.isIntersecting) { once.disconnect(); play(); } }, { threshold: 0.8 });
  once.observe(clock);
}

// The recording of the million-row scroll: the poster of the visitor's theme, and it plays while
// it is on screen. Without this script, or with reduced motion, it keeps its controls.
const reel = $('.reel video');
if (reel) {
  if (matchMedia('(prefers-color-scheme: light)').matches) reel.poster = 'img/video/scroll-wide-light.webp';
  if (!still) {
    reel.controls = false;
    new IntersectionObserver(([e]) => { if (e.isIntersecting) reel.play().catch(() => { reel.controls = true; }); else reel.pause(); }, { threshold: 0.5 }).observe(reel);
  }
}

// Safe on production: the switch flips once by itself when the window is in view, so the visitor
// sees the window turn red; after that it is theirs.
const safe = $('.safe');
if (safe && !still) {
  let touched = false;
  ['pointerdown', 'keydown'].forEach((ev) => safe.addEventListener(ev, () => { touched = true; }));
  const seenSafe = new IntersectionObserver(([e]) => {
    if (!e.isIntersecting) return;
    seenSafe.disconnect();
    setTimeout(() => { if (!touched) $('input[value="production"]', safe).checked = true; }, 900);
  }, { threshold: 0.6 });
  seenSafe.observe($('.safe-stage', safe));
}

// On a phone the download is for another machine: the main button shares the page instead.
if (navigator.share && matchMedia('(max-width: 720px) and (pointer: coarse)').matches) {
  $$('[data-download] .btn.primary').forEach((btn) => {
    $('span', btn).textContent = 'Send it to my computer';
    btn.addEventListener('click', (e) => {
      e.preventDefault();
      navigator.share({ title: document.title, url: location.href }).catch(() => {});
    });
  });
}

// The two commands of the download notes can be copied.
if (navigator.clipboard) {
  $$('.cmd').forEach((cmd) => {
    const text = cmd.textContent;
    const btn = Object.assign(document.createElement('button'), { type: 'button', textContent: 'Copy' });
    btn.addEventListener('click', () => navigator.clipboard.writeText(text).then(() => {
      btn.textContent = 'Copied';
      setTimeout(() => { btn.textContent = 'Copy'; }, 1600);
    }, () => {}));
    cmd.append(btn);
  });
}

// The interactive window of the Features section: only on wide screens, and only once the
// visitor comes near it. Its stylesheet is in place before the module builds the window.
const kuiMount = $('[data-kui]');
if (kuiMount) {
  const wide = matchMedia('(min-width: 861px)');
  let asked = false;
  const load = () => {
    if (asked || !wide.matches) return;
    asked = true;
    const sheet = Object.assign(document.createElement('link'), { rel: 'stylesheet', href: 'css/kui.css' });
    sheet.onload = () => import('./kui.js');
    document.head.append(sheet);
  };
  const near = new IntersectionObserver(([e]) => { if (e.isIntersecting) { near.disconnect(); load(); wide.addEventListener('change', load); } }, { rootMargin: '1200px 0px' });
  near.observe(kuiMount.parentElement);
}

// Download buttons: lead with the visitor's system.
if (/Linux|X11/.test(navigator.userAgent) && !/Android/.test(navigator.userAgent)) {
  $$('[data-download]').forEach((group) => {
    const mac = $('[data-os="mac"]', group), linux = $('[data-os="linux"]', group);
    linux.className = 'btn primary';
    mac.className = 'btn quiet';
    linux.prepend($('svg', mac));
    $('span', linux).textContent = 'Download for Linux';
    $('span', mac).textContent = 'macOS';
    group.prepend(linux);
  });
  $$('[data-mac-only]').forEach((el) => { el.hidden = true; });
}

window.kernoReady = true;
