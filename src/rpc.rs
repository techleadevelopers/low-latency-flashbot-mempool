use crate::config::{Config, RpcPreference};
use ethers::providers::{Http, Provider};
use ethers::types::{Address, U256, U64};
use reqwest::Client;
use serde::Serialize;
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcKind {
    Alchemy,
    Infura,
}

impl RpcKind {
    fn as_str(self) -> &'static str {
        match self {
            RpcKind::Alchemy => "alchemy",
            RpcKind::Infura => "infura",
        }
    }
}

#[derive(Debug)]
struct RpcEndpointState {
    failures: u32,
    timeout_failures: u32,
    rate_limit_failures: u32,
    stale_failures: u32,
    cooldown_until: Option<Instant>,
    avg_latency: Option<Duration>,
    last_block: Option<u64>,
    last_block_at: Option<Instant>,
}

#[derive(Debug)]
struct ScoreCacheState {
    updated_at: Instant,
    read_scores: Vec<Option<f64>>,
    send_scores: Vec<Option<f64>>,
}

#[derive(Debug)]
pub struct RpcEndpoint {
    pub id: usize,
    pub name: String,
    pub url: String,
    pub kind: RpcKind,
    pub provider: Arc<Provider<Http>>,
    client: Client,
    state: Mutex<RpcEndpointState>,
}

#[derive(Clone, Debug)]
pub struct RpcHandle {
    pub id: usize,
    pub name: String,
    pub url: String,
    pub provider: Arc<Provider<Http>>,
    client: Client,
}

#[derive(Debug)]
pub struct RpcFleet {
    endpoints: Vec<Arc<RpcEndpoint>>,
    rotation: AtomicUsize,
    rate_limit_cooldown_secs: u64,
    read_preference: RpcPreference,
    send_preference: RpcPreference,
    score_cache: Mutex<ScoreCacheState>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RpcEndpointSnapshot {
    pub id: usize,
    pub name: String,
    pub url: String,
    pub kind: String,
    pub failures: u32,
    pub timeout_failures: u32,
    pub rate_limit_failures: u32,
    pub stale_failures: u32,
    pub cooldown_remaining_secs: u64,
    pub avg_latency_ms: Option<u128>,
    pub last_block: Option<u64>,
    pub block_age_secs: Option<u64>,
}

impl RpcFleet {
    pub fn from_config(config: &Config) -> Result<Self, Box<dyn std::error::Error>> {
        let mut endpoints = Vec::new();

        for (idx, (name, url)) in config.rpc_urls().into_iter().enumerate() {
            let kind = if idx == 0 {
                RpcKind::Alchemy
            } else {
                RpcKind::Infura
            };

            let provider = Provider::<Http>::try_from(url.as_str())?;
            endpoints.push(Arc::new(RpcEndpoint {
                id: idx,
                name,
                url,
                kind,
                provider: Arc::new(provider),
                client: Client::new(),
                state: Mutex::new(RpcEndpointState {
                    failures: 0,
                    timeout_failures: 0,
                    rate_limit_failures: 0,
                    stale_failures: 0,
                    cooldown_until: None,
                    avg_latency: None,
                    last_block: None,
                    last_block_at: None,
                }),
            }));
        }

        if endpoints.is_empty() {
            return Err("no RPC endpoints configured".into());
        }

        Ok(Self {
            score_cache: Mutex::new(ScoreCacheState {
                updated_at: Instant::now() - Duration::from_secs(1),
                read_scores: vec![None; endpoints.len()],
                send_scores: vec![None; endpoints.len()],
            }),
            endpoints,
            rotation: AtomicUsize::new(0),
            rate_limit_cooldown_secs: config.rpc_rate_limit_cooldown_secs,
            read_preference: config.rpc_read_preference,
            send_preference: config.rpc_send_preference,
        })
    }

    pub fn read_endpoint(&self) -> RpcHandle {
        self.select_endpoint()
    }

    pub fn send_endpoint(&self) -> RpcHandle {
        self.select_send_endpoint()
    }

    pub fn report_success(&self, endpoint_id: usize, latency: Duration) {
        if let Some(endpoint) = self.endpoints.get(endpoint_id) {
            let mut state = endpoint.state.lock().expect("rpc endpoint state lock");
            state.failures = 0;
            state.cooldown_until = None;
            state.avg_latency = Some(match state.avg_latency {
                Some(previous) => weighted_average(previous, latency),
                None => latency,
            });
        }
        self.invalidate_score_cache();
    }

    pub fn report_failure(&self, endpoint_id: usize) {
        if let Some(endpoint) = self.endpoints.get(endpoint_id) {
            let mut state = endpoint.state.lock().expect("rpc endpoint state lock");
            state.failures = state.failures.saturating_add(1);
            let cooldown_secs = (state.failures.min(5) as u64) * 15;
            state.cooldown_until =
                Some(Instant::now() + Duration::from_secs(cooldown_secs.max(15)));
        }
        self.invalidate_score_cache();
    }

    pub fn report_provider_error(&self, endpoint_id: usize, error_text: &str) {
        let lower = error_text.to_lowercase();
        if let Some(endpoint) = self.endpoints.get(endpoint_id) {
            let mut state = endpoint.state.lock().expect("rpc endpoint state lock");
            state.failures = state.failures.saturating_add(1);
            let mut cooldown_secs = (state.failures.min(5) as u64) * 15;
            if lower.contains("429")
                || lower.contains("rate limit")
                || lower.contains("too many requests")
            {
                state.rate_limit_failures = state.rate_limit_failures.saturating_add(1);
                cooldown_secs = self.rate_limit_cooldown_secs.max(cooldown_secs).max(60);
            } else if lower.contains("timeout") || lower.contains("timed out") {
                state.timeout_failures = state.timeout_failures.saturating_add(1);
            } else if lower.contains("stale") {
                state.stale_failures = state.stale_failures.saturating_add(1);
            }
            state.cooldown_until =
                Some(Instant::now() + Duration::from_secs(cooldown_secs.max(15)));
        }
        self.invalidate_score_cache();
    }

    pub fn report_block(&self, endpoint_id: usize, block: U64) {
        if let Some(endpoint) = self.endpoints.get(endpoint_id) {
            let mut state = endpoint.state.lock().expect("rpc endpoint state lock");
            state.last_block = Some(block.as_u64());
            state.last_block_at = Some(Instant::now());
        }
        self.invalidate_score_cache();
    }

    pub fn mark_stale(&self, endpoint_id: usize) {
        if let Some(endpoint) = self.endpoints.get(endpoint_id) {
            let mut state = endpoint.state.lock().expect("rpc endpoint state lock");
            state.failures = state.failures.saturating_add(1);
            state.stale_failures = state.stale_failures.saturating_add(1);
            state.cooldown_until = Some(Instant::now() + Duration::from_secs(20));
        }
        self.invalidate_score_cache();
    }

    pub fn endpoint_count(&self) -> usize {
        self.endpoints.len()
    }

    pub fn all_handles(&self) -> Vec<RpcHandle> {
        self.endpoints
            .iter()
            .map(|endpoint| self.to_handle(endpoint))
            .collect()
    }

    pub fn snapshot(&self) -> Vec<RpcEndpointSnapshot> {
        let now = Instant::now();
        self.endpoints
            .iter()
            .map(|endpoint| {
                let state = endpoint.state.lock().expect("rpc endpoint state lock");
                let cooldown_remaining_secs = state
                    .cooldown_until
                    .and_then(|until| until.checked_duration_since(now))
                    .map(|duration| duration.as_secs())
                    .unwrap_or(0);

                RpcEndpointSnapshot {
                    id: endpoint.id,
                    name: endpoint.name.clone(),
                    url: endpoint.url.clone(),
                    kind: endpoint.kind.as_str().to_string(),
                    failures: state.failures,
                    timeout_failures: state.timeout_failures,
                    rate_limit_failures: state.rate_limit_failures,
                    stale_failures: state.stale_failures,
                    cooldown_remaining_secs,
                    avg_latency_ms: state.avg_latency.map(|value| value.as_millis()),
                    last_block: state.last_block,
                    block_age_secs: state
                        .last_block_at
                        .map(|instant| instant.elapsed().as_secs()),
                }
            })
            .collect()
    }

    fn select_endpoint(&self) -> RpcHandle {
        let now = Instant::now();
        let mut candidates: Vec<(Arc<RpcEndpoint>, f64)> = self
            .endpoints
            .iter()
            .filter(|endpoint| self.matches_preference(endpoint.kind, self.read_preference))
            .filter_map(|endpoint| self.endpoint_score(endpoint, now, false, true))
            .collect();

        if candidates.is_empty() {
            candidates = self
                .endpoints
                .iter()
                .map(|endpoint| (endpoint.clone(), 10_000.0))
                .collect();
        }

        candidates.sort_by(|left, right| left.1.partial_cmp(&right.1).unwrap_or(Ordering::Equal));

        let top_n = candidates.len().min(3);
        let rotation = self.rotation.fetch_add(1, AtomicOrdering::Relaxed);
        let selected = &candidates[rotation % top_n].0;

        self.to_handle(selected)
    }

    fn select_send_endpoint(&self) -> RpcHandle {
        let now = Instant::now();
        let mut candidates: Vec<(Arc<RpcEndpoint>, f64)> = self
            .endpoints
            .iter()
            .filter(|endpoint| self.matches_preference(endpoint.kind, self.send_preference))
            .filter_map(|endpoint| self.endpoint_score(endpoint, now, true, true))
            .collect();

        if candidates.is_empty() {
            candidates = self
                .endpoints
                .iter()
                .filter_map(|endpoint| self.endpoint_score(endpoint, now, true, true))
                .collect();
        }

        if candidates.is_empty() {
            return self.select_endpoint();
        }

        candidates.sort_by(|left, right| left.1.partial_cmp(&right.1).unwrap_or(Ordering::Equal));
        self.to_handle(&candidates[0].0)
    }

    fn endpoint_score(
        &self,
        endpoint: &Arc<RpcEndpoint>,
        now: Instant,
        send_mode: bool,
        use_cache: bool,
    ) -> Option<(Arc<RpcEndpoint>, f64)> {
        if use_cache {
            if let Some(score) = self.get_cached_score(endpoint.id, send_mode, now) {
                return Some((endpoint.clone(), score));
            }
            return None;
        }

        let state = endpoint.state.lock().expect("rpc endpoint state lock");
        if matches!(state.cooldown_until, Some(until) if until > now) {
            return None;
        }

        let latency_ms = state
            .avg_latency
            .map(|value| value.as_secs_f64() * 1000.0)
            .unwrap_or(120.0);
        let failure_penalty = f64::from(state.failures) * 400.0;
        let rate_limit_penalty = f64::from(state.rate_limit_failures) * 5000.0;
        let stale_penalty = if matches!(state.last_block_at, Some(at) if at.elapsed() > Duration::from_secs(30))
        {
            500.0
        } else {
            0.0
        };
        let kind_bias = match (endpoint.kind, send_mode) {
            (RpcKind::Alchemy, true) => 0.0,
            (RpcKind::Alchemy, false) => -100.0,
            (RpcKind::Infura, true) => -50.0,
            (RpcKind::Infura, false) => 0.0,
        };
        let infura_rate_limited_penalty =
            if matches!(endpoint.kind, RpcKind::Infura) && state.rate_limit_failures > 0 {
                20_000.0
            } else {
                0.0
            };

        Some((
            endpoint.clone(),
            latency_ms
                + failure_penalty
                + rate_limit_penalty
                + stale_penalty
                + infura_rate_limited_penalty
                + kind_bias,
        ))
    }

    fn get_cached_score(&self, endpoint_id: usize, send_mode: bool, now: Instant) -> Option<f64> {
        {
            let cache = self.score_cache.lock().expect("rpc score cache lock");
            if cache.updated_at.elapsed() <= Duration::from_millis(500) {
                let scores = if send_mode {
                    &cache.send_scores
                } else {
                    &cache.read_scores
                };
                return scores.get(endpoint_id).copied().flatten();
            }
        }

        self.recompute_scores(now);

        let cache = self.score_cache.lock().expect("rpc score cache lock");
        let scores = if send_mode {
            &cache.send_scores
        } else {
            &cache.read_scores
        };
        scores.get(endpoint_id).copied().flatten()
    }

    fn recompute_scores(&self, now: Instant) {
        let mut read_scores = vec![None; self.endpoints.len()];
        let mut send_scores = vec![None; self.endpoints.len()];

        for endpoint in &self.endpoints {
            read_scores[endpoint.id] = self
                .endpoint_score(endpoint, now, false, false)
                .map(|(_, score)| score);
            send_scores[endpoint.id] = self
                .endpoint_score(endpoint, now, true, false)
                .map(|(_, score)| score);
        }

        let mut cache = self.score_cache.lock().expect("rpc score cache lock");
        cache.updated_at = Instant::now();
        cache.read_scores = read_scores;
        cache.send_scores = send_scores;
    }

    fn invalidate_score_cache(&self) {
        if let Ok(mut cache) = self.score_cache.lock() {
            cache.updated_at = Instant::now() - Duration::from_secs(1);
        }
    }

    fn to_handle(&self, endpoint: &Arc<RpcEndpoint>) -> RpcHandle {
        RpcHandle {
            id: endpoint.id,
            name: endpoint.name.clone(),
            url: endpoint.url.clone(),
            provider: endpoint.provider.clone(),
            client: endpoint.client.clone(),
        }
    }

    fn matches_preference(&self, kind: RpcKind, preference: RpcPreference) -> bool {
        match preference {
            RpcPreference::Auto => true,
            RpcPreference::Alchemy => matches!(kind, RpcKind::Alchemy),
            RpcPreference::Infura => matches!(kind, RpcKind::Infura),
        }
    }
}

impl RpcHandle {
    pub async fn get_balances_batch(&self, addresses: &[Address]) -> Result<Vec<U256>, String> {
        let payload: Vec<Value> = addresses
            .iter()
            .enumerate()
            .map(|(idx, address)| {
                json!({
                    "jsonrpc": "2.0",
                    "method": "eth_getBalance",
                    "params": [format!("{:#x}", address), "latest"],
                    "id": idx,
                })
            })
            .collect();

        let response = self
            .client
            .post(&self.url)
            .json(&payload)
            .send()
            .await
            .map_err(|err| err.to_string())?
            .error_for_status()
            .map_err(|err| err.to_string())?;

        let body: Value = response.json().await.map_err(|err| err.to_string())?;
        let items = body
            .as_array()
            .ok_or_else(|| "batch eth_getBalance did not return an array response".to_string())?;

        let mut ordered = vec![U256::zero(); addresses.len()];
        for item in items {
            let id = item
                .get("id")
                .and_then(Value::as_u64)
                .ok_or_else(|| "missing id in batch response".to_string())?
                as usize;
            let result = item
                .get("result")
                .and_then(Value::as_str)
                .ok_or_else(|| "missing result in batch response".to_string())?;
            let value = parse_hex_u256(result)?;
            if id < ordered.len() {
                ordered[id] = value;
            }
        }

        Ok(ordered)
    }
}

fn parse_hex_u256(value: &str) -> Result<U256, String> {
    let trimmed = value.strip_prefix("0x").unwrap_or(value);
    if trimmed.is_empty() {
        return Ok(U256::zero());
    }
    U256::from_str_radix(trimmed, 16).map_err(|err| err.to_string())
}

fn weighted_average(previous: Duration, current: Duration) -> Duration {
    let previous_ms = previous.as_secs_f64() * 1000.0;
    let current_ms = current.as_secs_f64() * 1000.0;
    Duration::from_secs_f64(((previous_ms * 0.7) + (current_ms * 0.3)) / 1000.0)
}
