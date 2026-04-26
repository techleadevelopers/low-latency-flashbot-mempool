// radar.js — Latency radar (canvas2d): neon rings + sweep + central numeric readout.

const canvas = document.getElementById("radar-canvas");
const ctx = canvas.getContext("2d");

let stages = [];
let pulse = 0;
let pingRings = [];
let nextPingAt = 0;

function setStages(list) {
  stages = list || [];
}
window.__CRS_RADAR__ = { setStages };

function avgLatency() {
  if (!stages.length) return 0;
  let sum = 0, n = 0;
  for (const s of stages) {
    if (typeof s.last_ms === "number") { sum += s.last_ms; n++; }
  }
  return n ? Math.round(sum / n) : 0;
}

function draw(t) {
  pulse += 0.025;
  const W = canvas.__W || canvas.clientWidth || 280;
  const H = canvas.__H || canvas.clientHeight || 280;
  const cx = W / 2, cy = H / 2, R = Math.min(W, H) / 2 - 8;
  ctx.clearRect(0, 0, W, H);

  // background rings
  for (let i = 1; i <= 4; i++) {
    ctx.beginPath();
    ctx.strokeStyle = `rgba(0, 245, 255, ${0.06 + i * 0.03})`;
    ctx.lineWidth = 1;
    ctx.arc(cx, cy, (R / 4) * i, 0, Math.PI * 2);
    ctx.stroke();
  }

  // crosshair axes (subtle)
  ctx.strokeStyle = "rgba(0,245,255,0.10)";
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(cx - R, cy); ctx.lineTo(cx + R, cy);
  ctx.moveTo(cx, cy - R); ctx.lineTo(cx, cy + R);
  ctx.stroke();

  // sweep (conic gradient)
  const sweepAngle = pulse % (Math.PI * 2);
  const grad = ctx.createConicGradient
    ? ctx.createConicGradient(sweepAngle, cx, cy)
    : null;
  if (grad) {
    grad.addColorStop(0, "rgba(0,245,255,0.0)");
    grad.addColorStop(0.04, "rgba(0,245,255,0.55)");
    grad.addColorStop(0.35, "rgba(0,245,255,0.0)");
    grad.addColorStop(1, "rgba(0,245,255,0.0)");
    ctx.fillStyle = grad;
    ctx.beginPath();
    ctx.moveTo(cx, cy);
    ctx.arc(cx, cy, R, 0, Math.PI * 2);
    ctx.fill();
  }

  // expanding ping rings
  const now = t || performance.now();
  if (now > nextPingAt) {
    pingRings.push({ born: now });
    nextPingAt = now + 1100;
  }
  pingRings = pingRings.filter(p => now - p.born < 1800);
  for (const p of pingRings) {
    const age = (now - p.born) / 1800;
    const pr = R * age;
    ctx.beginPath();
    ctx.strokeStyle = `rgba(0,245,255,${0.45 * (1 - age)})`;
    ctx.lineWidth = 1.2;
    ctx.arc(cx, cy, pr, 0, Math.PI * 2);
    ctx.stroke();
  }

  // central readout
  const ms = avgLatency();
  const breath = 0.85 + 0.15 * Math.sin(pulse * 2);

  // glow disc
  const g = ctx.createRadialGradient(cx, cy, 0, cx, cy, R * 0.55);
  g.addColorStop(0, `rgba(0,245,255,${0.22 * breath})`);
  g.addColorStop(1, "rgba(0,245,255,0)");
  ctx.fillStyle = g;
  ctx.beginPath();
  ctx.arc(cx, cy, R * 0.55, 0, Math.PI * 2);
  ctx.fill();

  // label "AVG LATENCY"
  ctx.font = "10px 'JetBrains Mono', monospace";
  ctx.fillStyle = "rgba(140,200,220,0.75)";
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  ctx.fillText("AVG LATENCY", cx, cy - R * 0.32);

  // big number
  const sizeBig = Math.max(28, Math.min(64, R * 0.55));
  ctx.font = `900 ${sizeBig}px 'Orbitron', sans-serif`;
  ctx.fillStyle = "#d6f3ff";
  ctx.shadowColor = "rgba(0,245,255,0.85)";
  ctx.shadowBlur = 18 * breath;
  ctx.fillText(String(ms), cx, cy + 2);

  // unit
  ctx.shadowBlur = 6;
  ctx.font = "12px 'JetBrains Mono', monospace";
  ctx.fillStyle = "rgba(0,245,255,0.9)";
  ctx.fillText("ms", cx, cy + sizeBig * 0.55);

  ctx.shadowBlur = 0;

  // tick marks around perimeter
  ctx.strokeStyle = "rgba(0,245,255,0.35)";
  ctx.lineWidth = 1;
  for (let i = 0; i < 24; i++) {
    const a = (Math.PI * 2 * i) / 24;
    const inner = R - (i % 6 === 0 ? 8 : 4);
    const x1 = cx + Math.cos(a) * inner;
    const y1 = cy + Math.sin(a) * inner;
    const x2 = cx + Math.cos(a) * R;
    const y2 = cy + Math.sin(a) * R;
    ctx.beginPath();
    ctx.moveTo(x1, y1); ctx.lineTo(x2, y2); ctx.stroke();
  }
}

function loop(t) { draw(t); requestAnimationFrame(loop); }
requestAnimationFrame(loop);

// HiDPI sizing
function resizeRadar() {
  const dpr = Math.min(window.devicePixelRatio || 1, 2);
  const cssW = canvas.clientWidth || 220;
  const cssH = cssW;
  canvas.width = cssW * dpr;
  canvas.height = cssH * dpr;
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  canvas.__W = cssW; canvas.__H = cssH;
}
window.addEventListener("resize", resizeRadar);
setTimeout(resizeRadar, 10);
