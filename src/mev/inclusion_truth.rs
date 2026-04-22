use ethers::providers::{Http, Middleware, Provider};
use ethers::types::{Address, H256, U256, U64};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleOutcome {
    Pending,
    Included,
    NotIncluded,
    Outbid,
    Reverted,
    LateInclusion,
}

#[derive(Debug, Clone)]
pub struct PendingBundleRecord {
    pub bundle_hash: Option<H256>,
    pub tx_hash: H256,
    pub target_block: u64,
    pub submitted_at: Instant,
    pub relay: String,
    pub tip_wei: U256,
    pub expected_profit_usd: f64,
    pub competition_score: f64,
}

#[derive(Debug, Clone)]
pub struct InclusionTruth {
    pub tx_hash: H256,
    pub target_block: u64,
    pub included_block: Option<u64>,
    pub outcome: BundleOutcome,
    pub relay: String,
    pub latency_ms: u128,
    pub tip_wei: U256,
    pub expected_profit_usd: f64,
    pub competition_score: f64,
}

#[derive(Debug, Clone)]
pub struct CompetingTxSignal {
    pub pool: Address,
    pub block_number: u64,
    pub tx_hash: H256,
    pub effective_tip_wei: U256,
}

#[derive(Debug)]
pub struct InclusionTruthEngine {
    pending: HashMap<H256, PendingBundleRecord>,
    recent_truths: VecDeque<InclusionTruth>,
    capacity: usize,
    max_pending_blocks: u64,
}

impl InclusionTruthEngine {
    pub fn new(capacity: usize, max_pending_blocks: u64) -> Self {
        Self {
            pending: HashMap::with_capacity(capacity.min(1024)),
            recent_truths: VecDeque::with_capacity(capacity),
            capacity,
            max_pending_blocks,
        }
    }

    pub fn register(&mut self, record: PendingBundleRecord) {
        self.pending.insert(record.tx_hash, record);
    }

    pub fn reconcile_receipt(
        &mut self,
        tx_hash: H256,
        included_block: Option<U64>,
        success: Option<bool>,
        current_block: u64,
        competing: &[CompetingTxSignal],
    ) -> Option<InclusionTruth> {
        let record = self.pending.remove(&tx_hash)?;
        let latency_ms = record.submitted_at.elapsed().as_millis();
        let included = included_block.map(|block| block.as_u64());
        let outcome = match (included, success) {
            (Some(_block), Some(false)) => BundleOutcome::Reverted,
            (Some(block), _) if block > record.target_block => BundleOutcome::LateInclusion,
            (Some(_), _) => BundleOutcome::Included,
            (None, _) if current_block > record.target_block + self.max_pending_blocks => {
                if likely_outbid(&record, competing) {
                    BundleOutcome::Outbid
                } else {
                    BundleOutcome::NotIncluded
                }
            }
            _ => {
                self.pending.insert(tx_hash, record);
                return None;
            }
        };

        let truth = InclusionTruth {
            tx_hash,
            target_block: record.target_block,
            included_block: included,
            outcome,
            relay: record.relay,
            latency_ms,
            tip_wei: record.tip_wei,
            expected_profit_usd: record.expected_profit_usd,
            competition_score: record.competition_score,
        };
        self.push_truth(truth.clone());
        Some(truth)
    }

    pub fn expire_stale(&mut self, current_block: u64) -> Vec<InclusionTruth> {
        let stale: Vec<H256> = self
            .pending
            .iter()
            .filter_map(|(hash, record)| {
                (current_block > record.target_block + self.max_pending_blocks).then_some(*hash)
            })
            .collect();
        let mut truths = Vec::with_capacity(stale.len());
        for hash in stale {
            if let Some(record) = self.pending.remove(&hash) {
                let truth = InclusionTruth {
                    tx_hash: hash,
                    target_block: record.target_block,
                    included_block: None,
                    outcome: BundleOutcome::NotIncluded,
                    relay: record.relay,
                    latency_ms: record.submitted_at.elapsed().as_millis(),
                    tip_wei: record.tip_wei,
                    expected_profit_usd: record.expected_profit_usd,
                    competition_score: record.competition_score,
                };
                self.push_truth(truth.clone());
                truths.push(truth);
            }
        }
        truths
    }

    pub fn recent(&self) -> impl Iterator<Item = &InclusionTruth> {
        self.recent_truths.iter()
    }

    pub fn pending_hashes(&self) -> Vec<H256> {
        self.pending.keys().copied().collect()
    }

    pub async fn reconcile_receipts(
        &mut self,
        provider: Arc<Provider<Http>>,
        current_block: u64,
        competing: &[CompetingTxSignal],
    ) -> Vec<InclusionTruth> {
        let hashes: Vec<H256> = self.pending.keys().copied().collect();
        let mut outcomes = Vec::new();
        for hash in hashes {
            let receipt = provider.get_transaction_receipt(hash).await.ok().flatten();
            let included_block = receipt.as_ref().and_then(|receipt| receipt.block_number);
            let success = receipt
                .as_ref()
                .and_then(|receipt| receipt.status)
                .map(|status| status.as_u64() == 1);
            if let Some(truth) =
                self.reconcile_receipt(hash, included_block, success, current_block, competing)
            {
                outcomes.push(truth);
            }
        }
        outcomes.extend(self.expire_stale(current_block));
        outcomes
    }

    fn push_truth(&mut self, truth: InclusionTruth) {
        if self.recent_truths.len() == self.capacity {
            self.recent_truths.pop_front();
        }
        self.recent_truths.push_back(truth);
    }
}

fn likely_outbid(record: &PendingBundleRecord, competing: &[CompetingTxSignal]) -> bool {
    competing
        .iter()
        .any(|tx| tx.block_number >= record.target_block && tx.effective_tip_wei > record.tip_wei)
}

pub fn stale_by_time(record: &PendingBundleRecord, max_age: Duration) -> bool {
    record.submitted_at.elapsed() > max_age
}
