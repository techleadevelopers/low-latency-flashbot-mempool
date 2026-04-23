use crate::mev::inclusion_truth::BundleOutcome;
use crate::mev::market_truth::competition_reality::CompetitionReality;
use crate::mev::market_truth::edge_survival::EdgeSurvivalMetrics;
use crate::mev::market_truth::markout_engine::MarkoutMetrics;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionOutcomeReal {
    IncludedProfit,
    IncludedLoss,
    IncludedAdverseSelection,
    IncludedToxicFill,
    IncludedLateFill,
    IncludedPartialCapture,
    MissedOpportunity,
    NotIncluded,
    Reverted,
}

#[derive(Debug, Clone, Copy)]
pub struct ExecutionRealityInput {
    pub inclusion_outcome: BundleOutcome,
    pub net_execution_value: f64,
    pub latency_ms: u128,
    pub slippage_bps: f64,
    pub fill_ratio: f64,
}

pub fn classify_execution_outcome(
    input: ExecutionRealityInput,
    markout: &MarkoutMetrics,
    competition: &CompetitionReality,
    survival: &EdgeSurvivalMetrics,
) -> ExecutionOutcomeReal {
    match input.inclusion_outcome {
        BundleOutcome::Reverted => return ExecutionOutcomeReal::Reverted,
        BundleOutcome::NotIncluded | BundleOutcome::Outbid | BundleOutcome::Pending => {
            return if competition.competitor_capture_likelihood > 0.55 {
                ExecutionOutcomeReal::MissedOpportunity
            } else {
                ExecutionOutcomeReal::NotIncluded
            };
        }
        BundleOutcome::LateInclusion => return ExecutionOutcomeReal::IncludedLateFill,
        BundleOutcome::Included => {}
    }

    if input.fill_ratio < 0.98 {
        return ExecutionOutcomeReal::IncludedPartialCapture;
    }
    if markout.execution_toxicity_index >= 0.72 {
        return ExecutionOutcomeReal::IncludedToxicFill;
    }
    if markout.adverse_selection_score >= 0.62 || survival.survival_probability < 0.30 {
        return ExecutionOutcomeReal::IncludedAdverseSelection;
    }
    if input.net_execution_value < 0.0 || markout.edge_real_value < 0.0 {
        return ExecutionOutcomeReal::IncludedLoss;
    }
    ExecutionOutcomeReal::IncludedProfit
}
