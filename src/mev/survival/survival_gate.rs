use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy)]
pub struct SurvivalGateConfig {
    pub min_edge_survival_probability: f64,
    pub max_competitor_capture_likelihood: f64,
    pub max_latency_risk_score: f64,
    pub max_mempool_staleness_score: f64,
    pub min_pool_state_freshness_score: f64,
}

impl Default for SurvivalGateConfig {
    fn default() -> Self {
        Self {
            min_edge_survival_probability: 0.62,
            max_competitor_capture_likelihood: 0.48,
            max_latency_risk_score: 0.55,
            max_mempool_staleness_score: 0.38,
            min_pool_state_freshness_score: 0.60,
        }
    }
}

impl SurvivalGateConfig {
    pub fn from_env() -> Self {
        let default = Self::default();
        Self {
            min_edge_survival_probability: env_f64(
                "MEV_SURVIVAL_MIN_PROBABILITY",
                default.min_edge_survival_probability,
            ),
            max_competitor_capture_likelihood: env_f64(
                "MEV_SURVIVAL_MAX_COMPETITOR_CAPTURE",
                default.max_competitor_capture_likelihood,
            ),
            max_latency_risk_score: env_f64(
                "MEV_SURVIVAL_MAX_LATENCY_RISK",
                default.max_latency_risk_score,
            ),
            max_mempool_staleness_score: env_f64(
                "MEV_SURVIVAL_MAX_MEMPOOL_STALENESS",
                default.max_mempool_staleness_score,
            ),
            min_pool_state_freshness_score: env_f64(
                "MEV_SURVIVAL_MIN_POOL_FRESHNESS",
                default.min_pool_state_freshness_score,
            ),
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
        if input.edge_survival_probability < config.min_edge_survival_probability {
            return SurvivalGateDecision::Drop(SurvivalDropReason::EdgeDecay);
        }
        if input.execution_viability_window_ms <= input.estimated_latency_ms {
            return SurvivalGateDecision::Drop(SurvivalDropReason::ViabilityWindowExceeded);
        }
        if input.competitor_capture_likelihood >= config.max_competitor_capture_likelihood {
            return SurvivalGateDecision::Drop(SurvivalDropReason::CompetitorCapture);
        }
        if input.latency_risk_score > config.max_latency_risk_score {
            return SurvivalGateDecision::Drop(SurvivalDropReason::LatencyRisk);
        }
        if input.mempool_staleness_score > config.max_mempool_staleness_score {
            return SurvivalGateDecision::Drop(SurvivalDropReason::MempoolStale);
        }
        if input.pool_state_freshness_score < config.min_pool_state_freshness_score {
            return SurvivalGateDecision::Drop(SurvivalDropReason::PoolStateDecayed);
        }

        SurvivalGateDecision::Allow
    }
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .map(|value| value.clamp(0.0, 1.0))
        .unwrap_or(default)
}
