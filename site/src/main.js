// Annuncia, direction D: the art opens the page, then goes into the notch and the page starts recording.

const $ = (s, r = document) => r.querySelector(s);
const $$ = (s, r = document) => [...r.querySelectorAll(s)];
const clamp = (v, a = 0, b = 1) => Math.min(b, Math.max(a, v));
const lerp = (a, b, t) => a + (b - a) * t;
const smooth = (a, b, v) => { const t = clamp((v - a) / (b - a)); return t * t * (3 - 2 * t); };
const mixRGB = (a, b, t) => [lerp(a[0], b[0], t), lerp(a[1], b[1], t), lerp(a[2], b[2], t)];
const reduced = matchMedia('(prefers-reduced-motion: reduce)').matches;
const fmt = (ms) => { const s = Math.floor(ms / 1000); return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`; };

let pending = null; // .reveal elements not yet shown

const INK = [13, 13, 36];
const BLACK = [0, 0, 0];
const SILVER = [212, 212, 242];
const BLUE = [61, 61, 255];

/* ------------------------------------------------------------------ */
/* Source image: the art is already 1-bit, so blur it back to tone.    */
/* ------------------------------------------------------------------ */

const gabriel = new Image();
gabriel.decoding = 'async';
gabriel.src = '/images/gabriel.png';
const gabrielReady = gabriel.decode().catch(() => new Promise((r) => { gabriel.onload = r; }));

const SRC_W = 640, SRC_H = 480;
let source = null; // canvas: R = tone, G = blueness

function boxBlur(a, w, h, r) {
  const t = new Float32Array(a.length), n = r * 2 + 1;
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      let s = 0;
      for (let k = -r; k <= r; k++) s += a[y * w + clamp(x + k, 0, w - 1)];
      t[y * w + x] = s / n;
    }
  }
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      let s = 0;
      for (let k = -r; k <= r; k++) s += t[clamp(y + k, 0, h - 1) * w + x];
      a[y * w + x] = s / n;
    }
  }
}

function buildSource() {
  const c = document.createElement('canvas');
  c.width = SRC_W; c.height = SRC_H;
  const x = c.getContext('2d', { willReadFrequently: true });
  x.imageSmoothingQuality = 'high';
  x.drawImage(gabriel, 0, 0, SRC_W, SRC_H);
  const d = x.getImageData(0, 0, SRC_W, SRC_H), p = d.data, n = SRC_W * SRC_H;
  const tone = new Float32Array(n), blue = new Float32Array(n);
  for (let i = 0; i < n; i++) {
    const r = p[i * 4], g = p[i * 4 + 1], b = p[i * 4 + 2];
    tone[i] = clamp(((r * .3 + g * .59 + b * .11) / 255 - .06) / .76);
    blue[i] = clamp((b - r - 45) / 120);
  }
  boxBlur(tone, SRC_W, SRC_H, 1); boxBlur(tone, SRC_W, SRC_H, 1);
  boxBlur(blue, SRC_W, SRC_H, 1);
  for (let i = 0; i < n; i++) {
    p[i * 4] = tone[i] * 255; p[i * 4 + 1] = blue[i] * 255; p[i * 4 + 2] = 0; p[i * 4 + 3] = 255;
  }
  x.putImageData(d, 0, 0);
  source = c;
}

// Atkinson error diffusion, in place. Leaves 0/1 in g.
function atkinson(g, w, h) {
  for (let y = 0; y < h; y++) {
    const row = y * w;
    for (let x = 0; x < w; x++) {
      const i = row + x, old = g[i], v = old > .5 ? 1 : 0, e = (old - v) / 8;
      g[i] = v;
      if (x + 1 < w) g[i + 1] += e;
      if (x + 2 < w) g[i + 2] += e;
      if (y + 1 < h) {
        if (x > 0) g[i + w - 1] += e;
        g[i + w] += e;
        if (x + 1 < w) g[i + w + 1] += e;
        if (y + 2 < h) g[i + w + w] += e;
      }
    }
  }
}

/* ------------------------------------------------------------------ */
/* DitherField: Gabriel cover-fit into a canvas, re-dithered live.     */
/* ------------------------------------------------------------------ */

class DitherField {
  constructor(canvas, opts) {
    this.c = canvas;
    this.x = canvas.getContext('2d');
    this.o = opts;
    this.grids = new Map();
    this.lens = null; // {x, y} in CSS px
    this.base = document.createElement('canvas');
    this.bx = this.base.getContext('2d');
    this.fine = null;
    this.p = -1;
    this.cell = 0;
  }
  resize() {
    const w = this.c.clientWidth, h = this.c.clientHeight;
    if (!w || !h) return false;
    if (w === this.w && h === this.h) return false;
    this.w = w; this.h = h;
    this.dpr = Math.min(devicePixelRatio || 1, 2);
    this.c.width = Math.round(w * this.dpr);
    this.c.height = Math.round(h * this.dpr);
    this.grids.clear();
    const f = this.o.focal;
    this.s = Math.max(w / gabriel.naturalWidth, h / gabriel.naturalHeight);
    this.iw = gabriel.naturalWidth * this.s; this.ih = gabriel.naturalHeight * this.s;
    this.ox = clamp(w / 2 - f[0] * this.iw, w - this.iw, 0);
    this.oy = clamp(h * f[2] - f[1] * this.ih, h - this.ih, 0);
    this.fine = null;
    this.p = -1;
    return true;
  }
  // CSS-px position of a point in the original image
  at(ix, iy) { return [this.ox + ix * this.s, this.oy + iy * this.s]; }
  // Tone grid at a cell size. z = {k, ax, ay} scales the art about an anchor (CSS px); only k = 1 is cached.
  grid(cell, z) {
    const zoomed = z && z.k < .999;
    let g = this.grids.get(cell);
    if (g && !zoomed && !g.zoomed) return g;
    const gw = Math.ceil(this.w / cell), gh = Math.ceil(this.h / cell);
    if (!g) {
      const n = gw * gh, c = document.createElement('canvas');
      c.width = gw; c.height = gh;
      g = { gw, gh, cell, c, cx: c.getContext('2d', { willReadFrequently: true }), tone: new Float32Array(n), blue: new Float32Array(n), work: new Float32Array(n), img: new ImageData(gw, gh) };
      this.grids.set(cell, g);
    }
    const x = g.cx, k = zoomed ? z.k : 1, ax = zoomed ? z.ax : 0, ay = zoomed ? z.ay : 0;
    x.fillStyle = '#000'; x.fillRect(0, 0, gw, gh);
    x.imageSmoothingQuality = 'high';
    x.drawImage(source, (ax + (this.ox - ax) * k) / cell, (ay + (this.oy - ay) * k) / cell, this.iw * k / cell, this.ih * k / cell);
    const p = x.getImageData(0, 0, gw, gh).data, n = gw * gh;
    for (let i = 0; i < n; i++) { g.tone[i] = p[i * 4] / 255; g.blue[i] = p[i * 4 + 1] / 255; }
    g.zoomed = zoomed;
    return g;
  }
  // Dither one grid at progress p into an offscreen canvas.
  paint(g, p, target, tctx) {
    const { gw, gh, tone, blue, work, img, cell } = g;
    const o = this.o, mask = o.mask ? o.mask(p, this) : null;
    const contrast = o.contrast || 1;
    for (let y = 0; y < gh; y++) {
      for (let x = 0; x < gw; x++) {
        const i = y * gw + x;
        let v = (tone[i] - .5) * contrast + .5;
        if (mask) v *= mask((x + .5) * cell, (y + .5) * cell);
        work[i] = v;
      }
    }
    atkinson(work, gw, gh);
    const ink = o.ink(p), lit = (g.cell === o.fine && o.fineLit) || o.lit(p), bl = o.blue ? o.blue(p) : null, d = img.data;
    for (let i = 0, n = gw * gh; i < n; i++) {
      let col = work[i] ? lit : ink;
      if (!work[i] && bl && cell <= 3 && blue[i] > .1 && ((i * 2654435761) >>> 0) % 7 < 7 * Math.min(1, blue[i] * 2.4)) col = bl;
      d[i * 4] = col[0]; d[i * 4 + 1] = col[1]; d[i * 4 + 2] = col[2]; d[i * 4 + 3] = 255;
    }
    target.width = gw; target.height = gh;
    tctx.putImageData(img, 0, 0);
  }
  render(p) {
    if (!source || !this.w) return;
    const cell = this.o.cellAt(p);
    if (p !== this.p || cell !== this.cell) {
      this.p = p; this.cell = cell;
      this.paint(this.grid(cell, this.o.zoom ? this.o.zoom(p, this) : null), p, this.base, this.bx);
    }
    const x = this.x, k = this.dpr;
    x.imageSmoothingEnabled = false;
    x.drawImage(this.base, 0, 0, this.base.width * cell * k, this.base.height * cell * k);
    const lensR = this.o.lensR ? this.o.lensR(p, this) : 0;
    if (this.lens && lensR > 2) {
      if (!this.fine) {
        const fc = document.createElement('canvas');
        this.paint(this.grid(this.o.fine), this.o.fineP ?? 0, fc, fc.getContext('2d'));
        this.fine = fc;
      }
      const f = this.o.fine;
      const { x: lx, y: ly } = this.lens;
      x.save();
      x.beginPath(); x.arc(lx * k, ly * k, lensR * k, 0, Math.PI * 2); x.clip();
      x.drawImage(this.fine, 0, 0, this.fine.width * f * k, this.fine.height * f * k);
      x.restore();
      x.beginPath(); x.arc(lx * k, ly * k, lensR * k, 0, Math.PI * 2);
      x.lineWidth = k; x.strokeStyle = 'rgba(212,212,242,.32)'; x.stroke();
    }
  }
}

/* ------------------------------------------------------------------ */
/* Overture + the page's own recording                                */
/* ------------------------------------------------------------------ */

const overture = $('.overture');
const ovStage = $('#ovStage');
const statement = $('#ovStatement');
const wordmark = $('#wordmark');
const notch = $('#notch');
const ntime = $('#ntime');
const anTime = $('#anTime');
const stopSec = $('#stop');
const drop = $('#drop');
const dropTime = $('#dropTime');
const dropDur = $('#dropDur');

const small = () => innerWidth < 640;

const ovField = new DitherField($('#ovCanvas'), {
  focal: [.52, .3, .32],
  fine: 2,
  cellAt: (p) => {
    const e = smooth(.04, .6, p);
    return (small() ? 4 : 5) + Math.round(e * e * 5);
  },
  // The art shrinks into the notch point at the top center of the viewport.
  zoom: (p, f) => {
    const e = smooth(.04, .6, p);
    return { k: 1 - .94 * Math.pow(e, 1.25), ax: f.w / 2, ay: 0 };
  },
  contrast: 1.08,
  ink: (p) => mixRGB(INK, BLACK, smooth(.05, .45, p)),
  lit: () => SILVER,
  blue: (p) => mixRGB(BLUE, BLACK, smooth(.02, .3, p)),
  // The lit area shrinks toward the top-center of the viewport: the art goes into the notch.
  mask: (p, f) => {
    const e = smooth(.04, .6, p);
    const R = 1.25 * (1 - e * .8), cx = f.w / 2, md = Math.hypot(f.w / 2, f.h), soft = .35;
    const fade = 1 - smooth(.42, .6, p);
    const notchM = e > 0 ? (x, y) => clamp((R - Math.hypot(x - cx, y) / md) / soft) * fade * (1 + e * .35) : null;
    // Thin the dither out under the lede and definition so they stay readable; the rest of the art is untouched.
    const tv = clamp(1 - p * 4.5);
    const holes = tv > 0 ? copyHoles() : [];
    const PAD = 18, FEATHER = 44, FLOOR = .16;
    const hole = (x, y) => {
      let k = 1;
      for (const [l, t, r, b] of holes) {
        const dx = Math.max(l - PAD - x, 0, x - r - PAD), dy = Math.max(t - PAD - y, 0, y - b - PAD);
        k = Math.min(k, FLOOR + (1 - FLOOR) * smooth(0, FEATHER, Math.hypot(dx, dy)));
      }
      return lerp(1, k, tv);
    };
    if (!notchM && !holes.length) return null;
    return (x, y) => (notchM ? notchM(x, y) : 1) * (holes.length ? hole(x, y) : 1);
  },
  lensR: (p, f) => (1 - smooth(0, .12, p)) * clamp(f.w * .16, 96, 210),
});

const finField = new DitherField($('#finCanvas'), {
  focal: [.5, .32, .45],
  fine: 2,
  fineP: 1,
  cellAt: (p) => (small() ? 4 : 5) + Math.round((1 - p) * (1 - p) * 14),
  contrast: 1.05,
  ink: () => BLACK,
  lit: () => [64, 64, 88],
  fineLit: SILVER,
  blue: () => [30, 30, 120],
  // Comes back out of the notch: grows from the top center.
  mask: (p, f) => {
    const R = 1.25 * p, cx = f.w / 2, md = Math.hypot(f.w / 2, f.h);
    if (p >= 1) return null;
    return (x, y) => clamp((R - Math.hypot(x - cx, y) / md) / .3);
  },
  lensR: (p, f) => smooth(.6, 1, p) * clamp(f.w * .14, 90, 190),
});

let ovP = 0, finP = 0;

// Rectangles of the overture copy, in stage CSS px.
function copyHoles() {
  const s = ovStage.getBoundingClientRect();
  return $$('.ov-lede, .ov-def').map((el) => {
    const r = el.getBoundingClientRect();
    return [r.left - s.left, r.top - s.top, r.right - s.left, r.bottom - s.top];
  });
}
let staticMode = false;

function fitWordmark() {
  const avail = ovStage.clientWidth - parseFloat(getComputedStyle(ovStage.querySelector('.ov-copy')).paddingLeft) * 2;
  wordmark.style.fontSize = '100px';
  wordmark.style.display = 'inline-block';
  const w = wordmark.getBoundingClientRect().width;
  wordmark.style.display = '';
  // The negative left margin and tracking leave a small optical overhang; compensate.
  wordmark.style.fontSize = `${(100 * avail / w) * 1.01}px`;
}

function setFaceLens(field) {
  const [x, y] = field.at(650, 300);
  field.lens = { x, y };
  field.lensIdle = true;
}

/* Recording state (the page's own take) */
const rec = { on: false, paused: false, t0: 0, acc: 0 };
const elapsed = () => rec.acc + (rec.on && !rec.paused ? performance.now() - rec.t0 : 0);
function recOn() { if (rec.on) return; rec.on = true; rec.t0 = performance.now(); }
function recOff(reset) {
  if (rec.on) { rec.acc = elapsed(); rec.on = false; }
  if (reset) { rec.acc = 0; rec.paused = false; notch.classList.remove('paused'); $('.n-pause').setAttribute('aria-pressed', 'false'); }
}
function tick() {
  const t = fmt(elapsed());
  if (ntime.textContent !== t) { ntime.textContent = t; dropTime.textContent = t; dropDur.textContent = t; }
  if (anTime) {
    const a = fmt(performance.now() - pageT0 + 42000);
    if (anTime.textContent !== a) anTime.textContent = a;
  }
}
const pageT0 = performance.now();
setInterval(tick, 250);

$('.n-pause').addEventListener('click', (e) => {
  if (!rec.on) return;
  if (rec.paused) { rec.t0 = performance.now(); rec.paused = false; }
  else { rec.acc = elapsed(); rec.paused = true; }
  notch.classList.toggle('paused', rec.paused);
  e.currentTarget.setAttribute('aria-pressed', String(rec.paused));
  e.currentTarget.setAttribute('aria-label', rec.paused ? 'Resume the demo recording' : 'Pause the demo recording');
  tick();
});
$('.n-restart').addEventListener('click', () => { rec.acc = 0; rec.t0 = performance.now(); tick(); });
$('.n-discard').addEventListener('click', () => {
  notch.style.setProperty('--nw', 0);
  rec.acc = 0; rec.t0 = performance.now(); discardUntil = performance.now() + 900;
  tick();
  setTimeout(update, 950);
});
let discardUntil = 0;

/* Stop section geometry */
let notchW = 398, camW = 190;
function measureNotch() {
  const cs = getComputedStyle(notch);
  camW = parseFloat(cs.getPropertyValue('--cam')) || 190;
  notchW = camW + 2 * (parseFloat(cs.getPropertyValue('--wing')) || 104);
}

function update() {
  const vh = innerHeight;
  // Overture progress
  const r = overture.getBoundingClientRect();
  const pin = overture.offsetHeight - ovStage.offsetHeight;
  staticMode = pin < 40;
  const p = staticMode ? 0 : clamp(-r.top / pin);
  ovP = p;
  ovStage.style.setProperty('--p', p.toFixed(4));
  statement.style.setProperty('--sp', smooth(.6, .86, p).toFixed(3));

  let nd = smooth(.34, .48, p), nw = smooth(.5, .66, p);
  if (staticMode && r.bottom < 0) { nd = 1; nw = 1; }
  if (performance.now() < discardUntil) nw = 0;

  // Stop section
  const sr = stopSec.getBoundingClientRect();
  const spin = stopSec.offsetHeight - stopSec.firstElementChild.offsetHeight;
  const stopStatic = spin <= 40;
  const q = stopStatic ? 1 : clamp(-sr.top / spin);
  const pinned = sr.top <= 0 && sr.bottom > 0;
  const past = sr.bottom <= 0;

  if (nw >= .5 && !past && (stopStatic || q <= .02)) recOn();
  else if (nw < .5) recOff(true);
  else recOff(false);

  const handOff = (pinned && !stopStatic) || past;
  notch.style.setProperty('--nd', nd.toFixed(4));
  notch.style.setProperty('--nw', nw.toFixed(4));
  notch.classList.toggle('live', nd > 0 && !past);
  notch.classList.toggle('wings-open', nw > .98);
  notch.classList.toggle('hand-off', handOff);
  document.body.classList.toggle('rec-on', nd > .2 && !past);
  document.body.classList.toggle('in-body', p > .5 || r.bottom < 0);

  // The notch comes down
  drop.classList.toggle('on', pinned || stopStatic);
  if (drop.classList.contains('on')) {
    const stopT = smooth(.01, .1, q), open = smooth(.12, .44, q);
    const panelW = Math.min(680, innerWidth - 24);
    const panel = drop.querySelector('.drop-panel');
    const panelH = panel.offsetHeight;
    const w = open > 0 ? lerp(camW, panelW, open) : lerp(notchW, camW, stopT);
    drop.style.width = `${w}px`;
    drop.style.height = `${lerp(34, panelH, open)}px`;
    drop.style.borderRadius = `0 0 ${lerp(16, 26, open)}px ${lerp(16, 26, open)}px`;
    drop.classList.toggle('stopped', q > .01);
    drop.classList.toggle('open', open > .5);
    drop.querySelector('.drop-strip').style.opacity = (1 - stopT).toFixed(3);
    panel.style.opacity = smooth(.34, .5, q).toFixed(3);
  }

  reveal();

  // Finale
  const fr = $('.finale').getBoundingClientRect();
  finP = clamp((vh - fr.top) / (vh * .9));
  scheduleDraw();
}

let drawQueued = false;
function scheduleDraw() {
  if (drawQueued) return;
  drawQueued = true;
  requestAnimationFrame(() => {
    drawQueued = false;
    if (!source) return;
    const ovIn = overture.getBoundingClientRect().bottom > 0;
    if (ovIn) ovField.render(ovP);
    const fr = $('.finale').getBoundingClientRect();
    if (fr.top < innerHeight && fr.bottom > 0) finField.render(finP);
  });
}

function onResize() {
  fitWordmark();
  measureNotch();
  if (source) {
    if (ovField.resize()) setFaceLens(ovField);
    finField.resize();
  }
  update();
}

let scrollQueued = false;
addEventListener('scroll', () => {
  if (scrollQueued) return;
  scrollQueued = true;
  requestAnimationFrame(() => { scrollQueued = false; update(); });
}, { passive: true });
let rT;
addEventListener('resize', () => { clearTimeout(rT); rT = setTimeout(onResize, 120); });

function bindLens(el, field) {
  el.addEventListener('pointermove', (e) => {
    if (e.pointerType === 'touch') return;
    const b = el.getBoundingClientRect();
    field.lens = { x: e.clientX - b.left, y: e.clientY - b.top };
    field.lensIdle = false;
    scheduleDraw();
  });
  el.addEventListener('pointerleave', () => { if (field === ovField) setFaceLens(field); else field.lens = null; scheduleDraw(); });
}
bindLens(ovStage, ovField);
bindLens($('.finale'), finField);

fitWordmark();
measureNotch();
update();
document.fonts?.ready.then(() => { fitWordmark(); measureNotch(); ovField.p = -1; ovField.fine = null; update(); });
gabrielReady.then(() => {
  buildSource();
  ovField.resize(); setFaceLens(ovField);
  finField.resize();
  update();
  bubble.ready();
});

/* ------------------------------------------------------------------ */
/* Reveal on scroll                                                    */
/* ------------------------------------------------------------------ */

pending = $$('.reveal');
pending.forEach((el) => {
  if (el.getBoundingClientRect().top < innerHeight) { el.style.transition = 'none'; el.classList.add('in'); }
});
function reveal() {
  if (!pending || !pending.length) return;
  const lim = innerHeight * .92;
  pending = pending.filter((el) => {
    if (el.classList.contains('in')) return false;
    if (el.getBoundingClientRect().top < lim) { el.classList.add('in'); return false; }
    return true;
  });
}

// Run a callback while an element is on screen.
function whileVisible(el, onEnter, onLeave) {
  new IntersectionObserver(([en]) => (en.isIntersecting ? onEnter() : onLeave && onLeave()), { rootMargin: '80px' }).observe(el);
}

/* ------------------------------------------------------------------ */
/* The take: a desktop being recorded, with Gabriel in the bubble      */
/* ------------------------------------------------------------------ */

const desk = $('#desk');
const cursor = $('#cursor');
const bars = $$('.dw-chart i');

(() => {
  if (reduced) { cursor.style.transform = 'translate(0, 0)'; cursor.style.left = '62%'; cursor.style.top = '58%'; return; }
  const pts = [[48, 30, 0], [70, 36, 1], [76, 56, 1], [58, 66, 1], [36, 46, 1], [33, 25, 0]];
  let i = 0, t0 = 0, raf = 0, from = pts[0];
  const heights = () => bars.map(() => 30 + Math.random() * 60);
  function frame(now) {
    if (!t0) t0 = now;
    const to = pts[(i + 1) % pts.length];
    const t = clamp((now - t0) / 1300);
    const e = t < .5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2;
    const x = lerp(from[0], to[0], e), y = lerp(from[1], to[1], e) + Math.sin(e * Math.PI) * -4;
    cursor.style.left = `${x}%`; cursor.style.top = `${y}%`;
    if (t >= 1) {
      if (to[2]) {
        cursor.classList.remove('clicked'); void cursor.offsetWidth; cursor.classList.add('clicked');
        const hs = heights(), hi = Math.floor(Math.random() * bars.length);
        bars.forEach((b, k) => { b.style.height = `${hs[k]}%`; b.classList.toggle('hi', k === hi); });
      }
      from = to; i = (i + 1) % pts.length; t0 = now + 500;
    }
    raf = requestAnimationFrame(frame);
  }
  whileVisible(desk, () => { if (!raf) raf = requestAnimationFrame(frame); }, () => { cancelAnimationFrame(raf); raf = 0; t0 = 0; });
})();

const bubble = (() => {
  const el = $('#bubble');
  const cv = $('#bubbleCanvas');
  const cx = cv.getContext('2d');
  const icon = new Image(); icon.src = '/images/icon.png';
  // Region of the art that the camera sees, in image px.
  const RX0 = 430, RY0 = 110, RX1 = 900, RY1 = 660;
  // Head outline for pop out (image px), traced around hair, face and chin.
  const HEAD = [[566, 196], [594, 166], [636, 150], [684, 150], [720, 168], [752, 192], [774, 226], [780, 252], [778, 280], [764, 322], [744, 360], [722, 396], [692, 420], [654, 424], [622, 404], [604, 372], [590, 336], [572, 300], [560, 262], [558, 226]];
  const NORMAL = { x: 655, y: 300, r: 178 };
  const POP = { x: 662, y: 468, r: 168 };
  let bmp = null, bmpFor = 0, pop = 1, popTarget = 1, camOn = true, anim = 0;

  function dither(diam) {
    const cells = Math.max(60, Math.round(diam / 2.2));
    const cellImg = (NORMAL.r * 2 + POP.r * 2) / 2 / cells;
    const gw = Math.ceil((RX1 - RX0) / cellImg), gh = Math.ceil((RY1 - RY0) / cellImg);
    const c = document.createElement('canvas'); c.width = gw; c.height = gh;
    const x = c.getContext('2d', { willReadFrequently: true });
    x.imageSmoothingQuality = 'high';
    const sx = SRC_W / gabriel.naturalWidth;
    x.drawImage(source, RX0 * sx, RY0 * sx, (RX1 - RX0) * sx, (RY1 - RY0) * sx, 0, 0, gw, gh);
    const d = x.getImageData(0, 0, gw, gh), p = d.data, n = gw * gh, g = new Float32Array(n);
    for (let i = 0; i < n; i++) g[i] = Math.pow(clamp((p[i * 4] / 255 - .5) * 1.1 + .5), .72);
    atkinson(g, gw, gh);
    for (let i = 0; i < n; i++) {
      const v = g[i] ? [234, 234, 246] : [6, 6, 14];
      p[i * 4] = v[0]; p[i * 4 + 1] = v[1]; p[i * 4 + 2] = v[2]; p[i * 4 + 3] = 255;
    }
    x.putImageData(d, 0, 0);
    bmp = { c, cellImg };
    bmpFor = diam;
  }

  function draw() {
    if (!source) return;
    const D = el.offsetWidth;
    if (!D) return;
    const dpr = Math.min(devicePixelRatio || 1, 2);
    const W = D * 1.2, H = D * 1.5;
    if (cv.width !== Math.round(W * dpr)) { cv.width = Math.round(W * dpr); cv.height = Math.round(H * dpr); }
    if (!bmp || Math.abs(bmpFor - D) > D * .25) dither(D);
    const r = D / 2, ccx = W / 2, ccy = H - D * .6;
    cx.setTransform(dpr, 0, 0, dpr, 0, 0);
    cx.clearRect(0, 0, W, H);
    const t = pop * pop * (3 - 2 * pop);
    const F = { x: lerp(NORMAL.x, POP.x, t), y: lerp(NORMAL.y, POP.y, t), r: lerp(NORMAL.r, POP.r, t) };
    const k = r / F.r;
    const place = () => {
      cx.translate(ccx, ccy); cx.scale(-k, k); cx.translate(-F.x, -F.y); // mirrored, like a mirror
    };
    // Circle
    cx.save();
    cx.beginPath(); cx.arc(ccx, ccy, r, 0, Math.PI * 2); cx.clip();
    cx.fillStyle = '#000'; cx.fillRect(0, 0, W, H);
    if (camOn) {
      cx.imageSmoothingEnabled = false;
      place();
      cx.drawImage(bmp.c, RX0, RY0, RX1 - RX0, RY1 - RY0);
    } else if (icon.complete) {
      cx.imageSmoothingEnabled = true;
      const s = D * 1.5;
      cx.drawImage(icon, ccx - s / 2, ccy - s * .46, s, s);
    }
    cx.restore();
    // Ring
    cx.beginPath(); cx.arc(ccx, ccy, r - 1, 0, Math.PI * 2);
    cx.lineWidth = Math.max(2, D * .016); cx.strokeStyle = '#fff'; cx.stroke();
    // Pop out: the head rises above the circle
    if (camOn && t > .01) {
      cx.save();
      cx.globalAlpha = t;
      place();
      cx.beginPath();
      HEAD.forEach(([hx, hy], i) => (i ? cx.lineTo(hx, hy) : cx.moveTo(hx, hy)));
      cx.closePath(); cx.clip();
      cx.imageSmoothingEnabled = false;
      cx.drawImage(bmp.c, RX0, RY0, RX1 - RX0, RY1 - RY0);
      cx.restore();
    }
  }

  function animate() {
    cancelAnimationFrame(anim);
    if (reduced) { pop = popTarget; draw(); return; }
    const from = pop, t0 = performance.now();
    const step = (now) => {
      const u = clamp((now - t0) / 650);
      pop = lerp(from, popTarget, 1 - Math.pow(1 - u, 3));
      draw();
      if (u < 1) anim = requestAnimationFrame(step);
    };
    anim = requestAnimationFrame(step);
  }

  // Size
  $$('.seg [data-size]').forEach((b) => b.addEventListener('click', () => {
    $$('.seg [data-size]').forEach((o) => o.setAttribute('aria-checked', String(o === b)));
    el.dataset.size = b.dataset.size;
    const t0 = performance.now();
    const step = (now) => { keepInside(); draw(); if (now - t0 < 600) requestAnimationFrame(step); else { bmp = null; draw(); } };
    requestAnimationFrame(step);
  }));
  const togPop = $('#togPop'), togCam = $('#togCam');
  togPop.addEventListener('click', () => {
    popTarget = popTarget ? 0 : 1;
    togPop.setAttribute('aria-pressed', String(!!popTarget));
    animate();
  });
  togCam.addEventListener('click', () => {
    camOn = !camOn;
    togCam.setAttribute('aria-pressed', String(camOn));
    el.setAttribute('aria-label', camOn ? 'Camera bubble with Gabriel presenting. Drag to move it.' : 'Camera bubble showing the avatar while the camera is off. Drag to move it.');
    draw();
  });

  // Drag anywhere inside the desktop
  let drag = null;
  function keepInside() {
    const dw = desk.clientWidth, dh = desk.clientHeight, D = el.offsetWidth;
    const left = clamp(el.offsetLeft, 6, dw - D - 6), top = clamp(el.offsetTop, D * .6, dh - D - 6);
    el.style.left = `${(left / dw) * 100}%`; el.style.top = `${(top / dh) * 100}%`; el.style.bottom = 'auto';
  }
  el.addEventListener('pointerdown', (e) => {
    el.setPointerCapture(e.pointerId);
    drag = { x: e.clientX, y: e.clientY, l: el.offsetLeft, t: el.offsetTop };
    el.classList.add('dragging');
  });
  el.addEventListener('pointermove', (e) => {
    if (!drag) return;
    const dw = desk.clientWidth, dh = desk.clientHeight, D = el.offsetWidth;
    const l = clamp(drag.l + e.clientX - drag.x, 6, dw - D - 6);
    const t = clamp(drag.t + e.clientY - drag.y, D * .6, dh - D - 6);
    el.style.left = `${(l / dw) * 100}%`; el.style.top = `${(t / dh) * 100}%`; el.style.bottom = 'auto';
  });
  const end = () => { drag = null; el.classList.remove('dragging'); };
  el.addEventListener('pointerup', end);
  el.addEventListener('pointercancel', end);
  el.addEventListener('keydown', (e) => {
    const m = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] }[e.key];
    if (!m) return;
    e.preventDefault();
    el.style.left = `${el.offsetLeft + m[0] * 16}px`; el.style.top = `${el.offsetTop + m[1] * 16}px`; el.style.bottom = 'auto';
    keepInside();
  });

  new ResizeObserver(() => draw()).observe(el);
  icon.onload = () => draw();
  return { ready: () => { bmp = null; draw(); } };
})();

/* Menu bar popover toggles */
$$('.popover .po-row').forEach((b) => b.addEventListener('click', () => b.setAttribute('aria-pressed', String(b.getAttribute('aria-pressed') !== 'true'))));
$$('.po-seg button').forEach((b) => b.addEventListener('click', () => $$('.po-seg button').forEach((o) => o.setAttribute('aria-checked', String(o === b)))));
$('.po-go').addEventListener('click', (e) => {
  // Back to the top: the take starts when the art goes into the notch.
  if (reduced) return;
  e.preventDefault();
  scrollTo({ top: overture.offsetTop + (overture.offsetHeight - ovStage.offsetHeight) * .8, behavior: 'smooth' });
});

/* Drop-down actions */
const dpSay = $('#dpSay');
$$('.dpa[data-say]').forEach((b) => b.addEventListener('click', () => {
  dpSay.textContent = b.dataset.say;
  clearTimeout(b._t);
  b._t = setTimeout(() => { dpSay.textContent = ''; }, 2600);
}));

/* ------------------------------------------------------------------ */
/* Editor: clips, filler words, clean up                               */
/* ------------------------------------------------------------------ */

(() => {
  const SEGS = [
    { k: 'air', d: 1.6, label: 'Dead air' },
    { k: 'speech', d: 6.8 },
    { k: 'filler', d: .7, label: 'um' },
    { k: 'speech', d: 8.4, sel: true },
    { k: 'pause', d: 3.4, label: 'Long pause', keep: .6 },
    { k: 'speech', d: 6.2 },
    { k: 'filler', d: .6, label: 'uh' },
    { k: 'speech', d: 7.6 },
    { k: 'filler', d: .8, label: 'um' },
    { k: 'speech', d: 5.4 },
    { k: 'air', d: 2.4, label: 'Dead air' },
  ];
  const track = $('#edTrack'), ruler = $('#edRuler'), tray = $('#trayChips');
  const durEl = $('#edDur'), pvTime = $('#pvTime'), btn = $('#cleanBtn');
  let seed = 7;
  const rnd = () => ((seed = (seed * 16807) % 2147483647) / 2147483647);
  const wave = (s) => {
    const n = Math.max(6, Math.round(s.d * 9));
    let out = '';
    for (let i = 0; i < n; i++) {
      let a;
      if (s.k === 'speech') a = .18 + Math.pow(rnd(), .8) * .82 * (.55 + .45 * Math.sin(i / n * Math.PI));
      else if (s.k === 'filler') a = .22 + rnd() * .22;
      else a = .04 + rnd() * .03;
      const h = a * 100;
      out += `<rect x="${(i / n) * 100 + 100 / n * .22}" y="${50 - h / 2}" width="${100 / n * .56}" height="${h}" rx=".6"/>`;
    }
    return `<svg viewBox="0 0 100 100" preserveAspectRatio="none" fill="currentColor" aria-hidden="true">${out}</svg>`;
  };
  SEGS.forEach((s, i) => {
    s.state = 'full';
    const el = document.createElement('div');
    el.className = `seg-el ${s.k}${s.sel ? ' sel' : ''}`;
    el.style.setProperty('--g', s.d);
    el.innerHTML = wave(s) + (s.label && s.k === 'filler' ? `<span class="seg-tag">${s.label}</span>` : '');
    if (s.k === 'speech') el.setAttribute('aria-label', `Clip ${Math.ceil(i / 2)}`);
    s.el = el;
    track.appendChild(el);
  });
  const ph = document.createElement('div'); ph.className = 'playhead'; track.appendChild(ph);

  const effective = (s) => (s.state === 'cut' ? 0 : s.state === 'short' ? s.keep : s.d);
  const total = () => SEGS.reduce((a, s) => a + effective(s), 0);
  const fmtS = (v) => `${Math.floor(v / 60)}:${(v % 60).toFixed(1).padStart(4, '0')}`;
  function layout() {
    SEGS.forEach((s) => {
      s.el.style.setProperty('--g', Math.max(effective(s), 0.0001));
      s.el.classList.toggle('cut', s.state === 'cut');
      s.el.classList.toggle('short', s.state === 'short');
    });
    const T = total();
    durEl.textContent = fmtS(T);
    let html = '';
    for (let t = 0; t <= T; t += 10) html += `<span style="left:${(t / T) * 100}%">${Math.floor(t / 60)}:${String(Math.round(t % 60)).padStart(2, '0')}</span>`;
    ruler.innerHTML = html;
    const cuts = SEGS.filter((s) => s.state !== 'full');
    tray.innerHTML = cuts.length ? '' : '<span class="tray-empty">Nothing yet. Press Clean up.</span>';
    cuts.forEach((s) => {
      const b = document.createElement('button');
      b.type = 'button'; b.className = 'chip';
      const what = s.k === 'pause' ? `${s.label} <small>${s.d.toFixed(1)}s to ${s.keep.toFixed(1)}s</small>` : `${s.label} <small>${s.d.toFixed(1)}s</small>`;
      b.innerHTML = `<span class="grip" aria-hidden="true"></span>${what}`;
      b.setAttribute('aria-label', `Put back: ${s.label}`);
      b.title = 'Drag back into the timeline';
      b.addEventListener('click', () => { s.state = 'full'; layout(); syncBtn(); });
      tray.appendChild(b);
    });
  }
  function syncBtn() {
    const any = SEGS.some((s) => s.state !== 'full');
    btn.setAttribute('aria-pressed', String(any));
    btn.textContent = any ? 'Undo clean up' : 'Clean up';
  }
  btn.addEventListener('click', () => {
    const any = SEGS.some((s) => s.state !== 'full');
    SEGS.forEach((s) => { if (s.k !== 'speech') s.state = any ? 'full' : (s.k === 'pause' ? 'short' : 'cut'); });
    layout(); syncBtn();
  });
  layout();

  // Auto clean up once, when the editor is well into view.
  let autoDone = false;
  new IntersectionObserver(([en]) => {
    if (en.isIntersecting && !autoDone) {
      autoDone = true;
      setTimeout(() => { if (!SEGS.some((s) => s.state !== 'full')) btn.click(); }, reduced ? 0 : 1100);
    }
  }, { threshold: .55 }).observe($('#editor'));

  // Playhead sweep
  let raf = 0, t0 = 0;
  const fps = 30;
  function frame(now) {
    if (!t0) t0 = now;
    const T = total(), t = ((now - t0) / 1000 * 2.2) % T;
    ph.style.left = `${(t / T) * 100}%`;
    const f = Math.floor((t % 1) * fps);
    pvTime.textContent = `${Math.floor(t / 60)}:${String(Math.floor(t % 60)).padStart(2, '0')}:${String(f).padStart(2, '0')}`;
    raf = requestAnimationFrame(frame);
  }
  if (reduced) { ph.style.left = '38%'; pvTime.textContent = '0:12:08'; }
  else whileVisible($('#editor'), () => { if (!raf) raf = requestAnimationFrame(frame); }, () => { cancelAnimationFrame(raf); raf = 0; });
})();

/* ------------------------------------------------------------------ */
/* Edit opener: a sentence being spoken, its ums struck and cut        */
/* ------------------------------------------------------------------ */

(() => {
  const TOKENS = [
    ['So,'], ['um,', 1], ["here’s"], ['the'], ['plan'], ['for'], ['the'], ['launch.'],
    ['We'], ['uh', 1], ['ship'], ['the'], ['editor'], ['first.'],
  ];
  const RATE = 1.35; // spoken-time units per second
  const HOLD = 2.6;
  const ease = (x) => 1 - Math.pow(1 - x, 3);
  const box = $('#saidWords');
  const cv = $('#saidWave');
  const ctx = cv.getContext('2d');

  const words = TOKENS.map(([text, f]) => {
    const n = document.createElement('span');
    n.className = `kw${f ? ' um' : ''}`;
    n.textContent = text;
    box.appendChild(n);
    return { text, f: !!f, el: n, on: false, struck: false, cut: false };
  });
  const segs = [{ k: 'sil', t0: 0, d: .45, w: 1, seed: 0 }];
  let T = .45;
  words.forEach((w, i) => {
    const d = w.f ? .5 : .12 + .052 * w.text.replace(/[^a-z]/gi, '').length;
    w.t0 = T; w.t1 = T + d;
    w.seg = { k: w.f ? 'fill' : 'word', t0: T, d, w: 1, seed: i * 7.31 + 1.7, word: w };
    segs.push(w.seg);
    T += d;
    const gap = /\.$/.test(w.text) ? .46 : /,$/.test(w.text) ? .22 : .06;
    segs.push({ k: 'sil', t0: T, d: gap, w: 1, seed: i * 3.1 });
    T += gap;
  });
  const TOTAL = T;

  let t = 0, phase = 'play', phaseT = 0, rewindFrom = 0;

  function setWord(w, on, struck, cut) {
    if (on !== w.on) { w.el.classList.toggle('on', on); w.on = on; }
    if (struck !== w.struck) { w.el.classList.toggle('struck', struck); w.struck = struck; }
    if (cut === w.cut) return;
    w.cut = cut;
    if (cut) {
      w.nat = w.el.getBoundingClientRect().width;
      w.el.style.width = `${w.nat}px`;
      void w.el.offsetWidth;
      w.el.classList.add('cut');
      w.el.style.width = '0px';
    } else {
      w.el.classList.remove('cut');
      w.el.style.width = `${w.nat || 0}px`;
      setTimeout(() => { if (!w.cut) w.el.style.width = ''; }, 650);
    }
  }
  // Keep the block from jumping when a line closes up.
  function reserveHeight() {
    if (words.some((w) => w.cut)) return;
    box.style.minHeight = '';
    box.style.minHeight = `${box.offsetHeight}px`;
  }

  function step(dt) {
    if (phase === 'play') {
      t += dt * RATE;
      for (const w of words) {
        setWord(w, t >= w.t0, w.f && t >= w.t1 - .04, w.f && t >= w.t1 + .5);
        if (w.f) w.seg.w = 1 - ease(clamp((t - (w.t1 + .5)) / .6));
      }
      if (t >= TOTAL + HOLD) {
        phase = 'rewind'; phaseT = 0; rewindFrom = Math.min(t, TOTAL);
        words.forEach((w) => setWord(w, false, false, false));
      }
    } else {
      phaseT += dt;
      for (const w of words) if (w.f) w.seg.w = ease(clamp(phaseT / .55));
      if (phaseT > 1) { phase = 'play'; t = 0; }
    }
  }

  let W = 0, H = 0;
  function size() {
    const r = cv.getBoundingClientRect();
    const dpr = Math.min(2, devicePixelRatio || 1);
    W = r.width; H = r.height;
    cv.width = Math.round(W * dpr); cv.height = Math.round(H * dpr);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    draw();
  }
  function amp(s, u, bx) {
    if (s.k === 'sil') return .035 + .03 * Math.abs(Math.sin(bx * .71));
    const env = Math.pow(Math.sin(Math.PI * clamp(u)), .55);
    const n = .5 + .5 * Math.abs(Math.sin(u * 9.1 + s.seed) * Math.cos(u * 4.3 + s.seed * 1.7));
    const a = .1 + .9 * env * n;
    return s.k === 'fill' ? a * .6 : a;
  }
  const FONT = '"Instrument Sans", system-ui, sans-serif';
  function draw() {
    if (!W) return;
    const rulerH = 26, top = 16, waveH = H - rulerH - top - 6, mid = top + waveH / 2, scale = W / TOTAL;
    ctx.clearRect(0, 0, W, H);
    let x = 0;
    for (const s of segs) { s.x = x; s.wpx = s.d * s.w * scale; x += s.wpx; }
    const endX = x;
    let px = 0;
    if (phase === 'rewind') {
      let fromX = 0;
      for (const s of segs) if (s.t0 <= rewindFrom) fromX = s.x + clamp((rewindFrom - s.t0) / s.d) * s.wpx;
      px = fromX * (1 - ease(clamp(phaseT / .8)));
    } else if (t >= TOTAL) px = endX;
    else for (const s of segs) if (s.t0 <= t) px = s.x + clamp((t - s.t0) / s.d) * s.wpx;

    // Bars: played in white, upcoming translucent, struck filler fainter still
    const pitch = W < 600 ? 4 : 5, bw = pitch - 2;
    let si = 0;
    for (let bx = 0; bx < endX - .5; bx += pitch) {
      const c = bx + bw / 2;
      while (si < segs.length - 1 && c > segs[si].x + segs[si].wpx) si++;
      const s = segs[si];
      const u = s.wpx > 0 ? (c - s.x) / s.wpx : 0;
      const h = Math.max(2, amp(s, u, bx) * waveH);
      ctx.fillStyle = s.k === 'fill' && s.word.struck ? 'rgba(255,255,255,.1)' : c <= px ? '#f2f2f5' : 'rgba(255,255,255,.2)';
      ctx.beginPath(); ctx.roundRect(bx, mid - h / 2, bw, h, Math.min(1.5, bw / 2)); ctx.fill();
    }
    // Struck filler: strike line and a tag
    ctx.font = `600 12.5px ${FONT}`;
    ctx.textBaseline = 'middle'; ctx.textAlign = 'center';
    for (const w of words) {
      if (!w.f || !w.struck || w.seg.w < .05) continue;
      const s = w.seg;
      ctx.globalAlpha = clamp(s.w * 1.4);
      ctx.fillStyle = '#fff';
      ctx.fillRect(s.x - 2, mid - 1, s.wpx + 4, 2);
      const label = `cut “${w.text.replace(/[^a-z]/gi, '')}”`;
      const tw = ctx.measureText(label).width + 14, lx = clamp(s.x + s.wpx / 2, tw / 2, W - tw / 2);
      ctx.beginPath(); ctx.roundRect(lx - tw / 2, top - 14, tw, 22, 5); ctx.fill();
      ctx.fillStyle = '#000'; ctx.fillText(label, lx, top - 3);
      ctx.globalAlpha = 1;
    }
    // Ruler, in seconds of recording
    const ry = H - rulerH;
    ctx.textAlign = 'left'; ctx.textBaseline = 'alphabetic';
    ctx.font = `500 12.5px ${FONT}`;
    for (let k = 0; k * .5 <= TOTAL + .01; k++) {
      const tx = k * .5 * scale, major = k % 2 === 0;
      ctx.fillStyle = major ? 'rgba(255,255,255,.5)' : 'rgba(255,255,255,.22)';
      ctx.fillRect(Math.min(tx, W - 1), ry, 1, major ? 9 : 5);
      if (major && tx < W - 200) ctx.fillText(`0:${String(k / 2).padStart(2, '0')}`, tx + 5, ry + 21);
    }
    const removed = words.filter((w) => w.f).reduce((a, w) => a + w.seg.d * (1 - w.seg.w), 0);
    if (removed > .02 && phase === 'play') {
      ctx.textAlign = 'right'; ctx.font = `600 13px ${FONT}`; ctx.fillStyle = '#f2f2f5';
      ctx.fillText(`${removed.toFixed(1)}s of filler removed`, W, ry + 21);
    }
    // End of the recording, then the playhead (the one blue thing)
    ctx.fillStyle = 'rgba(255,255,255,.4)';
    ctx.fillRect(endX - 1, top, 1, waveH);
    ctx.fillStyle = '#3d3dff';
    const hx = Math.round(clamp(px, 1, W - 1));
    ctx.fillRect(hx - 1, top - 4, 2, waveH + 8);
    ctx.beginPath(); ctx.roundRect(hx - 6, top - 10, 12, 9, 3); ctx.fill();
  }

  if (reduced) {
    // The finished sentence: spoken and cleaned up.
    t = TOTAL;
    words.forEach((w) => { w.el.classList.add('on'); if (w.f) { w.el.style.display = 'none'; w.seg.w = 0; } });
  }
  new ResizeObserver(size).observe(cv);
  document.fonts?.ready.then(() => { reserveHeight(); size(); });
  addEventListener('resize', () => requestAnimationFrame(reserveHeight));
  if (!reduced) {
    let raf = 0, last = 0;
    const frame = (now) => {
      const dt = last ? Math.min(.05, (now - last) / 1000) : 0;
      last = now;
      step(dt); draw();
      raf = requestAnimationFrame(frame);
    };
    whileVisible($('#said'), () => { last = 0; if (!raf) raf = requestAnimationFrame(frame); }, () => { cancelAnimationFrame(raf); raf = 0; });
  }
})();

/* ------------------------------------------------------------------ */
/* Transcript: lights up word by word, click a word to jump            */
/* ------------------------------------------------------------------ */

(() => {
  const PARAS = [
    "So here's the plan for the launch. We ship the editor first, then transcripts, and the library right after that.",
    "The recorder stays in the menu bar. When you stop, the notch drops down and you can paste the file straight into Slack.",
    "Nothing gets uploaded. Search works across every recording, so you can find the part where we talked about pricing.",
  ];
  const box = $('#trText'), md = $('#trMd'), fill = $('#plFill'), time = $('#plTime');
  const words = [];
  let t = 0;
  box.innerHTML = '';
  const starts = [];
  PARAS.forEach((p) => {
    const div = document.createElement('div'); div.className = 'para';
    const ts = document.createElement('button'); ts.type = 'button'; ts.className = 'ts';
    const pStart = t; starts.push(pStart);
    ts.textContent = fmt(pStart * 1000);
    ts.setAttribute('aria-label', `Jump to ${fmt(pStart * 1000)}`);
    ts.addEventListener('click', () => seek(pStart));
    const body = document.createElement('p');
    p.split(' ').forEach((w, i) => {
      const s = document.createElement('span'); s.className = 'w'; s.textContent = w;
      const start = t; t += .18 + w.length * .045 + (/[.,]$/.test(w) ? .3 : 0);
      s.addEventListener('click', () => seek(start));
      words.push({ s, start });
      body.appendChild(s);
      body.appendChild(document.createTextNode(' '));
    });
    t += .8;
    div.append(ts, body);
    box.appendChild(div);
  });
  const T = t;
  md.textContent = PARAS.map((p, i) => `**${fmt(starts[i] * 1000)}** ${p}`).join('\n\n');

  let cur = 0, last = -1, raf = 0, prev = 0;
  function paint() {
    let idx = -1;
    for (let i = 0; i < words.length; i++) if (words[i].start <= cur) idx = i;
    if (idx !== last) {
      words.forEach((w, i) => { w.s.classList.toggle('on', i < idx); w.s.classList.toggle('now', i === idx); });
      last = idx;
      const el = words[idx]?.s;
      if (el) {
        const top = el.offsetTop - box.offsetTop;
        if (top > box.scrollTop + box.clientHeight - 60 || top < box.scrollTop) box.scrollTo({ top: Math.max(0, top - 40), behavior: reduced ? 'auto' : 'smooth' });
      }
    }
    fill.style.width = `${(cur / T) * 100}%`;
    time.textContent = `${fmt(cur * 1000)} / ${fmt(T * 1000)}`;
  }
  function seek(v) { cur = v; paint(); }
  function frame(now) {
    const dt = prev ? (now - prev) / 1000 : 0; prev = now;
    cur += dt;
    if (cur > T + 2) { cur = 0; box.scrollTo({ top: 0 }); }
    paint();
    raf = requestAnimationFrame(frame);
  }
  if (reduced) seek(words[14].start);
  else whileVisible(box, () => { prev = 0; if (!raf) raf = requestAnimationFrame(frame); }, () => { cancelAnimationFrame(raf); raf = 0; });
  paint();

  // Tabs
  const tText = $('#tabText'), tMd = $('#tabMd');
  [tText, tMd].forEach((b) => b.addEventListener('click', () => {
    const isMd = b === tMd;
    tText.setAttribute('aria-selected', String(!isMd)); tMd.setAttribute('aria-selected', String(isMd));
    box.hidden = isMd; md.hidden = !isMd;
  }));

  // Engine status: a quick fill when first seen
  const teFill = $('#teFill'), teState = $('#teState');
  let done = false;
  new IntersectionObserver(([en]) => {
    if (!en.isIntersecting || done) return;
    done = true;
    if (reduced) { teFill.style.width = '100%'; teState.textContent = 'Transcribed'; return; }
    teState.textContent = 'Transcribing';
    const t0 = performance.now();
    const step = (now) => {
      const u = clamp((now - t0) / 1500);
      teFill.style.width = `${(1 - Math.pow(1 - u, 2)) * 100}%`;
      if (u < 1) requestAnimationFrame(step); else teState.textContent = 'Transcribed';
    };
    requestAnimationFrame(step);
  }, { threshold: .6 }).observe($('.tr-engine'));
})();

/* ------------------------------------------------------------------ */
/* Library + search across transcripts                                 */
/* ------------------------------------------------------------------ */

(() => {
  const REC = [
    { title: 'Launch plan', dur: '2:41', tb: 'linear-gradient(175deg,#12123a,#050508)', win: [22, 12, 66, 72], acc: [30, 78, 22], bub: [5, 60], lines: [[0, "So here's the plan for the launch."], [21, 'When you stop, the notch drops down and the file is ready to paste.'], [74, 'We talked about pricing last week, so this is the short version.']] },
    { title: 'Export dialog bug', dur: '0:43', tb: 'linear-gradient(175deg,#141418,#050506)', win: [10, 14, 80, 74], acc: [16, 30, 30], lines: [[0, 'Watch what happens when I click export.'], [12, 'The dialog opens behind the main window, so it looks frozen.'], [31, 'It only happens on the second display.']] },
    { title: 'Onboarding walkthrough', dur: '4:12', tb: 'linear-gradient(175deg,#0c1426,#040508)', win: [30, 16, 60, 68], acc: [38, 70, 40], bub: [6, 58], lines: [[0, 'This is the first screen a new teammate sees.'], [96, 'Keep the window small so it does not sit under the notch on a laptop.'], [188, 'Then invite them to the project from settings.']] },
    { title: 'Weekly update', dur: '1:58', tb: 'linear-gradient(175deg,#16122a,#050508)', win: [14, 12, 50, 76], acc: [20, 40, 18], bub: [72, 58], lines: [[0, 'Three things this week.'], [26, 'We shipped the pricing page and fixed two crashes in sync.'], [80, 'Next week we start on the mobile layout.']] },
    { title: 'Design review', dur: '3:05', tb: 'linear-gradient(175deg,#101020,#030304)', win: [18, 10, 70, 80], acc: [24, 22, 46], bub: [6, 60], lines: [[0, "Let's look at the header first."], [72, 'The spacing near the notch feels tight on the smaller laptops.'], [140, 'Otherwise this is ready to build.']] },
    { title: 'Pricing page feedback', dur: '1:12', tb: 'linear-gradient(175deg,#0e0e16,#040406)', win: [24, 14, 56, 72], acc: [30, 56, 34], lines: [[0, "I'd move the comparison table up."], [28, 'Most people scroll straight to it.'], [51, 'And the free plan should be the first column.']] },
  ];
  const grid = $('#libGrid'), input = $('#libSearch'), count = $('#libCount');
  const esc = (s) => s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
  const cards = REC.map((r, i) => {
    const a = document.createElement('article');
    a.className = 'card';
    const [wx, wy, ww, wh] = r.win;
    a.innerHTML = `
      <div class="thumb" style="--tb:${r.tb};--td:${.35 + (i % 3) * .2}" aria-hidden="true">
        <span class="t-win" style="left:${wx}%;top:${wy}%;width:${ww}%;height:${wh}%"></span>
        <span class="t-acc" style="left:${r.acc[0]}%;top:${r.acc[1]}%;width:${r.acc[2]}%"></span>
        ${r.bub ? `<span class="t-bub" style="left:${r.bub[0]}%;top:${r.bub[1]}%"></span>` : ''}
        <span class="t-dur">${r.dur}</span>
      </div>
      <h3>${esc(r.title)}</h3>
      <p class="meta">${r.dur} · Transcribed</p>
      <div class="snip"></div>
      ${i === 1 ? '<div class="menu" aria-hidden="true"><span>Rename</span><span>Transcribe again</span><hr><span>Move to Trash</span></div>' : ''}`;
    grid.appendChild(a);
    return a;
  });
  function search(q) {
    q = q.trim().toLowerCase();
    let hits = 0;
    grid.classList.toggle('q', !!q);
    cards.forEach((c, i) => {
      c.classList.toggle('searching', !!q);
      const line = q ? REC[i].lines.find(([, s]) => s.toLowerCase().includes(q)) : null;
      c.classList.toggle('hit', !!line);
      c.classList.toggle('dim', !!q && !line);
      const snip = c.querySelector('.snip');
      if (line) {
        hits++;
        const [at, s] = line, k = s.toLowerCase().indexOf(q);
        snip.innerHTML = `“${esc(s.slice(0, k))}<mark>${esc(s.slice(k, k + q.length))}</mark>${esc(s.slice(k + q.length))}”<br><span class="jump">Jump to ${fmt(at * 1000)}</span>`;
      } else snip.innerHTML = '';
    });
    count.textContent = q ? `${hits} of ${REC.length} recordings` : `${REC.length} recordings`;
  }
  input.addEventListener('input', () => { typing = false; search(input.value); });
  let typing = false;
  const DEMO = 'the notch';
  new IntersectionObserver(([en]) => {
    if (!en.isIntersecting || input.value) return;
    if (reduced) { input.value = DEMO; search(DEMO); return; }
    typing = true;
    let i = 0;
    const step = () => {
      if (!typing) return;
      input.value = DEMO.slice(0, ++i);
      search(input.value);
      if (i < DEMO.length) setTimeout(step, 90 + Math.random() * 80);
    };
    setTimeout(step, 700);
  }, { threshold: .45 }).observe($('#lib'));
})();
