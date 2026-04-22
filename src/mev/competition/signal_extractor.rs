use crate::mev::competition::mempool_intel::{
    MempoolCompetitionForecast, MempoolIntel, PendingSwapIntent,
};
use crate::mev::inclusion_truth::CompetingTxSignal;
use ethers::types::{Address, Transaction, H256, U256};
use std::collections::{HashMap, VecDeque};

const SWAP_EXACT_TOKENS_FOR_TOKENS: [u8; 4] = [0x38, 0xed, 0x17, 0x39];
const SWAP_EXACT_ETH_FOR_TOKENS: [u8; 4] = [0x7f, 0xf3, 0x6a, 0xb5];
const SWAP_EXACT_TOKENS_FOR_ETH: [u8; 4] = [0x18, 0xcb, 0xaf, 0xe5];
const SWAP_EXACT_TOKENS_FOR_TOKENS_SUPPORTING_FEE: [u8; 4] = [0x5c, 0x11, 0xd7, 0x95];
const SWAP_EXACT_ETH_FOR_TOKENS_SUPPORTING_FEE: [u8; 4] = [0xb6, 0xf9, 0xde, 0x95];
const SWAP_EXACT_TOKENS_FOR_ETH_SUPPORTING_FEE: [u8; 4] = [0x79, 0x1a, 0xc9, 0x47];

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
pub struct CompetingSwapSignal {
    pub pool: Address,
    pub tx_hash: H256,
    pub actor: Address,
    pub direction: [u8; 4],
    pub effective_tip_wei: U256,
    pub block_number: u64,
    pub aggressiveness: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct CompetitionSnapshot {
    pub mempool_density: f64,
    pub similar_tx_count: u32,
    pub pool_activity_spike: f64,
    pub historical_outbid_rate: f64,
    pub competition_probability: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct CompetitionForecast {
    pub block_probability: f64,
    pub mempool_density: f64,
    pub similar_pending: u32,
    pub pressure_probability: f64,
    pub likely_outbid: bool,
    pub tip_multiplier: f64,
    pub max_pending_tip_wei: U256,
}

#[derive(Debug)]
pub struct CompetitionIntelligence {
    recent_txs: VecDeque<MempoolTxFeature>,
    mempool: MempoolIntel,
    pool_outbid_rate: HashMap<Address, f64>,
    pool_activity: HashMap<Address, PoolActivity>,
    capacity: usize,
}

pub struct CompetitionModel {
    inner: std::sync::Mutex<CompetitionIntelligence>,
}

impl CompetitionModel {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: std::sync::Mutex::new(CompetitionIntelligence::new(capacity)),
        }
    }

    pub fn observe_tx(&self, tx: MempoolTxFeature) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.observe_tx(tx);
        }
    }

    pub fn snapshot(&self, pool: Address, selector: [u8; 4]) -> CompetitionSnapshot {
        self.inner
            .lock()
            .map(|inner| inner.snapshot(pool, selector))
            .unwrap_or(CompetitionSnapshot {
                mempool_density: 0.0,
                similar_tx_count: 0,
                pool_activity_spike: 0.0,
                historical_outbid_rate: 0.20,
                competition_probability: 0.20,
            })
    }
}

impl CompetitionIntelligence {
    pub fn new(capacity: usize) -> Self {
        Self {
            recent_txs: VecDeque::with_capacity(capacity),
            mempool: MempoolIntel::new(capacity),
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

    pub fn observe_pending_transaction(
        &mut self,
        tx: &Transaction,
        observed_ms: u64,
    ) -> Option<PendingSwapIntent> {
        self.mempool.observe_transaction(tx, observed_ms)
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
        let activity = self
            .pool_activity
            .get(&pool)
            .copied()
            .unwrap_or(PoolActivity {
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

    pub fn forecast(
        &self,
        pool: Address,
        token_in: Address,
        token_out: Address,
        selector: [u8; 4],
    ) -> CompetitionForecast {
        let block = self.snapshot(pool, selector);
        let mempool = self.mempool.forecast(pool, token_in, token_out, selector);
        merge_forecast(block, mempool)
    }
}

fn merge_forecast(
    block: CompetitionSnapshot,
    mempool: MempoolCompetitionForecast,
) -> CompetitionForecast {
    let pressure_probability = (block.competition_probability * 0.42
        + mempool.density * 0.18
        + (mempool.similar_intents as f64 / 8.0).clamp(0.0, 1.0) * 0.24
        + mempool.avg_aggressiveness * 0.16)
        .clamp(0.0, 1.0);
    CompetitionForecast {
        block_probability: block.competition_probability,
        mempool_density: mempool.density,
        similar_pending: mempool.similar_intents,
        pressure_probability,
        likely_outbid: mempool.likely_outbid || pressure_probability >= 0.72,
        tip_multiplier: (1.0 + pressure_probability * 0.85)
            .max(mempool.tip_multiplier)
            .clamp(1.0, 3.0),
        max_pending_tip_wei: mempool.max_tip_wei,
    }
}

pub fn extract_block_signals(
    block_number: u64,
    transactions: &[Transaction],
    max_signals: usize,
) -> Vec<CompetingSwapSignal> {
    let mut out = Vec::with_capacity(max_signals.min(transactions.len()));
    for tx in transactions {
        if out.len() >= max_signals {
            break;
        }
        let Some(selector) = selector(tx) else {
            continue;
        };
        if !is_swap_selector(selector) {
            continue;
        }
        let Some(pool) = tx.to else {
            continue;
        };
        let effective_tip = tx
            .max_priority_fee_per_gas
            .or(tx.gas_price)
            .unwrap_or_default();
        out.push(CompetingSwapSignal {
            pool,
            tx_hash: tx.hash,
            actor: tx.from,
            direction: selector,
            effective_tip_wei: effective_tip,
            block_number,
            aggressiveness: aggressiveness(effective_tip, tx.value),
        });
    }
    out
}

impl From<CompetingSwapSignal> for CompetingTxSignal {
    fn from(value: CompetingSwapSignal) -> Self {
        Self {
            pool: value.pool,
            block_number: value.block_number,
            tx_hash: value.tx_hash,
            effective_tip_wei: value.effective_tip_wei,
        }
    }
}

fn selector(tx: &Transaction) -> Option<[u8; 4]> {
    let input = tx.input.as_ref();
    (input.len() >= 4).then(|| [input[0], input[1], input[2], input[3]])
}

fn is_swap_selector(selector: [u8; 4]) -> bool {
    matches!(
        selector,
        SWAP_EXACT_TOKENS_FOR_TOKENS
            | SWAP_EXACT_ETH_FOR_TOKENS
            | SWAP_EXACT_TOKENS_FOR_ETH
            | SWAP_EXACT_TOKENS_FOR_TOKENS_SUPPORTING_FEE
            | SWAP_EXACT_ETH_FOR_TOKENS_SUPPORTING_FEE
            | SWAP_EXACT_TOKENS_FOR_ETH_SUPPORTING_FEE
    )
}

fn aggressiveness(tip: U256, value: U256) -> f64 {
    let tip_gwei = tip.to_string().parse::<f64>().unwrap_or(0.0) / 1e9;
    let value_eth = value.to_string().parse::<f64>().unwrap_or(0.0) / 1e18;
    ((tip_gwei / 25.0) + (value_eth / 250.0)).clamp(0.0, 1.0)
}
