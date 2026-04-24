use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SurvivalAdaptiveParams {
    pub survival_probability_threshold: f64,
    pub max_competitor_capture: f64,
    pub max_latency_risk: f64,
    pub max_staleness: f64,
    pub min_pool_freshness: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SurvivalGateConfig {
    pub survival_probability_threshold: f64,
    pub max_competitor_capture: f64,
    pub max_latency_risk: f64,
    pub max_staleness: f64,
    pub min_pool_freshness: f64,
}

impl Default for SurvivalGateConfig {
    fn default() -> Self {
        Self {
            survival_probability_threshold: 0.62,
            max_competitor_capture: 0.48,
            max_latency_risk: 0.55,
            max_staleness: 0.38,
            min_pool_freshness: 0.60,
        }
    }
}

impl SurvivalGateConfig {
    pub fn from_env() -> Self {
        let default = Self::default();
        Self {
            survival_probability_threshold: env_f64(
                "MEV_SURVIVAL_MIN_PROBABILITY",
                default.survival_probability_threshold,
            ),
            max_competitor_capture: env_f64(
                "MEV_SURVIVAL_MAX_COMPETITOR_CAPTURE",
                default.max_competitor_capture,
            ),
            max_latency_risk: env_f64("MEV_SURVIVAL_MAX_LATENCY_RISK", default.max_latency_risk),
            max_staleness: env_f64("MEV_SURVIVAL_MAX_MEMPOOL_STALENESS", default.max_staleness),
            min_pool_freshness: env_f64(
                "MEV_SURVIVAL_MIN_POOL_FRESHNESS",
                default.min_pool_freshness,
            ),
        }
    }

    #[inline]
    pub fn current() -> Self {
        adaptive_store().load().into()
    }

    pub fn update_adaptive(params: SurvivalAdaptiveParams) -> SurvivalAdaptiveParams {
        let normalized = normalize(params);
        adaptive_store().store(normalized);
        normalized
    }
}

impl From<SurvivalGateConfig> for SurvivalAdaptiveParams {
    fn from(value: SurvivalGateConfig) -> Self {
        Self {
            survival_probability_threshold: value.survival_probability_threshold,
            max_competitor_capture: value.max_competitor_capture,
            max_latency_risk: value.max_latency_risk,
            max_staleness: value.max_staleness,
            min_pool_freshness: value.min_pool_freshness,
        }
    }
}

impl From<SurvivalAdaptiveParams> for SurvivalGateConfig {
    fn from(value: SurvivalAdaptiveParams) -> Self {
        Self {
            survival_probability_threshold: value.survival_probability_threshold,
            max_competitor_capture: value.max_competitor_capture,
            max_latency_risk: value.max_latency_risk,
            max_staleness: value.max_staleness,
            min_pool_freshness: value.min_pool_freshness,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SurvivalGateInput {
    pub edge_survival_probability: f64,
    pub execution_viability_window_ms: u64,
    pub estimated_latency_ms: u64,
    pub competitor_capture_likelihood: f64,
    pub latency_risk_score: f64,
    pub mempool_staleness_score: f64,
    pub pool_state_freshness_score: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SurvivalDropReason {
    EdgeDecay,
    ViabilityWindowExceeded,
    CompetitorCapture,
    LatencyRisk,
    MempoolStale,
    PoolStateDecayed,
}

impl SurvivalDropReason {
    pub fn as_str(self) -> &'static str {
        match self {
            SurvivalDropReason::EdgeDecay => "edge_decay",
            SurvivalDropReason::ViabilityWindowExceeded => "viability_window_exceeded",
            SurvivalDropReason::CompetitorCapture => "competitor_capture",
            SurvivalDropReason::LatencyRisk => "latency_risk",
            SurvivalDropReason::MempoolStale => "mempool_stale",
            SurvivalDropReason::PoolStateDecayed => "pool_state_decayed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SurvivalGateDecision {
    Allow,
    Drop(SurvivalDropReason),
}

pub struct SurvivalGate;

impl SurvivalGate {
    #[inline]
    pub fn evaluate(config: SurvivalGateConfig, input: SurvivalGateInput) -> SurvivalGateDecision {
        if input.edge_survival_probability < config.survival_probability_threshold {
            return SurvivalGateDecision::Drop(SurvivalDropReason::EdgeDecay);
        }
        if input.execution_viability_window_ms <= input.estimated_latency_ms {
            return SurvivalGateDecision::Drop(SurvivalDropReason::ViabilityWindowExceeded);
        }
        if input.competitor_capture_likelihood >= config.max_competitor_capture {
            return SurvivalGateDecision::Drop(SurvivalDropReason::CompetitorCapture);
        }
        if input.latency_risk_score > config.max_latency_risk {
            return SurvivalGateDecision::Drop(SurvivalDropReason::LatencyRisk);
        }
        if input.mempool_staleness_score > config.max_staleness {
            return SurvivalGateDecision::Drop(SurvivalDropReason::MempoolStale);
        }
        if input.pool_state_freshness_score < config.min_pool_freshness {
            return SurvivalGateDecision::Drop(SurvivalDropReason::PoolStateDecayed);
        }

        SurvivalGateDecision::Allow
    }
}

struct AdaptiveStore {
    survival_probability_threshold: AtomicU64,
    max_competitor_capture: AtomicU64,
    max_latency_risk: AtomicU64,
    max_staleness: AtomicU64,
    min_pool_freshness: AtomicU64,
}

impl AdaptiveStore {
    fn new(initial: SurvivalAdaptiveParams) -> Self {
        let initial = normalize(initial);
        Self {
            survival_probability_threshold: AtomicU64::new(
                initial.survival_probability_threshold.to_bits(),
            ),
            max_competitor_capture: AtomicU64::new(initial.max_competitor_capture.to_bits()),
            max_latency_risk: AtomicU64::new(initial.max_latency_risk.to_bits()),
            max_staleness: AtomicU64::new(initial.max_staleness.to_bits()),
            min_pool_freshness: AtomicU64::new(initial.min_pool_freshness.to_bits()),
        }
    }

    #[inline]
    fn load(&self) -> SurvivalAdaptiveParams {
        SurvivalAdaptiveParams {
            survival_probability_threshold: f64::from_bits(
                self.survival_probability_threshold.load(Ordering::Relaxed),
            ),
            max_competitor_capture: f64::from_bits(
                self.max_competitor_capture.load(Ordering::Relaxed),
            ),
            max_latency_risk: f64::from_bits(self.max_latency_risk.load(Ordering::Relaxed)),
            max_staleness: f64::from_bits(self.max_staleness.load(Ordering::Relaxed)),
            min_pool_freshness: f64::from_bits(self.min_pool_freshness.load(Ordering::Relaxed)),
        }
    }

    #[inline]
    fn store(&self, params: SurvivalAdaptiveParams) {
        self.survival_probability_threshold.store(
            params.survival_probability_threshold.to_bits(),
            Ordering::Relaxed,
        );
        self.max_competitor_capture
            .store(params.max_competitor_capture.to_bits(), Ordering::Relaxed);
        self.max_latency_risk
            .store(params.max_latency_risk.to_bits(), Ordering::Relaxed);
        self.max_staleness
            .store(params.max_staleness.to_bits(), Ordering::Relaxed);
        self.min_pool_freshness
            .store(params.min_pool_freshness.to_bits(), Ordering::Relaxed);
    }
}

fn adaptive_store() -> &'static AdaptiveStore {
    static STORE: OnceLock<AdaptiveStore> = OnceLock::new();
    STORE.get_or_init(|| AdaptiveStore::new(SurvivalGateConfig::from_env().into()))
}

fn normalize(params: SurvivalAdaptiveParams) -> SurvivalAdaptiveParams {
    SurvivalAdaptiveParams {
        survival_probability_threshold: params.survival_probability_threshold.clamp(0.45, 0.95),
        max_competitor_capture: params.max_competitor_capture.clamp(0.05, 0.95),
        max_latency_risk: params.max_latency_risk.clamp(0.05, 0.95),
        max_staleness: params.max_staleness.clamp(0.05, 0.95),
        min_pool_freshness: params.min_pool_freshness.clamp(0.05, 0.95),
    }
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .map(|value| value.clamp(0.0, 1.0))
        .unwrap_or(default)
}
