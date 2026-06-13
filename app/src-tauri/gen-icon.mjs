// Generate a 1024×1024 source app icon (no deps), then expand to the full platform set:
//   node gen-icon.mjs app-icon.png && npm run tauri icon app-icon.png
// Design: rounded "squircle" with an indigo→sky gradient and four harmony/equalizer bars.
import zlib from "node:zlib";
import fs from "node:fs";

const W = 1024, H = 1024;
const buf = Buffer.alloc(W * H * 4); // RGBA

const lerp = (a, b, t) => a + (b - a) * t;
const c0 = [79, 70, 229]; // indigo
const c1 = [56, 189, 248]; // sky

function insideRoundRect(px, py, x0, y0, x1, y1, r) {
  if (px < x0 || px > x1 || py < y0 || py > y1) return false;
  const nx = Math.min(Math.max(px, x0 + r), x1 - r);
  const ny = Math.min(Math.max(py, y0 + r), y1 - r);
  const dx = px - nx, dy = py - ny;
  return dx * dx + dy * dy <= r * r;
}

// background squircle
const M = 64, R = 224;

// infinity = lemniscate of Bernoulli, sampled to points for a distance-based stroke.
const cx = 512, cy = 512, a = 286, stroke = 74;
const pts = [];
const STEPS = 760;
for (let i = 0; i < STEPS; i++) {
  const th = (i / STEPS) * Math.PI * 2;
  const s = Math.sin(th), c = Math.cos(th);
  const den = 1 + s * s;
  pts.push([cx + (a * c) / den, cy + (a * s * c) / den]);
}
const bx0 = cx - a - stroke, bx1 = cx + a + stroke;
const by0 = cy - 150, by1 = cy + 150;

function px(x, y) {
  const o = (y * W + x) * 4;
  if (!insideRoundRect(x, y, M, M, W - M, H - M, R)) {
    buf[o + 3] = 0; // transparent corners
    return;
  }
  const t = (x + y) / (W + H);
  let r = lerp(c0[0], c1[0], t);
  let g = lerp(c0[1], c1[1], t);
  let b = lerp(c0[2], c1[2], t);
  if (x >= bx0 && x <= bx1 && y >= by0 && y <= by1) {
    let best = Infinity;
    for (let i = 0; i < pts.length; i++) {
      const dx = x - pts[i][0], dy = y - pts[i][1];
      const d2 = dx * dx + dy * dy;
      if (d2 < best) best = d2;
    }
    const cov = Math.min(Math.max(stroke / 2 - Math.sqrt(best) + 0.5, 0), 1);
    if (cov > 0) {
      const aa = cov * 0.96;
      r = lerp(r, 255, aa);
      g = lerp(g, 255, aa);
      b = lerp(b, 255, aa);
    }
  }
  buf[o] = r; buf[o + 1] = g; buf[o + 2] = b; buf[o + 3] = 255;
}

for (let y = 0; y < H; y++) for (let x = 0; x < W; x++) px(x, y);

// --- PNG encode ---
const raw = Buffer.alloc((W * 4 + 1) * H);
for (let y = 0; y < H; y++) {
  raw[y * (W * 4 + 1)] = 0;
  buf.copy(raw, y * (W * 4 + 1) + 1, y * W * 4, (y + 1) * W * 4);
}
function crc32(b) {
  let c = ~0;
  for (const x of b) { c ^= x; for (let k = 0; k < 8; k++) c = (c >>> 1) ^ (0xedb88320 & -(c & 1)); }
  return (~c) >>> 0;
}
function chunk(type, data) {
  const t = Buffer.from(type, "ascii");
  const len = Buffer.alloc(4); len.writeUInt32BE(data.length, 0);
  const crc = Buffer.alloc(4); crc.writeUInt32BE(crc32(Buffer.concat([t, data])), 0);
  return Buffer.concat([len, t, data, crc]);
}
const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(W, 0); ihdr.writeUInt32BE(H, 4); ihdr[8] = 8; ihdr[9] = 6;
const png = Buffer.concat([
  Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
  chunk("IHDR", ihdr),
  chunk("IDAT", zlib.deflateSync(raw)),
  chunk("IEND", Buffer.alloc(0)),
]);
const out = process.argv[2] || "app-icon.png";
fs.writeFileSync(out, png);
console.log(`wrote ${out} (${W}×${H}, ${png.length} bytes)`);
