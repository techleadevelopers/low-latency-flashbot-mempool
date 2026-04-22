use crate::config::Config;
use crate::dashboard::DashboardHandle;
use crate::mev::competition::{
    extract_block_signals, CompetitionForecast, CompetitionIntelligence, PoolActivity, PressureMap,
};
use crate::mev::feedback::FeedbackEngine;
use crate::mev::inclusion::{InclusionEngine, InclusionFeedback};
use crate::mev::inclusion_truth::{
    BundleOutcome, CompetingTxSignal, InclusionTruthEngine, PendingBundleRecord,
};
use crate::mev::post_block::PostBlockAnalyzer;
use crate::mev::tip_discovery::{OpportunityClass, TipDiscoveryEngine, TipOutcome};
use crate::rpc::RpcFleet;
use ethers::providers::Middleware;
use ethers::types::{Address, Transaction, H256, U256};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::MissedTickBehavior;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizedInclusion {
    Included,
    Outbid,
    Dropped,
    Reverted,
    Late,
}

#[derive(Clone)]
pub struct LearningRuntime {
    truth: Arc<Mutex<InclusionTruthEngine>>,
    competition: Arc<Mutex<CompetitionIntelligence>>,
    pressure: Arc<Mutex<PressureMap>>,
    tips: Arc<Mutex<TipDiscoveryEngine>>,
    post_block: Arc<Mutex<PostBlockAnalyzer>>,
    feedback: Arc<Mutex<FeedbackEngine>>,
    inclusion: Arc<Mutex<InclusionEngine>>,
    survival: Arc<Mutex<SurvivalState>>,
}

#[derive(Debug, Clone, Copy)]
pub struct SurvivalState {
    pub enabled: bool,
    pub degradation_ewma: f64,
    pub last_block: u64,
}

impl LearningRuntime {
    pub fn new(config: &Config) -> Self {
        Self {
            truth: Arc::new(Mutex::new(InclusionTruthEngine::new(512, 2))),
            competition: Arc::new(Mutex::new(CompetitionIntelligence::new(512))),
            pressure: Arc::new(Mutex::new(PressureMap::new(512))),
            tips: Arc::new(Mutex::new(TipDiscoveryEngine::new(512))),
            post_block: Arc::new(Mutex::new(PostBlockAnalyzer::new(512))),
            feedback: Arc::new(Mutex::new(FeedbackEngine::new(512))),
            inclusion: Arc::new(Mutex::new(InclusionEngine::from_config(config))),
            survival: Arc::new(Mutex::new(SurvivalState {
                enabled: false,
                degradation_ewma: 0.0,
                last_block: 0,
            })),
        }
    }

    pub fn register_execution(&self, event: ExecutionEvent) {
        if let Ok(mut truth) = self.truth.lock() {
            truth.register(PendingBundleRecord {
                bundle_hash: event.bundle_hash,
                tx_hash: event.tx_hash,
                target_block: event.target_block,
                submitted_at: event.submitted_at,
                relay: event.relay,
                tip_wei: event.tip_wei,
                expected_profit_usd: event.expected_profit_usd,
                competition_score: event.competition_score,
            });
        }
    }

    pub fn survival_mode(&self) -> bool {
        self.survival
            .lock()
            .map(|state| state.enabled)
            .unwrap_or(true)
    }

    pub fn feedback(&self) -> Arc<Mutex<FeedbackEngine>> {
        self.feedback.clone()
    }

    pub fn inclusion(&self) -> Arc<Mutex<InclusionEngine>> {
        self.inclusion.clone()
    }

    pub fn pressure(&self) -> Arc<Mutex<PressureMap>> {
        self.pressure.clone()
    }

    pub fn observe_pending_transaction(&self, tx: &Transaction) {
        let observed_ms = unix_ms();
        let intent =
            self.competition.lock().ok().and_then(|mut competition| {
                competition.observe_pending_transaction(tx, observed_ms)
            });
        if let Some(intent) = intent {
            let forecast = self.competition_forecast(
                intent.key.router,
                intent.key.token_in,
                intent.key.token_out,
                intent.key.selector,
            );
            if let Ok(mut pressure) = self.pressure.lock() {
                pressure.record_forecast(intent.key.router, forecast);
            }
        }
    }

    pub fn competition_forecast(
        &self,
        pool: Address,
        token_in: Address,
        token_out: Address,
        selector: [u8; 4],
    ) -> CompetitionForecast {
        self.competition
            .lock()
            .map(|competition| competition.forecast(pool, token_in, token_out, selector))
            .unwrap_or(CompetitionForecast {
                block_probability: 0.20,
                mempool_density: 0.0,
                similar_pending: 0,
                pressure_probability: 0.20,
                likely_outbid: false,
                tip_multiplier: 1.0,
                max_pending_tip_wei: U256::zero(),
            })
    }
}

#[derive(Debug, Clone)]
pub struct ExecutionEvent {
    pub bundle_hash: Option<H256>,
    pub tx_hash: H256,
    pub target_block: u64,
    pub submitted_at: std::time::Instant,
    pub relay: String,
    pub tip_wei: U256,
    pub expected_profit_usd: f64,
    pub competition_score: f64,
}

pub async fn run_block_loop(
    runtime: LearningRuntime,
    config: Arc<Config>,
    rpc_fleet: Arc<RpcFleet>,
    dashboard: DashboardHandle,
) {
    let mut last_block = 0u64;
    let mut interval = tokio::time::interval(Duration::from_millis(250));
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        interval.tick().await;
        let endpoint = rpc_fleet.read_endpoint();
        let Ok(block) = endpoint.provider.get_block_number().await else {
            continue;
        };
        let block_number = block.as_u64();
        if block_number <= last_block {
            continue;
        }
        last_block = block_number;
        process_block(
            &runtime,
            &config,
            endpoint.provider.clone(),
            block_number,
            &dashboard,
        )
        .await;
    }
}

async fn process_block(
    runtime: &LearningRuntime,
    config: &Config,
    provider: Arc<ethers::providers::Provider<ethers::providers::Http>>,
    block_number: u64,
    dashboard: &DashboardHandle,
) {
    let competing = collect_competing_signals(runtime, provider.clone(), block_number).await;
    let pending_hashes = runtime
        .truth
        .lock()
        .map(|truth| truth.pending_hashes())
        .unwrap_or_default();
    let mut receipt_results = Vec::with_capacity(pending_hashes.len());
    for hash in pending_hashes {
        let receipt = provider.get_transaction_receipt(hash).await.ok().flatten();
        let included_block = receipt.as_ref().and_then(|receipt| receipt.block_number);
        let success = receipt
            .as_ref()
            .and_then(|receipt| receipt.status)
            .map(|status| status.as_u64() == 1);
        receipt_results.push((hash, included_block, success));
    }
    let truths = {
        let mut truth = match runtime.truth.lock() {
            Ok(lock) => lock,
            Err(_) => return,
        };
        let mut truths = Vec::new();
        for (hash, included_block, success) in receipt_results {
            if let Some(item) =
                truth.reconcile_receipt(hash, included_block, success, block_number, &competing)
            {
                truths.push(item);
            }
        }
        truths.extend(truth.expire_stale(block_number));
        truths
    };

    for truth in truths {
        let finalized = classify(&truth.outcome);
        update_competition(runtime, &truth, finalized);
        update_tip_discovery(runtime, &truth, finalized);
        update_post_block(runtime, &truth);
        update_feedback(runtime, &truth);
        update_inclusion(runtime, &truth, finalized);
        update_survival(runtime, finalized, block_number);
        dashboard.event(
            "info",
            format!(
                "inclusion finalized tx={:?} outcome={:?} relay={} target={} included={:?}",
                truth.tx_hash, finalized, truth.relay, truth.target_block, truth.included_block
            ),
        );
    }

    if runtime.survival_mode() {
        dashboard.event(
            "warn",
            format!(
                "survival mode active: execution should be conservative on {}",
                config.network
            ),
        );
    }
}

async fn collect_competing_signals(
    runtime: &LearningRuntime,
    provider: Arc<ethers::providers::Provider<ethers::providers::Http>>,
    block_number: u64,
) -> Vec<CompetingTxSignal> {
    let block = provider
        .get_block_with_txs(ethers::types::BlockId::Number(block_number.into()))
        .await
        .ok()
        .flatten();
    let Some(block) = block else {
        return Vec::new();
    };
    let signals = extract_block_signals(block_number, &block.transactions, 256);
    if let Ok(mut pressure) = runtime.pressure.lock() {
        pressure.record_block(&signals);
    }
    signals.into_iter().map(Into::into).collect()
}

fn classify(outcome: &BundleOutcome) -> FinalizedInclusion {
    match outcome {
        BundleOutcome::Included => FinalizedInclusion::Included,
        BundleOutcome::Outbid => FinalizedInclusion::Outbid,
        BundleOutcome::Reverted => FinalizedInclusion::Reverted,
        BundleOutcome::LateInclusion => FinalizedInclusion::Late,
        BundleOutcome::NotIncluded | BundleOutcome::Pending => FinalizedInclusion::Dropped,
    }
}

fn update_competition(
    runtime: &LearningRuntime,
    truth: &crate::mev::inclusion_truth::InclusionTruth,
    finalized: FinalizedInclusion,
) {
    if let Ok(mut competition) = runtime.competition.lock() {
        let outbid = matches!(finalized, FinalizedInclusion::Outbid);
        competition.record_outbid(Address::zero(), outbid);
        competition.update_pool_activity(PoolActivity {
            pool: Address::zero(),
            swaps_last_block: 1,
            swaps_last_minute: 1,
            avg_notional_eth: truth.expected_profit_usd.max(0.0),
        });
    }
}

fn update_tip_discovery(
    runtime: &LearningRuntime,
    truth: &crate::mev::inclusion_truth::InclusionTruth,
    finalized: FinalizedInclusion,
) {
    if let Ok(mut tips) = runtime.tips.lock() {
        let included = matches!(
            finalized,
            FinalizedInclusion::Included | FinalizedInclusion::Late
        );
        let tip_bps = ((truth.tip_wei.to_string().parse::<f64>().unwrap_or(0.0) / 1e18)
            / truth.expected_profit_usd.max(1.0)
            * 10_000.0) as u64;
        tips.record(TipOutcome {
            class: OpportunityClass::BackrunV2,
            tip_bps,
            included,
            profit_usd: if included {
                truth.expected_profit_usd
            } else {
                0.0
            },
            competition_score: truth.competition_score,
        });
    }
}

fn update_post_block(
    runtime: &LearningRuntime,
    truth: &crate::mev::inclusion_truth::InclusionTruth,
) {
    if let Ok(mut post) = runtime.post_block.lock() {
        let _ = post.analyze_truth(truth);
    }
}

fn update_feedback(runtime: &LearningRuntime, truth: &crate::mev::inclusion_truth::InclusionTruth) {
    if let Ok(mut feedback) = runtime.feedback.lock() {
        let realized = if matches!(truth.outcome, BundleOutcome::Included) {
            truth.expected_profit_usd
        } else {
            0.0
        };
        feedback.record_inclusion_truth(truth, realized);
    }
}

fn update_inclusion(
    runtime: &LearningRuntime,
    truth: &crate::mev::inclusion_truth::InclusionTruth,
    finalized: FinalizedInclusion,
) {
    if let Ok(mut inclusion) = runtime.inclusion.lock() {
        match finalized {
            FinalizedInclusion::Included | FinalizedInclusion::Late => {
                inclusion.record_success_with_tip(
                    0,
                    Duration::from_millis(truth.latency_ms as u64),
                    0.0,
                );
            }
            FinalizedInclusion::Outbid => inclusion.record_failure(
                0,
                crate::mev::inclusion::InclusionFailureReason::Outbid,
                InclusionFeedback {
                    reason: crate::mev::inclusion::InclusionFailureReason::Outbid,
                    tip_usd: 0.0,
                    competition_score: truth.competition_score,
                },
            ),
            FinalizedInclusion::Reverted => inclusion.record_failure(
                0,
                crate::mev::inclusion::InclusionFailureReason::Revert,
                InclusionFeedback {
                    reason: crate::mev::inclusion::InclusionFailureReason::Revert,
                    tip_usd: 0.0,
                    competition_score: truth.competition_score,
                },
            ),
            FinalizedInclusion::Dropped => inclusion.record_failure(
                0,
                crate::mev::inclusion::InclusionFailureReason::NotIncluded,
                InclusionFeedback {
                    reason: crate::mev::inclusion::InclusionFailureReason::NotIncluded,
                    tip_usd: 0.0,
                    competition_score: truth.competition_score,
                },
            ),
        }
    }
}

fn update_survival(runtime: &LearningRuntime, finalized: FinalizedInclusion, block_number: u64) {
    if let Ok(mut survival) = runtime.survival.lock() {
        let bad = if matches!(
            finalized,
            FinalizedInclusion::Outbid | FinalizedInclusion::Dropped | FinalizedInclusion::Reverted
        ) {
            1.0
        } else {
            0.0
        };
        survival.degradation_ewma = survival.degradation_ewma * 0.85 + bad * 0.15;
        survival.enabled = survival.degradation_ewma > 0.55;
        survival.last_block = block_number;
    }
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}
