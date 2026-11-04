// The 404 art: the 1-bit Gabriel played back like a worn tape. Rows slip sideways in bands,
// a few go to static, and every so often the tracking gives out for a moment.

const canvas = document.getElementById('nf-art');
const ctx = canvas.getContext('2d', { willReadFrequently: true });
const W = canvas.width, H = canvas.height;
const reduced = matchMedia('(prefers-reduced-motion: reduce)').matches;

const INK = [13, 13, 36], SILVER = [212, 212, 242], BLUE = [61, 61, 255];

const img = new Image();
img.src = '/images/gabriel.png';
await img.decode();

// The art, cropped to the canvas's 4:3, as one bit per pixel.
ctx.drawImage(img, 0, 0, W, H);
const src = ctx.getImageData(0, 0, W, H).data;
const lit = new Uint8Array(W * H);
for (let i = 0; i < lit.length; i++) lit[i] = src[i * 4] + src[i * 4 + 1] + src[i * 4 + 2] > 382 ? 1 : 0;

const out = ctx.createImageData(W, H);
const px = out.data;
const shift = new Int16Array(H);
const noisy = new Uint8Array(H);

function plan(t, burst) {
  // A slow wobble everywhere, and a few bands that slip further.
  for (let y = 0; y < H; y++) shift[y] = Math.round(Math.sin(y * 0.09 + t * 0.002) * 1.5);
  const bands = burst ? 7 : 2;
  for (let b = 0; b < bands; b++) {
    const y0 = Math.floor(Math.random() * H), h = 2 + Math.floor(Math.random() * (burst ? 28 : 10));
    const dx = Math.round((Math.random() - 0.5) * (burst ? 90 : 18));
    for (let y = y0; y < Math.min(H, y0 + h); y++) shift[y] += dx;
  }
  noisy.fill(0);
  const lines = burst ? 24 : 3;
  for (let i = 0; i < lines; i++) noisy[Math.floor(Math.random() * H)] = 1;
  // The tracking bar: a soft band that rolls down the picture.
  const bar = Math.floor((t * 0.04) % (H + 40)) - 20;
  for (let y = Math.max(0, bar); y < Math.min(H, bar + 6); y++) noisy[y] = 2;
}

function draw(t, burst) {
  plan(t, burst);
  for (let y = 0; y < H; y++) {
    const row = y * W, s = shift[y], n = noisy[y];
    for (let x = 0; x < W; x++) {
      let on;
      if (n === 1) on = Math.random() < 0.5;
      else {
        const sx = x - s;
        on = sx >= 0 && sx < W ? lit[row + sx] === 1 : false;
        if (n === 2 && Math.random() < 0.35) on = !on;
      }
      const c = on ? (n || (burst && y % 3 === 0) ? BLUE : SILVER) : INK;
      const o = (row + x) * 4;
      px[o] = c[0]; px[o + 1] = c[1]; px[o + 2] = c[2]; px[o + 3] = 255;
    }
  }
  ctx.putImageData(out, 0, 0);
}

if (reduced) {
  draw(0, false);
} else {
  let last = 0, burstUntil = 0, nextBurst = 1200;
  const frame = (t) => {
    if (t - last > 90) { // about eleven frames a second, like a tape deck, not a game
      if (t > nextBurst) { burstUntil = t + 180 + Math.random() * 260; nextBurst = t + 2200 + Math.random() * 3200; }
      draw(t, t < burstUntil);
      last = t;
    }
    requestAnimationFrame(frame);
  };
  requestAnimationFrame(frame);
}
