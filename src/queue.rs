use crate::config::Config;
use ethers::prelude::*;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperationType {
    Install,
    Exec,
}

impl OperationType {
    pub fn as_str(self) -> &'static str {
        match self {
            OperationType::Install => "install",
            OperationType::Exec => "exec",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResidualCandidate {
    pub wallet: Address,
    pub operation: OperationType,
    pub requires_approve: bool,
    pub approval_tokens: Vec<Address>,
    pub native_balance: U256,
    pub token_value_wei: U256,
    pub stable_token_value_wei: U256,
    pub other_token_value_wei: U256,
    pub total_residual_wei: U256,
    pub estimated_net_profit_wei: U256,
    pub estimated_cost_wei: U256,
    pub asset_class: String,
    pub rpc: String,
    pub timestamp: Instant,
    // token_address REMOVIDO - não é usado pelo contrato Simple7702Delegate
}

impl ResidualCandidate {
    pub fn total_value_eth_f64(&self) -> f64 {
        wei_to_eth_f64(self.total_residual_wei)
    }

    pub fn net_profit_eth_f64(&self) -> f64 {
        wei_to_eth_f64(self.estimated_net_profit_wei)
    }

    pub fn roi_bps(&self) -> u64 {
        if self.estimated_cost_wei.is_zero() {
            return 0;
        }

        let profit = self.estimated_net_profit_wei.as_u128() as f64;
        let cost = self.estimated_cost_wei.as_u128() as f64;
        ((profit / cost) * 10_000.0) as u64
    }
}

pub fn estimated_bundle_gas_units(config: &Config, candidate: &ResidualCandidate) -> u64 {
    let approve_gas = config
        .estimated_approve_gas
        .saturating_mul(candidate.approval_tokens.len() as u64);
    match candidate.operation {
        OperationType::Install => config
            .estimated_install_gas
            .saturating_add(approve_gas)
            .saturating_add(config.estimated_exec_gas)
            .saturating_add(config.estimated_bundle_overhead_gas),
        OperationType::Exec => approve_gas
            .saturating_add(config.estimated_exec_gas)
            .saturating_add(config.estimated_bundle_overhead_gas),
    }
}

pub fn estimated_total_cost_wei(
    config: &Config,
    gas_price: U256,
    candidate: &ResidualCandidate,
) -> U256 {
    gas_price.saturating_mul(U256::from(estimated_bundle_gas_units(config, candidate)))
}

#[derive(Debug, Clone)]
pub struct PrioritizedJob {
    pub candidate: ResidualCandidate,
    pub rpc: String,
    pub enqueued_at: Instant,
    sequence: u64,
}

impl PartialEq for PrioritizedJob {
    fn eq(&self, other: &Self) -> bool {
        self.candidate.wallet == other.candidate.wallet
            && self.candidate.operation == other.candidate.operation
            && self.sequence == other.sequence
            && self.rpc == other.rpc
    }
}

impl Eq for PrioritizedJob {}

impl PartialOrd for PrioritizedJob {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PrioritizedJob {
    fn cmp(&self, other: &Self) -> Ordering {
        self.candidate
            .estimated_net_profit_wei
            .cmp(&other.candidate.estimated_net_profit_wei)
            .then_with(|| self.candidate.roi_bps().cmp(&other.candidate.roi_bps()))
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}

pub type Job = PrioritizedJob;

pub struct SweepQueue {
    heap: BinaryHeap<PrioritizedJob>,
    active_wallets: HashSet<Address>,
    last_enqueued_at: HashMap<(Address, OperationType), Instant>,
    dedupe_window: Duration,
    sequence: u64,
}

impl SweepQueue {
    pub fn new(dedupe_window: Duration) -> Self {
        Self {
            heap: BinaryHeap::new(),
            active_wallets: HashSet::new(),
            last_enqueued_at: HashMap::new(),
            dedupe_window,
            sequence: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.heap.len()
    }

    pub fn enqueue_prioritized(&mut self, candidate: ResidualCandidate, rpc: String) -> bool {
        if self.active_wallets.contains(&candidate.wallet) {
            return false;
        }

        let now = Instant::now();
        let dedupe_key = (candidate.wallet, candidate.operation);
        if let Some(last_enqueued_at) = self.last_enqueued_at.get(&dedupe_key) {
            if now.saturating_duration_since(*last_enqueued_at) < self.dedupe_window {
                return false;
            }
        }

        self.sequence = self.sequence.saturating_add(1);
        self.last_enqueued_at.insert(dedupe_key, now);
        self.heap.push(PrioritizedJob {
            candidate,
            rpc,
            enqueued_at: now,
            sequence: self.sequence,
        });
        true
    }

    pub fn pop(&mut self) -> Option<Job> {
        let job = self.heap.pop()?;
        self.active_wallets.insert(job.candidate.wallet);
        Some(job)
    }

    pub fn finish(&mut self, wallet: Address) {
        self.active_wallets.remove(&wallet);
    }

    pub fn peek_top_profit(&self) -> Option<U256> {
        self.heap
            .peek()
            .map(|job| job.candidate.estimated_net_profit_wei)
    }
}

fn wei_to_eth_f64(wei: U256) -> f64 {
    wei.to_string().parse::<f64>().unwrap_or(0.0) / 1e18
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn candidate(wallet_idx: u64, profit_wei: u64, cost_wei: u64) -> ResidualCandidate {
        ResidualCandidate {
            wallet: Address::from_low_u64_be(wallet_idx),
            operation: OperationType::Exec,
            requires_approve: false,
            approval_tokens: Vec::new(),
            native_balance: U256::from(profit_wei + cost_wei),
            token_value_wei: U256::zero(),
            stable_token_value_wei: U256::zero(),
            other_token_value_wei: U256::zero(),
            total_residual_wei: U256::from(profit_wei + cost_wei),
            estimated_net_profit_wei: U256::from(profit_wei),
            estimated_cost_wei: U256::from(cost_wei),
            asset_class: "native".to_string(),
            rpc: "test-rpc".to_string(),
            timestamp: Instant::now(),
            // token_address removido
        }
    }

    #[test]
    fn prioritizes_higher_profit_then_higher_roi() {
        let mut queue = SweepQueue::new(Duration::from_millis(0));
        let lower_profit_higher_roi = candidate(1, 150, 50);
        let higher_profit_lower_roi = candidate(2, 200, 100);
        let same_profit_better_roi = candidate(3, 200, 50);

        assert!(queue.enqueue_prioritized(
            lower_profit_higher_roi.clone(),
            lower_profit_higher_roi.rpc.clone()
        ));
        assert!(queue.enqueue_prioritized(
            higher_profit_lower_roi.clone(),
            higher_profit_lower_roi.rpc.clone()
        ));
        assert!(queue.enqueue_prioritized(
            same_profit_better_roi.clone(),
            same_profit_better_roi.rpc.clone()
        ));

        let first = queue.pop().expect("first job");
        let second = queue.pop().expect("second job");
        let third = queue.pop().expect("third job");

        assert_eq!(first.candidate.wallet, same_profit_better_roi.wallet);
        assert_eq!(second.candidate.wallet, higher_profit_lower_roi.wallet);
        assert_eq!(third.candidate.wallet, lower_profit_higher_roi.wallet);
    }

    #[test]
    fn dedupes_same_wallet_inside_window() {
        let mut queue = SweepQueue::new(Duration::from_secs(30));
        let candidate = candidate(1, 200, 100);

        assert!(queue.enqueue_prioritized(candidate.clone(), candidate.rpc.clone()));
        assert!(!queue.enqueue_prioritized(candidate.clone(), candidate.rpc.clone()));
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn blocks_active_wallet_until_finish() {
        let mut queue = SweepQueue::new(Duration::from_millis(0));
        let candidate = candidate(1, 200, 100);

        assert!(queue.enqueue_prioritized(candidate.clone(), candidate.rpc.clone()));
        let popped = queue.pop().expect("job");
        assert_eq!(popped.candidate.wallet, candidate.wallet);
        assert!(!queue.enqueue_prioritized(candidate.clone(), candidate.rpc.clone()));

        queue.finish(candidate.wallet);
        assert!(queue.enqueue_prioritized(candidate.clone(), candidate.rpc.clone()));
    }

    #[test]
    fn handles_burst_for_85_wallets_without_losing_order() {
        let mut queue = SweepQueue::new(Duration::from_millis(0));
        let total_wallets = 85u64;

        for wallet_idx in 1..=total_wallets {
            let candidate = candidate(wallet_idx, 1_000 + wallet_idx, 100);
            assert!(queue.enqueue_prioritized(candidate.clone(), candidate.rpc.clone()));
        }

        assert_eq!(queue.len(), total_wallets as usize);

        let mut last_profit = u64::MAX;
        let mut seen = HashSet::new();
        while let Some(job) = queue.pop() {
            let profit = job.candidate.estimated_net_profit_wei.as_u64();
            assert!(profit <= last_profit);
            assert!(seen.insert(job.candidate.wallet));
            last_profit = profit;
        }

        assert_eq!(seen.len(), total_wallets as usize);
    }

    #[test]
    #[ignore = "manual local stress test for burst behavior"]
    fn stress_burst_85_wallets_reports_local_queue_time() {
        let rounds = 1_000u64;
        let wallets_per_round = 85u64;
        let started = Instant::now();

        for round in 0..rounds {
            let mut queue = SweepQueue::new(Duration::from_millis(0));
            for wallet_idx in 1..=wallets_per_round {
                let candidate = candidate(
                    round * wallets_per_round + wallet_idx,
                    10_000 + wallet_idx,
                    100,
                );
                assert!(queue.enqueue_prioritized(candidate.clone(), candidate.rpc.clone()));
            }

            let mut popped = 0u64;
            while let Some(job) = queue.pop() {
                popped += 1;
                queue.finish(job.candidate.wallet);
            }
            assert_eq!(popped, wallets_per_round);
        }

        let elapsed = started.elapsed();
        eprintln!(
            "stress_burst_85_wallets_reports_local_queue_time rounds={} wallets_per_round={} elapsed_ms={}",
            rounds,
            wallets_per_round,
            elapsed.as_millis()
        );
    }

    #[test]
    #[ignore = "manual local benchmark for queue throughput"]
    fn benchmark_queue_throughput_for_large_bursts() {
        let rounds = env::var("QUEUE_BENCH_ROUNDS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(2_000);
        let wallets_per_round = env::var("QUEUE_BENCH_WALLETS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(250);
        let max_elapsed_ms = env::var("QUEUE_BENCH_MAX_MS")
            .ok()
            .and_then(|value| value.parse::<u128>().ok())
            .unwrap_or(12_000);

        let started = Instant::now();
        let mut total_jobs = 0u64;

        for round in 0..rounds {
            let mut queue = SweepQueue::new(Duration::from_millis(0));

            for wallet_idx in 1..=wallets_per_round {
                let unique_wallet = round * wallets_per_round + wallet_idx;
                let candidate = candidate(
                    unique_wallet,
                    100_000 + (wallet_idx % 10_000),
                    100 + (wallet_idx % 50),
                );
                assert!(queue.enqueue_prioritized(candidate.clone(), candidate.rpc.clone()));
                total_jobs += 1;
            }

            let mut popped = 0u64;
            while let Some(job) = queue.pop() {
                popped += 1;
                queue.finish(job.candidate.wallet);
            }

            assert_eq!(popped, wallets_per_round);
        }

        let elapsed = started.elapsed();
        let elapsed_ms = elapsed.as_millis();
        let jobs_per_sec = if elapsed.as_secs_f64() > 0.0 {
            total_jobs as f64 / elapsed.as_secs_f64()
        } else {
            0.0
        };

        eprintln!(
            "queue_benchmark rounds={} wallets_per_round={} total_jobs={} elapsed_ms={} jobs_per_sec={:.2}",
            rounds,
            wallets_per_round,
            total_jobs,
            elapsed_ms,
            jobs_per_sec
        );

        assert!(
            elapsed_ms <= max_elapsed_ms,
            "queue benchmark too slow: {} ms > {} ms (jobs_per_sec={:.2})",
            elapsed_ms,
            max_elapsed_ms,
            jobs_per_sec
        );
    }
}
