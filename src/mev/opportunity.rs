use crate::mev::execution::payload_builder::ExecutionPayload;
use ethers::types::{Address, TxHash, U256};
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpportunityKind {
    Backrun,
}

impl OpportunityKind {
    pub fn as_str(self) -> &'static str {
        match self {
            OpportunityKind::Backrun => "backrun",
        }
    }
}

#[derive(Debug, Clone)]
pub struct OpportunityScore {
    pub expected_profit_wei: U256,
    pub execution_cost_wei: U256,
    pub slippage_adjusted_profit_wei: U256,
    pub roi_bps: u64,
    pub risk_score: u16,
    pub competition_score: u16,
    pub confidence_score: u16,
}

impl OpportunityScore {
    pub fn passes(
        &self,
        min_profit_wei: U256,
        min_roi_bps: u64,
        max_risk_score: u16,
        max_competition_score: u16,
        min_confidence_score: u16,
    ) -> bool {
        self.slippage_adjusted_profit_wei >= min_profit_wei
            && self.roi_bps >= min_roi_bps
            && self.risk_score <= max_risk_score
            && self.competition_score <= max_competition_score
            && self.confidence_score >= min_confidence_score
    }
}

#[derive(Debug, Clone)]
pub struct MevOpportunity {
    pub id: String,
    pub kind: OpportunityKind,
    pub detected_at: Instant,
    pub victim_tx: TxHash,
    pub target: Address,
    pub input_token: Address,
    pub output_token: Address,
    pub notional_wei: U256,
    pub gas_limit: u64,
    pub private_only: bool,
    pub score: OpportunityScore,
    pub execution_payload: Option<ExecutionPayload>,
}

impl MevOpportunity {
    pub fn age_ms(&self) -> u128 {
        self.detected_at.elapsed().as_millis()
    }

    pub fn allocation_wei(&self, max_allocation_wei: U256) -> U256 {
        self.notional_wei.min(max_allocation_wei)
    }
}

pub fn wei_to_eth_f64(wei: U256) -> f64 {
    wei.to_string().parse::<f64>().unwrap_or(0.0) / 1e18
}

pub fn roi_bps(profit_wei: U256, cost_wei: U256) -> u64 {
    if cost_wei.is_zero() {
        return 0;
    }
    let profit = profit_wei.as_u128() as f64;
    let cost = cost_wei.as_u128() as f64;
    ((profit / cost) * 10_000.0) as u64
}

pub fn clamp_score(value: i64) -> u16 {
    value.clamp(0, 100) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_high_competition_even_with_profit() {
        let score = OpportunityScore {
            expected_profit_wei: U256::from(10_000u64),
            execution_cost_wei: U256::from(1_000u64),
            slippage_adjusted_profit_wei: U256::from(9_000u64),
            roi_bps: 90_000,
            risk_score: 10,
            competition_score: 90,
            confidence_score: 95,
        };

        assert!(!score.passes(U256::from(1_000u64), 1_000, 30, 25, 80));
    }
}
