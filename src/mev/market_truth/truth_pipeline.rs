use crate::mev::inclusion_truth::InclusionTruth;
use crate::mev::market_truth::competition_reality::{
    CompetitionReality, CompetitionRealityEngine, CompetitionRealityInput,
};
use crate::mev::market_truth::edge_survival::{
    EdgeSurvivalEngine, EdgeSurvivalInput, EdgeSurvivalMetrics,
};
use crate::mev::market_truth::execution_outcome_real::{
    classify_execution_outcome, ExecutionOutcomeReal, ExecutionRealityInput,
};
use crate::mev::market_truth::markout_engine::{MarketSnapshot, MarkoutEngine, MarkoutMetrics};
use crate::mev::state::event_store::{MarketTruthUpdate, StateEvent};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketTruthInput {
    pub truth: InclusionTruth,
    pub entry_timestamp_ms: u64,
    pub entry_price: f64,
    pub execution_price: f64,
    pub net_execution_value: f64,
    pub slippage_bps: f64,
    pub fill_ratio: f64,
    pub market_snapshots: Vec<MarketSnapshot>,
    pub competition: CompetitionRealityInput,
    pub survival: EdgeSurvivalInput,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketTruthReport {
    pub tx_hash: ethers::types::H256,
    pub outcome: ExecutionOutcomeReal,
    pub markout: MarkoutMetrics,
    pub competition: CompetitionReality,
    pub survival: EdgeSurvivalMetrics,
}

pub struct TruthPipeline;

impl TruthPipeline {
    pub fn run(input: MarketTruthInput) -> MarketTruthReport {
        let markout = MarkoutEngine::compute(
            input.entry_timestamp_ms,
            input.entry_price,
            input.execution_price,
            &input.market_snapshots,
        );
        let competition = CompetitionRealityEngine::compute(input.competition);
        let survival = EdgeSurvivalEngine::compute(input.survival);
        let outcome = classify_execution_outcome(
            ExecutionRealityInput {
                inclusion_outcome: input.truth.outcome,
                net_execution_value: input.net_execution_value,
                latency_ms: input.truth.latency_ms,
                slippage_bps: input.slippage_bps,
                fill_ratio: input.fill_ratio,
            },
            &markout,
            &competition,
            &survival,
        );
        MarketTruthReport {
            tx_hash: input.truth.tx_hash,
            outcome,
            markout,
            competition,
            survival,
        }
    }

    pub fn append_report(
        event_store: &Arc<crate::mev::state::event_store::EventStore>,
        report: &MarketTruthReport,
    ) {
        let _ = event_store.append(StateEvent::MarketTruthUpdate(MarketTruthUpdate {
            tx_hash: report.tx_hash,
            outcome: format!("{:?}", report.outcome),
            edge_real_value: report.markout.edge_real_value,
            adverse_selection_score: report.markout.adverse_selection_score,
            fill_quality_score: report.markout.fill_quality_score,
            execution_toxicity_index: report.markout.execution_toxicity_index,
            opportunity_consumed_ratio: report.competition.opportunity_consumed_ratio,
            late_entry_probability: report.competition.late_entry_probability,
            competitor_capture_likelihood: report.competition.competitor_capture_likelihood,
            edge_survival_probability: report.survival.edge_survival_probability,
            decay_velocity: report.survival.decay_velocity,
            execution_viability_window_ms: report.survival.execution_viability_window_ms,
        }));
    }
}
