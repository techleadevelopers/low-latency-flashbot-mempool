use ethers::types::{Address, H256, U256};
use std::collections::{HashMap, VecDeque};

#[derive(Debug, Clone, Copy)]
pub struct MempoolTxFeature {
    pub tx_hash: H256,
    pub pool: Address,
    pub selector: [u8; 4],
    pub value_wei: U256,
    pub max_fee_per_gas: U256,
    pub observed_ms: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct PoolActivity {
    pub pool: Address,
    pub swaps_last_block: u32,
    pub swaps_last_minute: u32,
    pub avg_notional_eth: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct CompetitionSnapshot {
    pub mempool_density: f64,
    pub similar_tx_count: u32,
    pub pool_activity_spike: f64,
    pub historical_outbid_rate: f64,
    pub competition_probability: f64,
}

#[derive(Debug)]
pub struct CompetitionIntelligence {
    recent_txs: VecDeque<MempoolTxFeature>,
    pool_outbid_rate: HashMap<Address, f64>,
    pool_activity: HashMap<Address, PoolActivity>,
    capacity: usize,
}

impl CompetitionIntelligence {
    pub fn new(capacity: usize) -> Self {
        Self {
            recent_txs: VecDeque::with_capacity(capacity),
            pool_outbid_rate: HashMap::new(),
            pool_activity: HashMap::new(),
            capacity,
        }
    }

    pub fn observe_tx(&mut self, tx: MempoolTxFeature) {
        if self.recent_txs.len() == self.capacity {
            self.recent_txs.pop_front();
        }
        self.recent_txs.push_back(tx);
    }

    pub fn update_pool_activity(&mut self, activity: PoolActivity) {
        self.pool_activity.insert(activity.pool, activity);
    }

    pub fn record_outbid(&mut self, pool: Address, outbid: bool) {
        let current = self.pool_outbid_rate.get(&pool).copied().unwrap_or(0.20);
        let target = if outbid { 1.0 } else { 0.0 };
        self.pool_outbid_rate
            .insert(pool, current * 0.90 + target * 0.10);
    }

    pub fn snapshot(&self, pool: Address, selector: [u8; 4]) -> CompetitionSnapshot {
        let mut similar = 0u32;
        let mut density = 0u32;
        for tx in &self.recent_txs {
            density = density.saturating_add(1);
            if tx.pool == pool || tx.selector == selector {
                similar = similar.saturating_add(1);
            }
        }
        let mempool_density = (density as f64 / self.capacity.max(1) as f64).clamp(0.0, 1.0);
        let similar_factor = (similar as f64 / 12.0).clamp(0.0, 1.0);
        let activity = self.pool_activity.get(&pool).copied().unwrap_or(PoolActivity {
            pool,
            swaps_last_block: 0,
            swaps_last_minute: 0,
            avg_notional_eth: 0.0,
        });
        let pool_activity_spike = (activity.swaps_last_block as f64 / 4.0
            + activity.swaps_last_minute as f64 / 80.0)
            .clamp(0.0, 1.0);
        let historical_outbid_rate = self.pool_outbid_rate.get(&pool).copied().unwrap_or(0.20);
        let competition_probability = (mempool_density * 0.20
            + similar_factor * 0.30
            + pool_activity_spike * 0.25
            + historical_outbid_rate * 0.25)
            .clamp(0.0, 1.0);

        CompetitionSnapshot {
            mempool_density,
            similar_tx_count: similar,
            pool_activity_spike,
            historical_outbid_rate,
            competition_probability,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn similar_transactions_raise_competition_probability() {
        let mut intel = CompetitionIntelligence::new(32);
        let pool = Address::from_low_u64_be(1);
        for idx in 0..8 {
            intel.observe_tx(MempoolTxFeature {
                tx_hash: H256::from_low_u64_be(idx),
                pool,
                selector: [1, 2, 3, 4],
                value_wei: U256::from(1),
                max_fee_per_gas: U256::from(1),
                observed_ms: idx,
            });
        }
        assert!(intel.snapshot(pool, [1, 2, 3, 4]).competition_probability > 0.35);
    }
}
