// fx.js — PixiJS WebGL background: hex grid, scan beam, particles, neon arcs.

const canvas = document.getElementById("fx-canvas");

const app = new PIXI.Application({
  view: canvas,
  resizeTo: window,
  backgroundAlpha: 0,
  antialias: true,
  resolution: Math.min(window.devicePixelRatio || 1, 2),
  autoDensity: true,
});

// === Layers ===
const grid = new PIXI.Graphics();
const arcs = new PIXI.Graphics();
const beam = new PIXI.Graphics();
const particles = new PIXI.Container();
app.stage.addChild(grid, arcs, beam, particles);

// === Hex grid ===
function drawGrid() {
  grid.clear();
  const w = app.renderer.width;
  const h = app.renderer.height;
  const step = 56;
  const r = step / 2;
  const sq3 = Math.sqrt(3);

  grid.lineStyle({ width: 1, color: 0x00f5ff, alpha: 0.08 });

  for (let y = -r; y < h + r; y += r * sq3) {
    for (let x = -r; x < w + r; x += step * 1.5) {
      const offset = (Math.round(y / (r * sq3)) % 2 === 0) ? 0 : step * 0.75;
      hex(grid, x + offset, y, r);
    }
  }

  // outer frame
  grid.lineStyle({ width: 1, color: 0x00f5ff, alpha: 0.18 });
  grid.drawRect(8, 8, w - 16, h - 16);
  grid.lineStyle({ width: 1, color: 0xff2bd6, alpha: 0.10 });
  grid.drawRect(16, 16, w - 32, h - 32);
}

function hex(g, cx, cy, r) {
  g.moveTo(cx + r, cy);
  for (let i = 1; i <= 6; i++) {
    const a = (Math.PI / 3) * i;
    g.lineTo(cx + r * Math.cos(a), cy + r * Math.sin(a));
  }
}

// === Particles ===
const PARTICLE_COUNT = 80;
const dots = [];
for (let i = 0; i < PARTICLE_COUNT; i++) {
  const g = new PIXI.Graphics();
  const isMag = Math.random() < 0.25;
  const c = isMag ? 0xff2bd6 : 0x00f5ff;
  g.beginFill(c, 1).drawCircle(0, 0, 1.4).endFill();
  g.tint = c;
  g.alpha = 0;
  particles.addChild(g);
  dots.push({
    g,
    x: Math.random() * window.innerWidth,
    y: Math.random() * window.innerHeight,
    vx: (Math.random() - 0.5) * 0.4,
    vy: (Math.random() - 0.5) * 0.4 - 0.15,
    life: Math.random() * 200,
    color: c,
  });
}

// === Scan beam ===
let beamY = 0;
function drawBeam() {
  beam.clear();
  const w = app.renderer.width;
  const grad = beam;
  // soft scan line
  for (let i = -40; i < 40; i++) {
    const a = Math.max(0, 0.04 - Math.abs(i) * 0.001);
    grad.lineStyle({ width: 1, color: 0x00f5ff, alpha: a });
    grad.moveTo(0, beamY + i);
    grad.lineTo(w, beamY + i);
  }
  // bright core
  beam.lineStyle({ width: 1, color: 0xa6f7ff, alpha: 0.25 });
  beam.moveTo(0, beamY);
  beam.lineTo(w, beamY);
}

// === Arcs (neon connections) ===
const arcsList = [];
function spawnArc() {
  const w = app.renderer.width, h = app.renderer.height;
  const sx = Math.random() * w, sy = Math.random() * h;
  const ex = sx + (Math.random() - 0.5) * 280;
  const ey = sy + (Math.random() - 0.5) * 220;
  arcsList.push({
    sx, sy, ex, ey,
    t: 0,
    life: 1.0,
    color: Math.random() < 0.5 ? 0x00f5ff : 0xff2bd6,
  });
}
setInterval(spawnArc, 700);

function drawArcs(dt) {
  arcs.clear();
  for (const a of arcsList) {
    a.t += dt * 0.02;
    a.life -= dt * 0.012;
    if (a.life <= 0) continue;
    const prog = Math.min(1, a.t);
    const cx = (a.sx + a.ex) / 2 + Math.sin(a.t * 4) * 30;
    const cy = (a.sy + a.ey) / 2 - 40;
    const ex = a.sx + (a.ex - a.sx) * prog;
    const ey = a.sy + (a.ey - a.sy) * prog;
    arcs.lineStyle({ width: 1, color: a.color, alpha: a.life * 0.5 });
    arcs.moveTo(a.sx, a.sy);
    arcs.quadraticCurveTo(cx, cy, ex, ey);
  }
  // prune dead arcs
  for (let i = arcsList.length - 1; i >= 0; i--) {
    if (arcsList[i].life <= 0) arcsList.splice(i, 1);
  }
}

// === Ticker ===
let lastFrame = performance.now(), frames = 0, fps = 60;
let beamSpeed = 1.6;

app.ticker.add((dt) => {
  // beam
  beamY += beamSpeed * dt;
  if (beamY > app.renderer.height + 40) beamY = -40;
  drawBeam();

  // particles
  for (const p of dots) {
    p.x += p.vx * dt;
    p.y += p.vy * dt;
    p.life += dt;
    if (p.x < 0) p.x = window.innerWidth;
    if (p.x > window.innerWidth) p.x = 0;
    if (p.y < -10) { p.y = window.innerHeight + 10; }
    if (p.y > window.innerHeight + 10) { p.y = -10; }
    p.g.x = p.x;
    p.g.y = p.y;
    p.g.alpha = 0.25 + Math.sin(p.life * 0.05) * 0.25;
  }

  drawArcs(dt);

  frames++;
  const now = performance.now();
  if (now - lastFrame > 1000) {
    fps = Math.round(frames * 1000 / (now - lastFrame));
    frames = 0;
    lastFrame = now;
    const el = document.getElementById("foot-fps");
    if (el) el.textContent = fps;
  }
});

window.addEventListener("resize", () => {
  drawGrid();
});
drawGrid();
