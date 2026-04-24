use crate::mev::inclusion_truth::InclusionTruth;
use crate::mev::market_truth::competition_reality::{
    CompetitionReality, CompetitionRealityEngine, CompetitionRealityInput,
};
use crate::mev::market_truth::edge_survival::{
    EdgeSurvivalEngine, EdgeSurvivalInput, EdgeSurvivalMetrics,
};
use crate::mev::market_truth::execution_outcome_real::{
    classify_execution_outcome, ExecutionOutcomeReal, ExecutionRealityInput, OutcomeConfidence,
};
use crate::mev::market_truth::execution_replay_engine::{ExecutionReplayEngine, ReplayResult};
use crate::mev::market_truth::market_snapshot_engine::MarketSnapshot;
use crate::mev::market_truth::markout_engine::{MarkoutEngine, MarkoutMetrics};
use crate::mev::state::event_store::{MarketTruthUpdate, ObservedSnapshot, StateEvent};
use std::sync::Arc;

#[derive(Debug, Clone, Copy)]
pub struct DataQuality {
    pub has_snapshots: bool,
    pub has_real_execution_price: bool,
    pub has_real_slippage: bool,
    pub has_balance_delta: bool,
}

#[derive(Debug, Clone)]
pub struct MarketTruthInput {
    pub truth: InclusionTruth,
    pub entry_timestamp_ms: u64,
    pub expected_price: f64,
    pub execution_price: f64,
    pub net_execution_value: f64,
    pub balance_delta_wei: ethers::types::U256,
    pub gas_paid_wei: ethers::types::U256,
    pub slippage_bps: f64,
    pub fill_ratio: f64,
    pub market_snapshots: Vec<MarketSnapshot>,
    pub data_quality: DataQuality,
    pub competition: CompetitionRealityInput,
    pub survival: EdgeSurvivalInput,
    pub expected_execution_value: f64,
    pub observed_best_execution_value: f64,
}

#[derive(Debug, Clone)]
pub struct MarketTruthReport {
    pub tx_hash: ethers::types::H256,
    pub outcome: ExecutionOutcomeReal,
    pub outcome_confidence: OutcomeConfidence,
    pub data_quality: DataQuality,
    pub pool_address: ethers::types::Address,
    pub profit_token: ethers::types::Address,
    pub profit_recipient: ethers::types::Address,
    pub expected_price: f64,
    pub execution_price: f64,
    pub realized_pnl: f64,
    pub balance_delta_wei: ethers::types::U256,
    pub gas_paid_wei: ethers::types::U256,
    pub slippage_bps: f64,
    pub fill_ratio: f64,
    pub latency_ms: u64,
    pub market_snapshots: Vec<MarketSnapshot>,
    pub markout: MarkoutMetrics,
    pub competition: CompetitionReality,
    pub survival: EdgeSurvivalMetrics,
    pub replay: ReplayResult,
}

pub struct TruthPipeline;

impl TruthPipeline {
    pub fn run(input: MarketTruthInput) -> MarketTruthReport {
        let markout = MarkoutEngine::compute(
            input.entry_timestamp_ms,
            input.expected_price,
            input.execution_price,
            &input.market_snapshots,
        );
        let competition = CompetitionRealityEngine::compute(input.competition);
        let survival = EdgeSurvivalEngine::compute(input.survival);
        let included = matches!(
            input.truth.outcome,
            crate::mev::inclusion_truth::BundleOutcome::Included
                | crate::mev::inclusion_truth::BundleOutcome::LateInclusion
        );
        let replay = ExecutionReplayEngine::compute_single(
            input.net_execution_value,
            input.observed_best_execution_value,
            input.expected_execution_value,
            included,
        );
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
        let outcome_confidence =
            if !input.data_quality.has_snapshots || !input.data_quality.has_real_execution_price {
                OutcomeConfidence::Low
            } else if input.data_quality.has_real_slippage && input.data_quality.has_balance_delta {
                OutcomeConfidence::High
            } else {
                OutcomeConfidence::Medium
            };
        MarketTruthReport {
            tx_hash: input.truth.tx_hash,
            outcome,
            outcome_confidence,
            data_quality: input.data_quality,
            pool_address: input.truth.pool_address,
            profit_token: input.truth.profit_token,
            profit_recipient: input.truth.profit_recipient,
            expected_price: input.expected_price,
            execution_price: input.execution_price,
            realized_pnl: input.net_execution_value,
            balance_delta_wei: input.balance_delta_wei,
            gas_paid_wei: input.gas_paid_wei,
            slippage_bps: input.slippage_bps,
            fill_ratio: input.fill_ratio,
            latency_ms: input.truth.latency_ms.min(u128::from(u64::MAX)) as u64,
            market_snapshots: input.market_snapshots,
            markout,
            competition,
            survival,
            replay,
        }
    }

    pub fn append_report(
        event_store: &Arc<crate::mev::state::event_store::EventStore>,
        report: &MarketTruthReport,
    ) {
        let _ = event_store.append(StateEvent::MarketTruthUpdate(MarketTruthUpdate {
            tx_hash: report.tx_hash,
            outcome: format!("{:?}", report.outcome),
            outcome_confidence: format!("{:?}", report.outcome_confidence),
            has_snapshots: report.data_quality.has_snapshots,
            has_real_execution_price: report.data_quality.has_real_execution_price,
            has_real_slippage: report.data_quality.has_real_slippage,
            has_balance_delta: report.data_quality.has_balance_delta,
            pool_address: report.pool_address,
            profit_token: report.profit_token,
            profit_recipient: report.profit_recipient,
            expected_price: report.expected_price,
            execution_price: report.execution_price,
            realized_pnl: report.realized_pnl,
            balance_delta_wei: report.balance_delta_wei,
            gas_paid_wei: report.gas_paid_wei,
            slippage_bps: report.slippage_bps,
            fill_ratio: report.fill_ratio,
            latency_ms: report.latency_ms,
            markout_100ms: report.markout.markout_100ms,
            markout_500ms: report.markout.markout_500ms,
            markout_1s: report.markout.markout_1s,
            markout_5s: report.markout.markout_5s,
            edge_real_value: report.markout.edge_real_value,
            adverse_selection_score: report.markout.adverse_selection_score,
            fill_quality_score: report.markout.fill_quality_score,
            execution_toxicity_index: report.markout.execution_toxicity_index,
            opportunity_consumed_ratio: report.competition.opportunity_consumed_ratio,
            alpha_decay_estimate: report.competition.alpha_decay_estimate,
            late_entry_probability: report.competition.late_entry_probability,
            competitor_capture_likelihood: report.competition.competitor_capture_likelihood,
            edge_survival_probability: report.survival.survival_probability,
            decay_velocity: report.survival.decay_velocity,
            execution_viability_window_ms: report.survival.execution_viability_window_ms,
            lost_alpha: report.replay.lost_alpha,
            inefficiency_score: report.replay.inefficiency_score,
            missed_opportunity: report.replay.missed_opportunity,
            snapshots: report.market_snapshots(),
        }));
    }
}

impl MarketTruthReport {
    fn market_snapshots(&self) -> Vec<ObservedSnapshot> {
        self.market_snapshots
            .iter()
            .map(|snapshot| ObservedSnapshot {
                timestamp_ms: snapshot.timestamp_ms,
                block_number: snapshot.block_number,
                pool_address: snapshot.pool_address,
                price: snapshot.price,
                reserve_in: snapshot.reserve_in,
                reserve_out: snapshot.reserve_out,
            })
            .collect()
    }
}
