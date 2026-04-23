use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct EdgeSurvivalInput {
    pub competition_pressure: f64,
    pub mempool_congestion: f64,
    pub historical_markout_degradation: f64,
    pub latency_ms: u128,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct EdgeSurvival {
    pub survival_probability: f64,
    pub decay_velocity: f64,
    pub execution_viability_window_ms: u64,
}

pub type EdgeSurvivalMetrics = EdgeSurvival;

pub struct EdgeSurvivalEngine;

impl EdgeSurvivalEngine {
    pub fn compute(input: EdgeSurvivalInput) -> EdgeSurvival {
        let pressure = input.competition_pressure.clamp(0.0, 1.0);
        let congestion = input.mempool_congestion.clamp(0.0, 1.0);
        let markout_degradation = input.historical_markout_degradation.clamp(0.0, 1.0);
        let latency_risk = (input.latency_ms as f64 / 2_000.0).clamp(0.0, 1.0);
        let decay_velocity = (pressure * 0.35
            + congestion * 0.20
            + markout_degradation * 0.25
            + latency_risk * 0.20)
            .clamp(0.0, 1.0);
        let survival_probability = (1.0 - decay_velocity).clamp(0.0, 1.0);
        let execution_viability_window_ms = ((1.0 - decay_velocity) * 5_000.0).max(100.0) as u64;

        EdgeSurvival {
            survival_probability,
            decay_velocity,
            execution_viability_window_ms,
        }
    }
}
