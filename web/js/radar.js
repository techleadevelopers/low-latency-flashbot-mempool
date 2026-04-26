// radar.js — Latency radar (canvas2d) with neon polygon and pulse rings.

const canvas = document.getElementById("radar-canvas");
const ctx = canvas.getContext("2d");

let stages = [];
let pulse = 0;

function setStages(list) { stages = list; }
window.__CRS_RADAR__ = { setStages };

function draw() {
  pulse += 0.025;
  const W = canvas.width, H = canvas.height;
  const cx = W / 2, cy = H / 2, R = Math.min(W, H) / 2 - 18;
  ctx.clearRect(0, 0, W, H);

  // background rings
  for (let i = 1; i <= 4; i++) {
    ctx.beginPath();
    ctx.strokeStyle = `rgba(0, 245, 255, ${0.05 + i * 0.025})`;
    ctx.lineWidth = 1;
    ctx.arc(cx, cy, (R / 4) * i, 0, Math.PI * 2);
    ctx.stroke();
  }

  // sweep
  const sweepAngle = pulse % (Math.PI * 2);
  const grad = ctx.createConicGradient
    ? ctx.createConicGradient(sweepAngle, cx, cy)
    : null;
  if (grad) {
    grad.addColorStop(0, "rgba(0,245,255,0.0)");
    grad.addColorStop(0.05, "rgba(0,245,255,0.55)");
    grad.addColorStop(0.4, "rgba(0,245,255,0.0)");
    grad.addColorStop(1, "rgba(0,245,255,0.0)");
    ctx.fillStyle = grad;
    ctx.beginPath();
    ctx.moveTo(cx, cy);
    ctx.arc(cx, cy, R, 0, Math.PI * 2);
    ctx.fill();
  }

  if (!stages.length) return;

  // axes
  const n = stages.length;
  ctx.strokeStyle = "rgba(0,245,255,0.15)";
  ctx.fillStyle = "rgba(214,243,255,0.5)";
  ctx.font = "9px 'JetBrains Mono', monospace";
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  for (let i = 0; i < n; i++) {
    const a = (Math.PI * 2 * i) / n - Math.PI / 2;
    const ex = cx + Math.cos(a) * R;
    const ey = cy + Math.sin(a) * R;
    ctx.beginPath();
    ctx.moveTo(cx, cy); ctx.lineTo(ex, ey); ctx.stroke();

    // label
    const lx = cx + Math.cos(a) * (R + 12);
    const ly = cy + Math.sin(a) * (R + 12);
    ctx.fillText(stages[i].stage.replace(/_/g, " "), lx, ly);
  }

  // normalize
  const maxVal = Math.max(50, ...stages.map(s => s.last_ms || 0));

  // last polygon (cyan, filled)
  drawPolygon(cx, cy, R, stages.map(s => (s.last_ms || 0) / maxVal),
    "rgba(0,245,255,0.6)", "rgba(0,245,255,0.18)");

  // avg polygon (magenta, outlined)
  drawPolygon(cx, cy, R, stages.map(s => (s.avg_ms || 0) / maxVal),
    "rgba(255,43,214,0.7)", null, [3, 3]);
}

function drawPolygon(cx, cy, R, vals, stroke, fill, dash) {
  const n = vals.length;
  ctx.beginPath();
  for (let i = 0; i < n; i++) {
    const a = (Math.PI * 2 * i) / n - Math.PI / 2;
    const r = R * Math.min(1, vals[i]);
    const px = cx + Math.cos(a) * r;
    const py = cy + Math.sin(a) * r;
    if (i === 0) ctx.moveTo(px, py); else ctx.lineTo(px, py);
  }
  ctx.closePath();
  if (fill) { ctx.fillStyle = fill; ctx.fill(); }
  if (dash) ctx.setLineDash(dash); else ctx.setLineDash([]);
  ctx.lineWidth = 1.5;
  ctx.strokeStyle = stroke;
  ctx.shadowColor = stroke;
  ctx.shadowBlur = 8;
  ctx.stroke();
  ctx.shadowBlur = 0;
  ctx.setLineDash([]);
}

function loop() { draw(); requestAnimationFrame(loop); }
loop();

// HiDPI sizing
function resizeRadar() {
  const dpr = Math.min(window.devicePixelRatio || 1, 2);
  const cssW = canvas.clientWidth || 280;
  const cssH = cssW;
  canvas.width = cssW * dpr;
  canvas.height = cssH * dpr;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  // remap drawing coords to css size
  canvas.__W = cssW; canvas.__H = cssH;
}
window.addEventListener("resize", resizeRadar);
setTimeout(resizeRadar, 10);
