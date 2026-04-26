// data.js — fetches /api/status from the Rust dashboard if reachable;
// otherwise generates a realistic simulation that matches the DashboardState shape.

const NETWORKS = ["arbitrum", "ethereum", "bsc"];
const ASSET_CLASSES = ["native", "stable", "other-token"];
const RPC_NAMES = [
  "alchemy://arb-mainnet/g/0",
  "infura://arb1/v3/0",
  "infura://arb1/v3/1",
  "public://arb1.arbitrum.io",
];
const LATENCY_STAGES = [
  "block_fetch", "scan_cycle", "enqueue_latency",
  "queue_wait", "tx_prepare", "bundle_attempt"
];

function rand(min, max) { return Math.random() * (max - min) + min; }
function randInt(min, max) { return Math.floor(rand(min, max + 1)); }
function pick(arr) { return arr[Math.floor(Math.random() * arr.length)]; }

function makeAddr(seed) {
  // deterministic-ish address from seed
  let s = seed.toString(16).padStart(8, "0");
  let body = "";
  for (let i = 0; i < 5; i++) body += s.split("").reverse().join("");
  return "0x" + body.slice(0, 40);
}

function shortAddr(a) { return a.slice(0, 6) + "…" + a.slice(-4); }

class DataSource {
  constructor() {
    this.startedAt = Date.now();
    this.tick = 0;
    this.useLive = false;

    // simulation seed
    this.walletCount = 24;
    this.wallets = Array.from({ length: this.walletCount }, (_, i) => ({
      address: makeAddr(0xabc123 + i * 7919),
      balance: rand(0.0008, 0.038),
      rpc: pick(RPC_NAMES),
      class: pick(ASSET_CLASSES),
      detections: randInt(0, 18),
      sweeps: randInt(0, 6),
      detected: rand(0, 0.012),
      realized: 0,
      lastSeen: Date.now() - randInt(2, 1800) * 1000,
      delegated_7702: Math.random() < 0.92,
      preapproved: Math.random() < 0.85,
    }));
    this.wallets.forEach(w => { w.realized = w.detected * rand(0.55, 0.95); });

    this.rpcs = RPC_NAMES.map((url, i) => ({
      url,
      role: i === 0 ? "send" : "read",
      health: rand(0.55, 0.99),
      latencyMs: randInt(35, 220),
      errors: randInt(0, 12),
      lastBlock: 250_000_000 + randInt(0, 5000),
    }));

    this.attempted = 312;
    this.succeeded = 274;
    this.failed = 38;
    this.realizedTotal = 0;

    this.latency = LATENCY_STAGES.map(stage => ({
      stage,
      samples: randInt(40, 420),
      last_ms: randInt(8, 180),
      avg_ms: randInt(12, 90),
      max_ms: randInt(120, 480),
    }));

    this.events = [];
    this.seedEvents();

    // contract abi (mirrors src/contract.rs Simple7702Delegate)
    this.contractAbi = {
      name: "Simple7702Delegate",
      address: "0x7f5e9ab4dc12fe0a8b21cf04d913c5a2e9b8d4a3",
      owner: "0x4a2c91b6ddf6e8e103a9b0f51c2eb4a5d7f8e210",
      destination: "0xc0ffee0bb44de8d2c11a72f5b3c3097a1c44e3a9",
      frozen: false,
      codehash: "0x" + Array.from({length:64},()=>("0123456789abcdef"[Math.floor(Math.random()*16)])).join(""),
      runtime_size: 6244,
      runtime_limit: 24576,
      deploy_block: 250_001_122,
      bytecode_preview: this.makeBytecodePreview(),
      read: [
        { name: "owner",             sig: "owner() view returns (address)",                                                ret: "address" },
        { name: "destination",       sig: "destination() view returns (address)",                                          ret: "address" },
        { name: "frozen",            sig: "frozen() view returns (bool)",                                                  ret: "bool" },
        { name: "getNativeBalance",  sig: "getNativeBalance() view returns (uint256)",                                     ret: "uint256" },
        { name: "getTokenBalance",   sig: "getTokenBalance(address token) view returns (uint256)",                         ret: "uint256" },
        { name: "getTokenAllowance", sig: "getTokenAllowance(address token, address tokenOwner) view returns (uint256)",   ret: "uint256" },
        { name: "getArbitrumTokens", sig: "getArbitrumTokens() view returns (address[])",                                  ret: "address[]" },
        { name: "getBscTokens",      sig: "getBscTokens() view returns (address[])",                                       ret: "address[]" },
      ],
      write_sweeps: [
        { name: "sweepNative",     sig: "sweepNative()" },
        { name: "sweepTokens",     sig: "sweepTokens(address[] tokens)" },
        { name: "sweepAll",        sig: "sweepAll(address[] tokens)" },
        { name: "sweepTokensFrom", sig: "sweepTokensFrom(address from, address[] tokens)" },
        { name: "sweepAllFrom",    sig: "sweepAllFrom(address from, address[] tokens)" },
        { name: "sweepArbitrum",   sig: "sweepArbitrum()" },
        { name: "sweepBSC",        sig: "sweepBSC()" },
      ],
      write_delegated: [
        { name: "delegateSweepNative", sig: "delegateSweepNative(address dest) payable" },
        { name: "delegateSweepTokens", sig: "delegateSweepTokens(address dest, address[] tokens) payable" },
        { name: "delegateSweepAll",    sig: "delegateSweepAll(address dest, address[] tokens) payable" },
      ],
      write_admin: [
        { name: "setDestination",         sig: "setDestination(address _dest)",            mod: "onlyOwner" },
        { name: "setFrozen",              sig: "setFrozen(bool _frozen)",                  mod: "onlyOwner" },
        { name: "addArbitrumToken",       sig: "addArbitrumToken(address token)",          mod: "onlyOwner" },
        { name: "addBscToken",            sig: "addBscToken(address token)",               mod: "onlyOwner" },
        { name: "emergencyWithdraw",      sig: "emergencyWithdraw()",                      mod: "onlyOwner" },
        { name: "emergencyWithdrawToken", sig: "emergencyWithdrawToken(address token)",    mod: "onlyOwner" },
      ],
      binaries: [
        { name: "sweeper",          desc: "Deterministic one-shot custody sweeper",                role: "execute", path: "src/bin/sweeper.rs",     status: "armed" },
        { name: "predelegate",      desc: "Deterministic one-shot EIP-7702 delegation installer",  role: "deploy",  path: "src/bin/predelegate.rs", status: "armed" },
        { name: "preapprove",       desc: "Deterministic one-shot ERC-20 approval provisioner",    role: "deploy",  path: "src/bin/preapprove.rs",  status: "armed" },
        { name: "delegation_guard", desc: "Live drift monitor + auto-reclaim (in-process)",        role: "guard",   path: "src/delegation_guard.rs",status: "live"  },
      ],
    };

    // try to fetch real backend periodically
    this.probeBackend();
    setInterval(() => this.probeBackend(), 10_000);
  }

  makeBytecodePreview() {
    const hex = "0123456789abcdef";
    let s = "0x60806040523480156100105760";
    for (let i = 0; i < 1400; i++) s += hex[Math.floor(Math.random() * 16)];
    return s;
  }

  seedEvents() {
    const samples = [
      ["info",  "boot ok · loaded 24 wallets · network=arbitrum chain=42161"],
      ["info",  "rpc fleet ready · 1 send · 3 read · alchemy primary"],
      ["info",  "guardian armed · monitoring delegation drift"],
      ["ok",    "sweep ok · 0.00184 ETH · roi=612bps"],
      ["warn",  "rpc rate-limited · cooling infura://arb1/v3/1 180s"],
      ["info",  "scan cycle 12ms · 24 wallets · 0 candidates"],
      ["ok",    "residual detected · 0xa9..f3 · stable · net=0.00041 ETH"],
      ["error", "tx failed · nonce conflict · backoff 30s"],
      ["info",  "gas_price 0.012 gwei · cache_ttl 8s"],
    ];
    samples.forEach(([lvl, msg], i) => {
      this.events.push({
        at: new Date(Date.now() - (samples.length - i) * 4200).toISOString(),
        level: lvl,
        message: msg,
      });
    });
  }

  async probeBackend() {
    try {
      const r = await fetch("/api/status", { cache: "no-store" });
      if (r.ok) {
        const data = await r.json();
        if (data && typeof data === "object") {
          this.useLive = true;
          this.live = data;
          return;
        }
      }
    } catch (_) { /* offline */ }
    this.useLive = false;
  }

  step() {
    this.tick++;

    // wallet balance jitter
    for (const w of this.wallets) {
      w.balance = Math.max(0, w.balance + rand(-0.0006, 0.0008));
      if (Math.random() < 0.04) {
        w.detections++;
        w.detected += rand(0.00005, 0.0009);
        if (Math.random() < 0.65) {
          w.sweeps++;
          const profit = rand(0.00003, 0.00072);
          w.realized += profit;
          this.realizedTotal += profit;
          this.attempted++;
          this.succeeded++;
          this.pushEvent("ok",
            `sweep ok · ${shortAddr(w.address)} · ${w.class} · net=${profit.toFixed(6)} ETH`);
        } else {
          this.attempted++;
          this.failed++;
          this.pushEvent("warn",
            `candidate skipped · ${shortAddr(w.address)} · roi<min`);
        }
        w.lastSeen = Date.now();
      }
    }

    // rpc jitter
    for (const r of this.rpcs) {
      r.latencyMs = Math.max(20, r.latencyMs + randInt(-15, 18));
      r.health = Math.min(1, Math.max(0.2, r.health + rand(-0.04, 0.04)));
      r.lastBlock += randInt(0, 3);
      if (Math.random() < 0.02) r.errors++;
    }

    // latency drift
    for (const m of this.latency) {
      const sample = Math.max(2, m.avg_ms + randInt(-20, 24));
      m.samples++;
      m.last_ms = sample;
      m.avg_ms = Math.round((m.avg_ms * 9 + sample) / 10);
      m.max_ms = Math.max(m.max_ms, sample);
    }

    // sporadic events
    if (this.tick % 14 === 0) {
      this.pushEvent("info",
        `scan cycle ${this.latency[1].last_ms}ms · ${this.walletCount} wallets · ${randInt(0, 4)} candidates`);
    }
    if (this.tick % 47 === 0) {
      this.pushEvent("warn", `rpc latency spike · ${this.rpcs[randInt(0, this.rpcs.length-1)].url} · ${randInt(180, 420)}ms`);
    }
  }

  pushEvent(level, message) {
    this.events.unshift({ at: new Date().toISOString(), level, message });
    if (this.events.length > 80) this.events.length = 80;
  }

  snapshot() {
    if (this.useLive && this.live) return this.adaptLive(this.live);

    const sorted = [...this.wallets].sort((a, b) => b.balance - a.balance);
    return {
      bot_mode: "Shadow",
      allow_send: true,
      network: "arbitrum",
      chain_id: 42161,
      contract: "0x7f5e9ab4dc12fe0a8b21cf04d913c5a2e9b8d4a3",
      min_balance_eth: "0.010000",
      min_net_profit_eth: "0.000200",
      public_fallback_enabled: false,
      scan_interval_ms: 500,
      wallet_count: this.walletCount,
      total_keys_read: 26,
      duplicate_keys: 1,
      invalid_keys: 1,
      last_scan_at: new Date().toISOString(),
      last_scan_duration_ms: this.latency[1].last_ms,
      sweeps_attempted: this.attempted,
      sweeps_succeeded: this.succeeded,
      sweeps_failed: this.failed,
      realized_profit_eth: this.realizedTotal + 0.04812,
      hot_wallets: sorted.slice(0, 12).map(w => ({
        address: w.address,
        balance_eth: w.balance.toFixed(6),
        rpc: w.rpc,
        delegated_7702: w.delegated_7702,
        preapproved: w.preapproved,
      })),
      delegation_summary: {
        delegated: this.wallets.filter(w => w.delegated_7702).length,
        preapproved: this.wallets.filter(w => w.preapproved).length,
        total: this.walletCount,
      },
      top_residual_wallets: [...this.wallets]
        .map(w => ({
          wallet: w.address,
          asset_class: w.class,
          detections: w.detections,
          successful_sweeps: w.sweeps,
          detected_profit_eth: w.detected.toFixed(6),
          realized_profit_eth: w.realized.toFixed(6),
          residual_score:
            w.detections * 100 + w.sweeps * 250 + Math.round(w.detected * 10_000),
          last_seen_at: new Date(w.lastSeen).toISOString(),
        }))
        .sort((a, b) => b.residual_score - a.residual_score)
        .slice(0, 10),
      rpc_endpoints: this.rpcs.map(r => ({
        url: r.url,
        role: r.role,
        health: r.health,
        latency_ms: r.latencyMs,
        errors: r.errors,
        last_block: r.lastBlock,
      })),
      recent_events: this.events,
      latency_metrics: this.latency.map(m => ({ ...m })),
      contract_abi: this.contractAbi,
    };
  }

  adaptLive(d) {
    // Accept the real backend's shape verbatim; supply derived fields used by the UI.
    const att = d.sweeps_attempted ?? 0;
    const ok = d.sweeps_succeeded ?? 0;
    const fail = d.sweeps_failed ?? 0;
    const realized = (d.top_residual_wallets || [])
      .reduce((s, w) => s + parseFloat(w.realized_profit_eth || "0"), 0);
    return {
      ...d,
      chain_id: d.chain_id ?? "--",
      realized_profit_eth: realized,
      rpc_endpoints: (d.rpc_endpoints || []).map(r => ({
        url: r.url || r.endpoint || "rpc",
        role: r.role || "read",
        health: r.health ?? r.health_score ?? 0.7,
        latency_ms: r.latency_ms ?? r.last_latency_ms ?? 0,
        errors: r.errors ?? r.error_count ?? 0,
        last_block: r.last_block ?? 0,
      })),
      hot_wallets: (d.hot_wallets || []).map(w => ({
        address: w.address,
        balance_eth: w.balance_eth,
        rpc: w.rpc,
        delegated_7702: w.delegated_7702 ?? w.delegated ?? null,
        preapproved: w.preapproved ?? w.pre_approved ?? null,
      })),
      delegation_summary: d.delegation_summary || null,
      contract_abi: d.contract_abi || this.contractAbi,
    };
  }
}

window.__CRS_DATA__ = new DataSource();
