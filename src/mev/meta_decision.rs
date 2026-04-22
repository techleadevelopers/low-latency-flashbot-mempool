use ethers::types::{Address, H256};

#[derive(Debug, Clone, Copy)]
pub struct MetaOpportunity {
    pub expected_profit: f64,
    pub gas_cost: f64,
    pub slippage_estimate: f64,
    pub price_impact: f64,
    pub liquidity_depth: f64,
    pub latency_estimate_ms: f64,
    pub pool_address: Address,
    pub victim_tx_hash: Option<H256>,
    pub mempool_density: f64,
    pub tx_popularity: f64,
    pub pool_activity_recent: f64,
    pub historical_failure_rate: f64,
    pub block_delay_ms: f64,
    pub simulation_age_ms: f64,
    pub victim_confirmed_or_replaced: bool,
    pub capital_gas_window_remaining: f64,
    pub daily_drawdown_remaining: f64,
    pub trade_allocation_remaining: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct MetaDecisionConfig {
    pub profit_multiplier: f64,
    pub max_price_impact: f64,
    pub min_liquidity: f64,
    pub max_slippage: f64,
    pub max_competition_threshold: f64,
    pub max_block_delay_ms: f64,
    pub max_simulation_age_ms: f64,
    pub min_real_profit: f64,
    pub execution_threshold: f64,
    pub failure_penalty_multiplier: f64,
    pub max_latency_ms: f64,
}

impl Default for MetaDecisionConfig {
    fn default() -> Self {
        Self {
            profit_multiplier: 1.8,
            max_price_impact: 0.025,
            min_liquidity: 25_000.0,
            max_slippage: 0.003,
            max_competition_threshold: 0.62,
            max_block_delay_ms: 900.0,
            max_simulation_age_ms: 1_200.0,
            min_real_profit: 2.0,
            execution_threshold: 0.78,
            failure_penalty_multiplier: 1.25,
            max_latency_ms: 350.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    LowProfit,
    HighPriceImpact,
    LowLiquidity,
    HighSlippage,
    HighCompetition,
    StaleBlock,
    StaleSimulation,
    VictimUnavailable,
    LowRealisticProfit,
    LowConfidence,
    CapitalLimit,
}

impl SkipReason {
    pub fn as_str(self) -> &'static str {
        match self {
            SkipReason::LowProfit => "low_profit",
            SkipReason::HighPriceImpact => "high_price_impact",
            SkipReason::LowLiquidity => "low_liquidity",
            SkipReason::HighSlippage => "high_slippage",
            SkipReason::HighCompetition => "high_competition",
            SkipReason::StaleBlock => "stale_block",
            SkipReason::StaleSimulation => "stale_simulation",
            SkipReason::VictimUnavailable => "victim_unavailable",
            SkipReason::LowRealisticProfit => "low_realistic_profit",
            SkipReason::LowConfidence => "low_confidence",
            SkipReason::CapitalLimit => "capital_limit",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum MetaDecision {
    Execute {
        opportunity: MetaOpportunity,
        score: MetaScore,
    },
    Skip(SkipReason),
}

#[derive(Debug, Clone, Copy)]
pub struct MetaScore {
    pub competition_score: f64,
    pub realistic_profit: f64,
    pub confidence_score: f64,
    pub expected_value: f64,
}

pub struct MetaDecisionEngine {
    config: MetaDecisionConfig,
}

impl MetaDecisionEngine {
    pub const fn new(config: MetaDecisionConfig) -> Self {
        Self { config }
    }

    #[inline(always)]
    pub fn decide(&self, opportunity: MetaOpportunity) -> MetaDecision {
        if opportunity.expected_profit <= opportunity.gas_cost * self.config.profit_multiplier {
            return MetaDecision::Skip(SkipReason::LowProfit);
        }
        if opportunity.price_impact > self.config.max_price_impact {
            return MetaDecision::Skip(SkipReason::HighPriceImpact);
        }
        if opportunity.liquidity_depth < self.config.min_liquidity {
            return MetaDecision::Skip(SkipReason::LowLiquidity);
        }
        if opportunity.slippage_estimate > self.config.max_slippage {
            return MetaDecision::Skip(SkipReason::HighSlippage);
        }

        let competition_score = competition_score(&opportunity);
        if competition_score > self.config.max_competition_threshold {
            return MetaDecision::Skip(SkipReason::HighCompetition);
        }

        if opportunity.block_delay_ms > self.config.max_block_delay_ms {
            return MetaDecision::Skip(SkipReason::StaleBlock);
        }
        if opportunity.simulation_age_ms > self.config.max_simulation_age_ms {
            return MetaDecision::Skip(SkipReason::StaleSimulation);
        }
        if opportunity.victim_confirmed_or_replaced {
            return MetaDecision::Skip(SkipReason::VictimUnavailable);
        }

        if opportunity.capital_gas_window_remaining < opportunity.gas_cost
            || opportunity.daily_drawdown_remaining < opportunity.gas_cost
            || opportunity.trade_allocation_remaining <= 0.0
        {
            return MetaDecision::Skip(SkipReason::CapitalLimit);
        }

        let failure_penalty =
            opportunity.gas_cost * competition_score * self.config.failure_penalty_multiplier;
        let realistic_profit = opportunity.expected_profit
            - opportunity.gas_cost
            - opportunity.slippage_estimate
            - failure_penalty;
        if realistic_profit < self.config.min_real_profit {
            return MetaDecision::Skip(SkipReason::LowRealisticProfit);
        }

        let confidence_score = confidence_score(
            &opportunity,
            realistic_profit,
            competition_score,
            &self.config,
        );
        if confidence_score < self.config.execution_threshold {
            return MetaDecision::Skip(SkipReason::LowConfidence);
        }

        let expected_value = realistic_profit * confidence_score * (1.0 - competition_score);
        MetaDecision::Execute {
            opportunity,
            score: MetaScore {
                competition_score,
                realistic_profit,
                confidence_score,
                expected_value,
            },
        }
    }

    pub fn rank_top_n<'a>(
        &self,
        opportunities: &'a [MetaOpportunity],
        out: &mut [Option<(usize, MetaScore)>],
    ) {
        for slot in out.iter_mut() {
            *slot = None;
        }

        for (idx, opportunity) in opportunities.iter().copied().enumerate() {
            let MetaDecision::Execute { score, .. } = self.decide(opportunity) else {
                continue;
            };
            insert_ranked(out, idx, score);
        }
    }
}

#[inline(always)]
fn competition_score(opportunity: &MetaOpportunity) -> f64 {
    clamp01(
        opportunity.mempool_density * 0.30
            + opportunity.tx_popularity * 0.25
            + opportunity.pool_activity_recent * 0.25
            + opportunity.historical_failure_rate * 0.20,
    )
}

#[inline(always)]
fn confidence_score(
    opportunity: &MetaOpportunity,
    realistic_profit: f64,
    competition_score: f64,
    config: &MetaDecisionConfig,
) -> f64 {
    let profit_margin = clamp01(realistic_profit / (opportunity.gas_cost * 4.0).max(1.0));
    let liquidity_factor = clamp01(opportunity.liquidity_depth / (config.min_liquidity * 8.0));
    let competition_inverse = 1.0 - competition_score;
    let latency_factor = 1.0 - clamp01(opportunity.latency_estimate_ms / config.max_latency_ms);

    clamp01(
        profit_margin * 0.38
            + liquidity_factor * 0.22
            + competition_inverse * 0.28
            + latency_factor * 0.12,
    )
}

#[inline(always)]
fn insert_ranked(out: &mut [Option<(usize, MetaScore)>], idx: usize, score: MetaScore) {
    let mut carry = Some((idx, score));
    for slot in out.iter_mut() {
        match (*slot, carry) {
            (None, Some(value)) => {
                *slot = Some(value);
                return;
            }
            (Some(existing), Some(value)) if value.1.expected_value > existing.1.expected_value => {
                *slot = Some(value);
                carry = Some(existing);
            }
            _ => {}
        }
    }
}

#[inline(always)]
fn clamp01(value: f64) -> f64 {
    if value <= 0.0 {
        0.0
    } else if value >= 1.0 {
        1.0
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good() -> MetaOpportunity {
        MetaOpportunity {
            expected_profit: 20.0,
            gas_cost: 3.0,
            slippage_estimate: 0.002,
            price_impact: 0.01,
            liquidity_depth: 500_000.0,
            latency_estimate_ms: 80.0,
            pool_address: Address::zero(),
            victim_tx_hash: Some(H256::zero()),
            mempool_density: 0.15,
            tx_popularity: 0.10,
            pool_activity_recent: 0.20,
            historical_failure_rate: 0.10,
            block_delay_ms: 100.0,
            simulation_age_ms: 150.0,
            victim_confirmed_or_replaced: false,
            capital_gas_window_remaining: 100.0,
            daily_drawdown_remaining: 100.0,
            trade_allocation_remaining: 50.0,
        }
    }

    #[test]
    fn rejects_high_competition() {
        let engine = MetaDecisionEngine::new(MetaDecisionConfig::default());
        let mut opp = good();
        opp.mempool_density = 1.0;
        opp.tx_popularity = 1.0;
        opp.pool_activity_recent = 1.0;
        opp.historical_failure_rate = 1.0;
        assert!(matches!(
            engine.decide(opp),
            MetaDecision::Skip(SkipReason::HighCompetition)
        ));
    }

    #[test]
    fn accepts_high_quality_opportunity() {
        let engine = MetaDecisionEngine::new(MetaDecisionConfig::default());
        assert!(matches!(
            engine.decide(good()),
            MetaDecision::Execute { .. }
        ));
    }

    #[test]
    fn ranks_top_expected_value_without_allocating() {
        let engine = MetaDecisionEngine::new(MetaDecisionConfig::default());
        let mut a = good();
        let mut b = good();
        a.expected_profit = 15.0;
        b.expected_profit = 30.0;
        let opportunities = [a, b];
        let mut out = [None; 1];
        engine.rank_top_n(&opportunities, &mut out);
        assert_eq!(out[0].unwrap().0, 1);
    }
}
