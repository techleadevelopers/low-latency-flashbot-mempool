use ethers::types::Address;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RouteCluster {
    pub pool: Address,
    pub token_in: Address,
    pub token_out: Address,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CompetitionRealityInput {
    pub route: RouteCluster,
    pub mempool_similar_count: u32,
    pub inclusion_delay_ms: u128,
    pub competitor_pressure: f64,
    pub competing_included_count: u32,
    pub observed_alpha_before: f64,
    pub observed_alpha_after: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CompetitionReality {
    pub opportunity_consumed_ratio: f64,
    pub pre_execution_alpha_decay_estimate: f64,
    pub late_entry_probability: f64,
    pub competitor_capture_likelihood: f64,
}

pub struct CompetitionRealityEngine;

impl CompetitionRealityEngine {
    pub fn compute(input: CompetitionRealityInput) -> CompetitionReality {
        let consumed = consumed_ratio(input.observed_alpha_before, input.observed_alpha_after);
        let latency_factor = (input.inclusion_delay_ms as f64 / 2_500.0).clamp(0.0, 1.0);
        let density = (input.mempool_similar_count as f64 / 12.0).clamp(0.0, 1.0);
        let included_competition = (input.competing_included_count as f64 / 4.0).clamp(0.0, 1.0);
        let pressure = input.competitor_pressure.clamp(0.0, 1.0);
        let late_entry_probability =
            (latency_factor * 0.45 + density * 0.20 + pressure * 0.25 + consumed * 0.10)
                .clamp(0.0, 1.0);
        let competitor_capture_likelihood =
            (included_competition * 0.35 + pressure * 0.30 + density * 0.20 + consumed * 0.15)
                .clamp(0.0, 1.0);

        CompetitionReality {
            opportunity_consumed_ratio: consumed,
            pre_execution_alpha_decay_estimate: input
                .observed_alpha_before
                .max(0.0)
                .saturating_sub_f64(input.observed_alpha_after.max(0.0)),
            late_entry_probability,
            competitor_capture_likelihood,
        }
    }
}

fn consumed_ratio(before: f64, after: f64) -> f64 {
    if !before.is_finite() || before <= 0.0 {
        return 0.0;
    }
    ((before - after.max(0.0)) / before).clamp(0.0, 1.0)
}

trait SaturatingSubF64 {
    fn saturating_sub_f64(self, rhs: f64) -> f64;
}

impl SaturatingSubF64 for f64 {
    fn saturating_sub_f64(self, rhs: f64) -> f64 {
        if self <= rhs {
            0.0
        } else {
            self - rhs
        }
    }
}
