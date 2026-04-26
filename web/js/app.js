// app.js — main UI loop. Reads from window.__CRS_DATA__ and updates the DOM.

const $ = (id) => document.getElementById(id);

function fmtAddr(a) { return a.slice(0, 8) + "…" + a.slice(-6); }
function fmtTime(iso) {
  if (!iso) return "--";
  try { return new Date(iso).toISOString().slice(11, 19); } catch (_) { return "--"; }
}
function fmtDuration(ms) {
  const s = Math.floor(ms / 1000);
  const h = String(Math.floor(s / 3600)).padStart(2, "0");
  const m = String(Math.floor((s % 3600) / 60)).padStart(2, "0");
  const sec = String(s % 60).padStart(2, "0");
  return `${h}:${m}:${sec}`;
}

function renderHeader(s) {
  const mode = $("meta-mode");
  mode.textContent = (s.bot_mode || "?").toUpperCase();
  mode.className = "meta-value mode-" + (s.bot_mode || "shadow").toLowerCase();
  $("meta-network").textContent = (s.network || "?").toUpperCase();
  $("meta-chain").textContent = s.chain_id ?? "--";
  $("meta-contract").textContent = s.contract || "--";
}

function renderStats(s) {
  $("stat-wallets").textContent = s.wallet_count ?? 0;
  $("stat-keys-read").textContent = s.total_keys_read ?? 0;
  $("stat-keys-dup").textContent = s.duplicate_keys ?? 0;
  $("stat-keys-bad").textContent = s.invalid_keys ?? 0;

  $("stat-attempted").textContent = s.sweeps_attempted ?? 0;
  $("stat-interval").textContent = s.scan_interval_ms ?? 0;

  $("stat-ok").textContent = s.sweeps_succeeded ?? 0;
  const att = s.sweeps_attempted || 0;
  const rate = att > 0 ? ((s.sweeps_succeeded || 0) / att * 100).toFixed(1) : "0.0";
  $("stat-rate").textContent = rate + "%";

  $("stat-fail").textContent = s.sweeps_failed ?? 0;
  $("stat-last-scan").textContent = (s.last_scan_duration_ms ?? "--") + "ms";

  const profit = typeof s.realized_profit_eth === "number"
    ? s.realized_profit_eth
    : parseFloat(s.realized_profit_eth || "0");
  $("stat-profit").textContent = profit.toFixed(6);
  $("stat-min-profit").textContent = parseFloat(s.min_net_profit_eth || "0").toFixed(6);
}

function renderWallets(s) {
  const grid = $("wallet-grid");
  const html = (s.hot_wallets || []).map(w => `
    <div class="wallet-card">
      <div class="wallet-addr"><span class="wallet-status"></span>${fmtAddr(w.address)}</div>
      <div class="wallet-row">
        <span class="wallet-balance">${parseFloat(w.balance_eth).toFixed(6)} Ξ</span>
        <span class="wallet-rpc">${shortenRpc(w.rpc)}</span>
      </div>
    </div>
  `).join("");
  grid.innerHTML = html;
}

function shortenRpc(url) {
  if (!url) return "--";
  return url.replace(/^.*?:\/\//, "").split("/")[0];
}

function renderRpcs(s) {
  const list = $("rpc-list");
  const html = (s.rpc_endpoints || []).map(r => {
    const health = Math.round((r.health || 0) * 100);
    const tagClass = (r.role === "send") ? "rpc-tag send" : "rpc-tag";
    return `
      <div class="rpc-row">
        <div class="rpc-name">
          <span class="${tagClass}">${(r.role || "read").toUpperCase()}</span>
          ${shortenRpc(r.url)}
        </div>
        <div class="rpc-bar-wrap"><div class="rpc-bar" style="width:${health}%"></div></div>
        <div class="rpc-meta">
          ${r.latency_ms || 0}ms · err ${r.errors || 0}<br>
          blk ${(r.last_block || 0).toLocaleString()}
        </div>
      </div>
    `;
  }).join("");
  list.innerHTML = html;
}

function renderResidual(s) {
  const tbody = $("residual-body");
  const rows = s.top_residual_wallets || [];
  const maxScore = Math.max(1, ...rows.map(r => r.residual_score || 0));
  tbody.innerHTML = rows.map((r, i) => `
    <tr>
      <td>${i + 1}</td>
      <td class="mono">${fmtAddr(r.wallet)}</td>
      <td><span class="class-${r.asset_class}">${r.asset_class}</span></td>
      <td class="num">${r.detections}</td>
      <td class="num">${r.successful_sweeps}</td>
      <td class="num">${r.detected_profit_eth}</td>
      <td class="num">${r.realized_profit_eth}</td>
      <td>
        <span class="score-bar" style="width:${Math.max(6, (r.residual_score / maxScore) * 80)}px"></span>
        <span class="num">${r.residual_score}</span>
      </td>
    </tr>
  `).join("");
}

function renderLatency(s) {
  const list = $("latency-list");
  list.innerHTML = (s.latency_metrics || []).map(m => `
    <div class="latency-row">
      <span class="latency-stage">${m.stage}</span>
      <span class="latency-num last">${m.last_ms ?? "—"}</span>
      <span class="latency-num avg">${m.avg_ms ?? "—"}</span>
      <span class="latency-num max">${m.max_ms ?? "—"}</span>
    </div>
  `).join("");

  if (window.__CRS_RADAR__) window.__CRS_RADAR__.setStages(s.latency_metrics || []);
}

let lastEventCount = 0;
function renderConsole(s) {
  const c = $("event-console");
  const events = s.recent_events || [];
  if (events.length === lastEventCount) return;
  lastEventCount = events.length;
  c.innerHTML = events.slice(0, 60).map(e => `
    <div class="console-line">
      <span class="console-time">${fmtTime(e.at)}</span>
      <span class="console-level ${e.level}">${e.level.toUpperCase()}</span>
      <span class="console-msg">${escapeHtml(e.message)}</span>
    </div>
  `).join("");
}

function escapeHtml(s) {
  return String(s)
    .replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

const startTime = Date.now();
let tick = 0;

function frame() {
  const ds = window.__CRS_DATA__;
  if (!ds) { requestAnimationFrame(frame); return; }
  ds.step();
  const snap = ds.snapshot();

  renderHeader(snap);
  renderStats(snap);
  renderWallets(snap);
  renderRpcs(snap);
  renderResidual(snap);
  renderLatency(snap);
  renderConsole(snap);

  $("meta-uptime").textContent = fmtDuration(Date.now() - startTime);
  $("meta-uplink").innerHTML = ds.useLive
    ? '<span class="dot"></span> LIVE'
    : '<span class="dot" style="background:#ffb454;box-shadow:0 0 8px #ffb454"></span> SIM';
  $("foot-tick").textContent = ++tick;
}

setInterval(frame, 1000);
frame();
