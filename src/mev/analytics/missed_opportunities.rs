use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MissReason {
    LowProfit,
    HighCompetition,
    SimulationFailed,
    CapitalLimit,
    LatencyExceeded,
    PayloadUnavailable,
    PoolUnavailable,
}

impl MissReason {
    pub fn as_str(self) -> &'static str {
        match self {
            MissReason::LowProfit => "low_profit",
            MissReason::HighCompetition => "high_competition",
            MissReason::SimulationFailed => "simulation_failed",
            MissReason::CapitalLimit => "capital_limit",
            MissReason::LatencyExceeded => "latency_exceeded",
            MissReason::PayloadUnavailable => "payload_unavailable",
            MissReason::PoolUnavailable => "pool_unavailable",
        }
    }
}

#[derive(Debug, Default)]
pub struct MissedOpportunityTracker {
    counts: HashMap<MissReason, u64>,
}

impl MissedOpportunityTracker {
    pub fn record(&mut self, reason: MissReason) {
        *self.counts.entry(reason).or_default() += 1;
    }

    pub fn count(&self, reason: MissReason) -> u64 {
        self.counts.get(&reason).copied().unwrap_or(0)
    }
}
