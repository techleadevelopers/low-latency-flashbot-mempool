use crate::mev::execution::tx_lifecycle::ExecutionStage;
use crate::mev::market_truth::markout_engine::{MarketSnapshot, MarkoutEngine, MarkoutMetrics};
use crate::mev::state::event_store::{replay_after_dir, StateEvent};
use ethers::types::H256;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayExecutionInput {
    pub tx_hash: H256,
    pub entry_timestamp_ms: u64,
    pub entry_price: f64,
    pub actual_execution_price: f64,
    pub snapshots: Vec<MarketSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayTradeResult {
    pub tx_hash: H256,
    pub hypothetical_best_execution: f64,
    pub actual_execution: f64,
    pub lost_alpha: f64,
    pub markout: MarkoutMetrics,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReplayOutput {
    pub lost_alpha_per_trade: Vec<ReplayTradeResult>,
    pub execution_inefficiency_map: HashMap<H256, f64>,
    pub missed_opportunity_heatmap: HashMap<String, u64>,
}

pub struct ExecutionReplayEngine;

impl ExecutionReplayEngine {
    pub fn replay_event_store(
        event_dir: impl AsRef<Path>,
        inputs: &[ReplayExecutionInput],
    ) -> std::io::Result<ReplayOutput> {
        let events = replay_after_dir(event_dir, 0)?;
        let mut stages = HashMap::new();
        for envelope in events {
            match envelope.event {
                StateEvent::TxSigned(event) => {
                    stages.insert(event.tx_hash, ExecutionStage::Signed);
                }
                StateEvent::TxSubmitted(event) => {
                    stages.insert(event.tx_hash, ExecutionStage::Submitted);
                }
                StateEvent::TxIncluded(event) => {
                    stages.insert(event.tx_hash, ExecutionStage::Included);
                }
                StateEvent::TxDropped(event) => {
                    stages.insert(event.tx_hash, ExecutionStage::Dropped);
                }
                StateEvent::TxCancelled(event) => {
                    stages.insert(event.tx_hash, ExecutionStage::Cancelled);
                }
                StateEvent::TxReplaced(event) => {
                    stages.insert(event.new_tx_hash, ExecutionStage::Replaced);
                }
                _ => {}
            }
        }

        let mut output = ReplayOutput::default();
        for input in inputs {
            let markout = MarkoutEngine::compute(
                input.entry_timestamp_ms,
                input.entry_price,
                input.actual_execution_price,
                &input.snapshots,
            );
            let hypothetical_best_execution = input
                .snapshots
                .iter()
                .map(|snapshot| snapshot.price)
                .fold(input.actual_execution_price, f64::max);
            let lost_alpha = (hypothetical_best_execution - input.actual_execution_price).max(0.0);
            output
                .execution_inefficiency_map
                .insert(input.tx_hash, lost_alpha);
            if matches!(stages.get(&input.tx_hash), Some(ExecutionStage::Dropped)) {
                *output
                    .missed_opportunity_heatmap
                    .entry("dropped".to_string())
                    .or_default() += 1;
            }
            output.lost_alpha_per_trade.push(ReplayTradeResult {
                tx_hash: input.tx_hash,
                hypothetical_best_execution,
                actual_execution: input.actual_execution_price,
                lost_alpha,
                markout,
            });
        }
        output
            .lost_alpha_per_trade
            .sort_by_key(|result| result.tx_hash);
        Ok(output)
    }
}
