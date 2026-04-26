// app.js — main UI loop + sidebar router. Reads from window.__CRS_DATA__ and renders views.

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
function shortenRpc(url) {
  if (!url) return "--";
  return url.replace(/^.*?:\/\//, "").split("/")[0];
}
function escapeHtml(s) {
  return String(s)
    .replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}
function timeAgo(ms) {
  const d = (Date.now() - ms) / 1000;
  if (d < 60) return Math.floor(d) + "s";
  if (d < 3600) return Math.floor(d / 60) + "m";
  return Math.floor(d / 3600) + "h";
}

/* ========= Router ========= */
const VIEWS = ["dashboard", "wallets", "rpc", "eip7702", "events"];
let currentView = "dashboard";

function setView(v) {
  if (!VIEWS.includes(v)) v = "dashboard";
  currentView = v;
  document.querySelectorAll(".view").forEach(el => {
    el.classList.toggle("active", el.dataset.view === v);
  });
  document.querySelectorAll(".nav-item").forEach(el => {
    el.classList.toggle("active", el.dataset.view === v);
  });
}

function initRouter() {
  document.querySelectorAll(".nav-item").forEach(a => {
    a.addEventListener("click", e => {
      e.preventDefault();
      const v = a.dataset.view;
      window.location.hash = v;
      setView(v);
    });
  });
  window.addEventListener("hashchange", () => {
    const v = (window.location.hash || "#dashboard").slice(1);
    setView(v);
  });
  const initial = (window.location.hash || "#dashboard").slice(1);
  setView(initial);
}

/* ========= Header ========= */
function renderHeader(s) {
  const mode = $("meta-mode");
  mode.textContent = (s.bot_mode || "?").toUpperCase();
  mode.className = "meta-value mode-" + (s.bot_mode || "shadow").toLowerCase();
  $("meta-network").textContent = (s.network || "?").toUpperCase();
  $("meta-chain").textContent = s.chain_id ?? "--";
  $("meta-contract").textContent = s.contract || "--";

  const summary = s.delegation_summary;
  const total = summary?.total ?? (s.hot_wallets || []).length;
  const deleg = summary?.delegated ?? (s.hot_wallets || []).filter(w => w.delegated_7702).length;
  const appr = summary?.preapproved ?? (s.hot_wallets || []).filter(w => w.preapproved).length;
  const delegEl = $("meta-deleg");
  const apprEl = $("meta-preappr");
  if (delegEl) {
    delegEl.innerHTML = `<span class="dot ${deleg === total ? "ok" : "warn"}"></span> ${deleg}/${total}`;
  }
  if (apprEl) {
    apprEl.innerHTML = `<span class="dot ${appr === total ? "ok" : "warn"}"></span> ${appr}/${total}`;
  }
}

/* ========= Dashboard view ========= */
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

function renderResidual(s) {
  const tbody = $("residual-body");
  if (!tbody) return;
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
  if (window.__CRS_RADAR__) window.__CRS_RADAR__.setStages(s.latency_metrics || []);
}

/* ========= Wallets ops view ========= */
function renderWalletsView(s) {
  const wallets = s.hot_wallets || [];
  const total = wallets.length;
  const armed = wallets.filter(w => parseFloat(w.balance_eth) > 0).length;
  const sum = wallets.reduce((a, w) => a + parseFloat(w.balance_eth || 0), 0);
  $("ops-wallet-total").textContent = total;
  $("ops-wallet-armed").textContent = armed;
  $("ops-wallet-sum").textContent = sum.toFixed(6);

  const tbody = $("ops-wallet-body");
  if (!tbody) return;
  const sorted = [...wallets].sort((a, b) => parseFloat(b.balance_eth) - parseFloat(a.balance_eth));
  tbody.innerHTML = sorted.map((w, i) => {
    const deleg = w.delegated_7702;
    const appr = w.preapproved;
    return `
      <tr>
        <td class="dim">${i + 1}</td>
        <td class="mono">${fmtAddr(w.address)}</td>
        <td class="num neon-c">${parseFloat(w.balance_eth).toFixed(6)}</td>
        <td class="dim">${shortenRpc(w.rpc)}</td>
        <td>${badge(deleg, "7702")}</td>
        <td>${badge(appr, "APR", true)}</td>
        <td class="dim">${w.last_seen_at ? fmtTime(w.last_seen_at) : "--"}</td>
      </tr>
    `;
  }).join("");
}

/* ========= RPC ops view ========= */
function renderRpcView(s) {
  const rpcs = s.rpc_endpoints || [];
  const send = rpcs.filter(r => r.role === "send").length;
  const read = rpcs.length - send;
  $("ops-rpc-total").textContent = rpcs.length;
  $("ops-rpc-send").textContent = send;
  $("ops-rpc-read").textContent = read;

  const grid = $("ops-rpc-grid");
  if (!grid) return;
  grid.innerHTML = rpcs.map(r => {
    const health = Math.round((r.health || 0) * 100);
    const tag = r.role === "send" ? "send" : "read";
    return `
      <div class="rpc-card">
        <div class="rpc-card-head">
          <span class="rpc-tag ${tag}">${(r.role || "read").toUpperCase()}</span>
          <span class="rpc-card-url mono">${shortenRpc(r.url)}</span>
          <span class="rpc-card-status"><span class="dot ${health > 70 ? "ok" : health > 40 ? "warn" : "err"}"></span></span>
        </div>
        <div class="rpc-card-bar">
          <div class="rpc-card-bar-fill" style="width:${health}%"></div>
        </div>
        <div class="rpc-card-grid">
          <div><span class="kv-k">HEALTH</span><span class="kv-v">${health}%</span></div>
          <div><span class="kv-k">LATENCY</span><span class="kv-v">${r.latency_ms || 0} ms</span></div>
          <div><span class="kv-k">ERRORS</span><span class="kv-v">${r.errors || 0}</span></div>
          <div><span class="kv-k">LAST BLOCK</span><span class="kv-v mono">${(r.last_block || 0).toLocaleString()}</span></div>
        </div>
        <div class="rpc-card-actions">
          <button class="btn-cyber sm">PING</button>
          <button class="btn-cyber sm alt">RESET</button>
        </div>
      </div>
    `;
  }).join("");
}

/* ========= EIP-7702 ops view ========= */
function renderDelegView(s) {
  const wallets = s.hot_wallets || [];
  const summary = s.delegation_summary;
  const total = summary?.total ?? wallets.length;
  const deleg = summary?.delegated ?? wallets.filter(w => w.delegated_7702).length;
  const appr = summary?.preapproved ?? wallets.filter(w => w.preapproved).length;
  $("ops-deleg-count").textContent = `${deleg}/${total}`;
  $("ops-appr-count").textContent = `${appr}/${total}`;
  $("ops-deleg-target").textContent = s.contract || "--";

  const tbody = $("ops-deleg-body");
  if (!tbody) return;
  tbody.innerHTML = wallets.map((w, i) => {
    const d = w.delegated_7702;
    const a = w.preapproved;
    const target = s.contract || "0x…";
    return `
      <tr>
        <td class="dim">${i + 1}</td>
        <td class="mono">${fmtAddr(w.address)}</td>
        <td>${statusPill(d, "INSTALLED", "MISSING")}</td>
        <td class="mono dim">${fmtAddr(target)}</td>
        <td>${statusPill(a, "OK", "PENDING", true)}</td>
        <td class="dim">${fmtTime(new Date().toISOString())}</td>
        <td>
          <button class="btn-cyber sm ${d ? "alt" : ""}">${d ? "RE-APPLY" : "DELEGATE"}</button>
        </td>
      </tr>
    `;
  }).join("");
}

/* ========= Events ========= */
let lastEventCount = 0;
function renderConsole(s) {
  const c = $("event-console");
  if (!c) return;
  const events = s.recent_events || [];
  if (events.length === lastEventCount && currentView !== "events") return;
  lastEventCount = events.length;
  c.innerHTML = events.slice(0, 120).map(e => `
    <div class="console-line">
      <span class="console-time">${fmtTime(e.at)}</span>
      <span class="console-level ${e.level}">${e.level.toUpperCase()}</span>
      <span class="console-msg">${escapeHtml(e.message)}</span>
    </div>
  `).join("");
}

/* ========= Helpers ========= */
function badge(val, label, green = false) {
  if (val === null || val === undefined) return `<span class="op-badge">--</span>`;
  if (val) {
    return `<span class="op-badge on ${green ? "g" : ""}">${label}</span>`;
  }
  return `<span class="op-badge off">${label}</span>`;
}
function statusPill(ok, onLabel, offLabel, green = false) {
  if (ok === null || ok === undefined) return `<span class="op-pill">--</span>`;
  if (ok) {
    return `<span class="op-pill ok ${green ? "g" : ""}"><span class="dot ok"></span>${onLabel}</span>`;
  }
  return `<span class="op-pill warn"><span class="dot warn"></span>${offLabel}</span>`;
}

/* ========= Frame loop ========= */
const startTime = Date.now();
let tick = 0;

function frame() {
  const ds = window.__CRS_DATA__;
  if (!ds) { requestAnimationFrame(frame); return; }
  ds.step();
  const snap = ds.snapshot();

  renderHeader(snap);

  if (currentView === "dashboard") {
    renderStats(snap);
    renderResidual(snap);
    renderLatency(snap);
  } else if (currentView === "wallets") {
    renderWalletsView(snap);
  } else if (currentView === "rpc") {
    renderRpcView(snap);
  } else if (currentView === "eip7702") {
    renderDelegView(snap);
  } else if (currentView === "events") {
    renderConsole(snap);
  }

  // always feed the radar so it stays smooth across views
  if (window.__CRS_RADAR__) window.__CRS_RADAR__.setStages(snap.latency_metrics || []);

  $("meta-uptime").textContent = fmtDuration(Date.now() - startTime);
  $("meta-uplink").innerHTML = ds.useLive
    ? '<span class="dot ok"></span> LIVE'
    : '<span class="dot warn"></span> SIM';
  $("foot-tick").textContent = ++tick;
}

initRouter();
setInterval(frame, 1000);
frame();
