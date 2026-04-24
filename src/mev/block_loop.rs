use crate::config::{AssetClass, Config, MonitoredTokenConfig};
use crate::dashboard::DashboardHandle;
use crate::mev::competition::{
    extract_block_signals, CompetitionForecast, CompetitionIntelligence, PoolActivity, PressureMap,
};
use crate::mev::execution::finalizer::ExecutionFinalizer;
use crate::mev::execution::nonce_manager::{NonceManager, NonceReservation};
use crate::mev::execution::tx_lifecycle::TxLifecycleManager;
use crate::mev::feedback::survival_feedback::SurvivalFeedbackEngine;
use crate::mev::feedback::FeedbackEngine;
use crate::mev::inclusion::{InclusionEngine, InclusionFeedback};
use crate::mev::inclusion_truth::{
    BundleOutcome, CompetingTxSignal, InclusionTruthEngine, PendingBundleRecord,
};
use crate::mev::market_truth::competition_reality::{CompetitionRealityInput, RouteCluster};
use crate::mev::market_truth::edge_survival::EdgeSurvivalInput;
use crate::mev::market_truth::market_snapshot_engine::{
    extract_execution_price, ratio_to_f64, MarketSnapshotCollector, PoolMetadata,
};
use crate::mev::market_truth::truth_pipeline::{DataQuality, MarketTruthInput, TruthPipeline};
use crate::mev::post_block::PostBlockAnalyzer;
use crate::mev::state::event_store::StateEvent;
use crate::mev::survival::survival_gate::SurvivalGateConfig;
use crate::mev::tip_discovery::{OpportunityClass, TipDiscoveryEngine, TipOutcome};
use crate::rpc::RpcFleet;
use ethers::contract::abigen;
use ethers::providers::Middleware;
use ethers::types::{Address, BlockId, BlockNumber, Transaction, TransactionReceipt, H256, U256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::MissedTickBehavior;

abigen!(
    Erc20BalanceView,
    r#"[
        function balanceOf(address account) external view returns (uint256)
    ]"#,
);

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
    nonce: Arc<Mutex<NonceManager>>,
    lifecycle: Arc<Mutex<TxLifecycleManager>>,
    survival: Arc<Mutex<SurvivalState>>,
    survival_feedback: Arc<Mutex<SurvivalFeedbackEngine>>,
    market_snapshots: Arc<Mutex<MarketSnapshotCollector>>,
    token_meta: Arc<HashMap<Address, MonitoredTokenConfig>>,
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
            nonce: Arc::new(Mutex::new(NonceManager::new(512))),
            lifecycle: Arc::new(Mutex::new(TxLifecycleManager::new(512))),
            survival: Arc::new(Mutex::new(SurvivalState {
                enabled: false,
                degradation_ewma: 0.0,
                last_block: 0,
            })),
            survival_feedback: Arc::new(Mutex::new(SurvivalFeedbackEngine::new(
                SurvivalGateConfig::from_env().into(),
            ))),
            market_snapshots: Arc::new(Mutex::new(MarketSnapshotCollector::new(256))),
            token_meta: Arc::new(
                config
                    .monitored_tokens
                    .iter()
                    .cloned()
                    .map(|token| (token.address, token))
                    .collect(),
            ),
        }
    }

    pub fn register_execution(&self, event: ExecutionEvent) {
        if let Ok(mut lifecycle) = self.lifecycle.lock() {
            lifecycle.register_signed(
                event.opportunity_id.clone(),
                event.tx_hash,
                event.wallet,
                event.nonce,
                event.target_block,
                event.gas_price_wei,
                event.tip_wei,
            );
            lifecycle.mark_submitted(event.tx_hash);
        }
        if let Ok(mut nonce) = self.nonce.lock() {
            nonce.mark_submitted(
                NonceReservation {
                    address: event.wallet,
                    nonce: event.nonce,
                    generation: event.nonce_generation,
                },
                event.tx_hash,
            );
        }
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
                pool_address: event.pool_address,
                token0: event.token0,
                token1: event.token1,
                trade_input_token: event.trade_input_token,
                trade_output_token: event.trade_output_token,
                profit_token: event.profit_token,
                profit_recipient: event.profit_recipient,
                amount_in: event.amount_in,
                expected_amount_out: event.expected_amount_out,
                expected_execution_price: event.expected_execution_price,
            });
        }
        if let Ok(mut snapshots) = self.market_snapshots.lock() {
            snapshots.register_pool(PoolMetadata {
                pool_address: event.pool_address,
                token0: event.token0,
                token1: event.token1,
                trade_input_token: event.trade_input_token,
                trade_output_token: event.trade_output_token,
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

    pub fn nonce(&self) -> Arc<Mutex<NonceManager>> {
        self.nonce.clone()
    }

    pub fn lifecycle(&self) -> Arc<Mutex<TxLifecycleManager>> {
        self.lifecycle.clone()
    }

    pub fn market_snapshots(&self) -> Arc<Mutex<MarketSnapshotCollector>> {
        self.market_snapshots.clone()
    }

    pub fn register_market_pool(&self, metadata: PoolMetadata) {
        if let Ok(mut snapshots) = self.market_snapshots.lock() {
            snapshots.register_pool(metadata);
        }
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
    pub opportunity_id: String,
    pub bundle_hash: Option<H256>,
    pub tx_hash: H256,
    pub wallet: Address,
    pub nonce: U256,
    pub nonce_generation: u64,
    pub target_block: u64,
    pub submitted_at: std::time::Instant,
    pub relay: String,
    pub tip_wei: U256,
    pub gas_price_wei: U256,
    pub expected_profit_usd: f64,
    pub competition_score: f64,
    pub pool_address: Address,
    pub token0: Address,
    pub token1: Address,
    pub trade_input_token: Address,
    pub trade_output_token: Address,
    pub profit_token: Address,
    pub profit_recipient: Address,
    pub amount_in: U256,
    pub expected_amount_out: U256,
    pub expected_execution_price: f64,
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
    let tracked_pools = runtime
        .market_snapshots
        .lock()
        .map(|snapshots| snapshots.pools())
        .unwrap_or_default();
    if let Ok(block_snapshots) = MarketSnapshotCollector::collect_block_snapshots(
        provider.clone(),
        &tracked_pools,
        block_number,
    )
    .await
    {
        if let Ok(mut snapshots) = runtime.market_snapshots.lock() {
            snapshots.ingest_snapshots(block_snapshots);
        }
    }
    let pending_hashes = runtime
        .truth
        .lock()
        .map(|truth| truth.pending_hashes())
        .unwrap_or_default();
    let mut receipt_results = Vec::with_capacity(pending_hashes.len());
    let mut receipt_by_hash = HashMap::with_capacity(pending_hashes.len());
    for hash in pending_hashes {
        let receipt = provider.get_transaction_receipt(hash).await.ok().flatten();
        let included_block = receipt.as_ref().and_then(|receipt| receipt.block_number);
        let success = receipt
            .as_ref()
            .and_then(|receipt| receipt.status)
            .map(|status| status.as_u64() == 1);
        let gas_used = receipt.as_ref().and_then(|receipt| receipt.gas_used);
        let effective_gas_price = receipt
            .as_ref()
            .and_then(|receipt| receipt.effective_gas_price);
        receipt_results.push((hash, included_block, success, gas_used, effective_gas_price));
        if let Some(receipt) = receipt {
            receipt_by_hash.insert(hash, receipt);
        }
    }
    let truths = {
        let mut truth = match runtime.truth.lock() {
            Ok(lock) => lock,
            Err(_) => return,
        };
        let mut truths = Vec::new();
        for (hash, included_block, success, gas_used, effective_gas_price) in receipt_results {
            if let Some(item) = truth.reconcile_receipt(
                hash,
                included_block,
                success,
                gas_used,
                effective_gas_price,
                block_number,
                &competing,
            ) {
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
        finalize_execution(runtime, &truth);
        let receipt = receipt_by_hash.remove(&truth.tx_hash);
        tokio::spawn(update_market_truth(
            runtime.clone(),
            truth.clone(),
            receipt,
            provider.clone(),
        ));
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

async fn update_market_truth(
    runtime: LearningRuntime,
    truth: crate::mev::inclusion_truth::InclusionTruth,
    receipt: Option<TransactionReceipt>,
    provider: Arc<ethers::providers::Provider<ethers::providers::Http>>,
) {
    let event_store = runtime
        .lifecycle
        .lock()
        .ok()
        .and_then(|lifecycle| lifecycle.event_store());
    let Some(event_store) = event_store else {
        return;
    };

    let execution_ts_ms =
        observed_execution_timestamp_ms(provider.clone(), &truth, receipt.as_ref()).await;
    collect_observed_markout_samples(
        runtime.market_snapshots(),
        provider.clone(),
        PoolMetadata {
            pool_address: truth.pool_address,
            token0: truth.token0,
            token1: truth.token1,
            trade_input_token: truth.trade_input_token,
            trade_output_token: truth.trade_output_token,
        },
        execution_ts_ms,
    )
    .await;

    let competitor_pressure = truth.competition_score.clamp(0.0, 1.0);
    let market_snapshots = runtime
        .market_snapshots
        .lock()
        .ok()
        .map(|collector| collector.collect_markout_snapshots(truth.pool_address, execution_ts_ms))
        .unwrap_or_default();
    let execution_observation = receipt.as_ref().and_then(|receipt| {
        extract_execution_price(
            receipt,
            truth.pool_address,
            truth.token0,
            truth.token1,
            truth.trade_input_token,
            truth.trade_output_token,
        )
    });
    let expected_price = truth.expected_execution_price;
    let execution_price = execution_observation
        .map(|observation| observation.execution_price)
        .unwrap_or_default();
    let slippage_bps =
        if expected_price.is_finite() && expected_price > 0.0 && execution_price > 0.0 {
            ((execution_price - expected_price) / expected_price) * 10_000.0
        } else {
            f64::NAN
        };
    let fill_ratio = execution_observation
        .and_then(|observation| ratio_to_f64(observation.amount_out, truth.expected_amount_out))
        .unwrap_or(0.0);
    let (has_balance_delta, balance_delta_wei, gas_paid_wei, realized_pnl) =
        observed_balance_delta(&runtime, &truth, provider.clone()).await;
    let data_quality = DataQuality {
        has_snapshots: market_snapshots.len() == 4,
        has_real_execution_price: execution_observation.is_some(),
        has_real_slippage: slippage_bps.is_finite(),
        has_balance_delta,
    };
    let input = MarketTruthInput {
        truth: truth.clone(),
        entry_timestamp_ms: execution_ts_ms,
        expected_price,
        execution_price,
        net_execution_value: realized_pnl,
        balance_delta_wei,
        gas_paid_wei,
        slippage_bps,
        fill_ratio,
        market_snapshots,
        data_quality,
        competition: CompetitionRealityInput {
            route: RouteCluster {
                pool: truth.pool_address,
                token_in: truth.trade_input_token,
                token_out: truth.trade_output_token,
            },
            mempool_similar_count: 0,
            inclusion_delay_ms: truth.latency_ms,
            competitor_pressure,
            competing_included_count: 0,
            observed_alpha_before: expected_price,
            observed_alpha_after: execution_price,
        },
        survival: EdgeSurvivalInput {
            competition_pressure: competitor_pressure,
            mempool_congestion: 0.0,
            historical_markout_degradation: if realized_pnl.is_finite() && realized_pnl < 0.0 {
                1.0
            } else {
                0.0
            },
            latency_ms: truth.latency_ms,
        },
        expected_execution_value: truth.expected_profit_usd,
        observed_best_execution_value: truth.expected_profit_usd.max(0.0),
    };
    let report = TruthPipeline::run(input);
    TruthPipeline::append_report(&event_store, &report);
    if let Ok(mut feedback) = runtime.survival_feedback.lock() {
        let update = feedback.ingest_report(&report);
        let _ = event_store.append(StateEvent::SurvivalFeedbackUpdate(
            crate::mev::state::event_store::SurvivalFeedbackUpdate {
                tx_hash: update.tx_hash,
                outcome: update.outcome,
                sample_class: update.sample_class,
                accepted_sample: update.accepted_sample,
                false_positive_ewma: update.false_positive_ewma,
                false_negative_ewma: update.false_negative_ewma,
                good_execution_ewma: update.good_execution_ewma,
                false_positives: update.false_positives,
                false_negatives: update.false_negatives,
                correct_decisions: update.correct_decisions,
                low_confidence_ignored: update.low_confidence_ignored,
                accepted_samples: update.accepted_samples,
                adaptation_drift: update.adaptation_drift,
                survival_probability_threshold: update.survival_probability_threshold,
                max_competitor_capture: update.max_competitor_capture,
                max_latency_risk: update.max_latency_risk,
                max_staleness: update.max_staleness,
                min_pool_freshness: update.min_pool_freshness,
            },
        ));
    }
}

fn finalize_execution(
    runtime: &LearningRuntime,
    truth: &crate::mev::inclusion_truth::InclusionTruth,
) {
    let Ok(mut lifecycle) = runtime.lifecycle.lock() else {
        return;
    };
    let Ok(mut nonce) = runtime.nonce.lock() else {
        return;
    };
    let _ = ExecutionFinalizer::finalize_truth(&mut lifecycle, &mut nonce, None, truth);
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

async fn observed_balance_delta(
    runtime: &LearningRuntime,
    truth: &crate::mev::inclusion_truth::InclusionTruth,
    provider: Arc<ethers::providers::Provider<ethers::providers::Http>>,
) -> (bool, U256, U256, f64) {
    let Some(block_number) = truth.included_block else {
        return (false, U256::zero(), U256::zero(), f64::NAN);
    };
    let token = Erc20BalanceView::new(truth.profit_token, provider.clone());
    let pre_block = block_number.saturating_sub(1);
    let pre_balance = token
        .balance_of(truth.profit_recipient)
        .block(BlockId::Number(BlockNumber::Number(pre_block.into())))
        .call()
        .await
        .ok();
    let post_balance = token
        .balance_of(truth.profit_recipient)
        .block(BlockId::Number(BlockNumber::Number(block_number.into())))
        .call()
        .await
        .ok();
    let (Some(pre_balance), Some(post_balance)) = (pre_balance, post_balance) else {
        return (false, U256::zero(), U256::zero(), f64::NAN);
    };
    let balance_delta = post_balance.saturating_sub(pre_balance);
    let gas_paid = truth.gas_used.saturating_mul(truth.effective_gas_price);
    let realized = runtime
        .token_meta
        .get(&truth.profit_token)
        .filter(|meta| {
            meta.asset_class == AssetClass::Native
                || meta.symbol.eq_ignore_ascii_case("WETH")
                || meta.symbol.eq_ignore_ascii_case("ETH")
        })
        .map(|_| {
            let balance_f = balance_delta.to_string().parse::<f64>().unwrap_or(0.0) / 1e18;
            let gas_f = gas_paid.to_string().parse::<f64>().unwrap_or(0.0) / 1e18;
            balance_f - gas_f
        })
        .unwrap_or(f64::NAN);
    (true, balance_delta, gas_paid, realized)
}

async fn observed_execution_timestamp_ms(
    provider: Arc<ethers::providers::Provider<ethers::providers::Http>>,
    truth: &crate::mev::inclusion_truth::InclusionTruth,
    receipt: Option<&TransactionReceipt>,
) -> u64 {
    if let Some(block_number) = receipt.and_then(|receipt| receipt.block_number) {
        if let Ok(Some(block)) = provider.get_block(block_number).await {
            return block.timestamp.as_u64().saturating_mul(1_000);
        }
    }
    unix_ms().saturating_sub(truth.latency_ms as u64)
}

async fn collect_observed_markout_samples(
    collector: Arc<Mutex<MarketSnapshotCollector>>,
    provider: Arc<ethers::providers::Provider<ethers::providers::Http>>,
    metadata: PoolMetadata,
    execution_ts_ms: u64,
) {
    for delta_ms in [100u64, 500, 1_000, 5_000] {
        let now_ms = unix_ms();
        let target_ms = execution_ts_ms.saturating_add(delta_ms);
        if target_ms > now_ms {
            tokio::time::sleep(Duration::from_millis(target_ms - now_ms)).await;
        }
        if let Ok(Some(snapshot)) = MarketSnapshotCollector::sample_pool_state(
            provider.clone(),
            metadata,
            target_ms.max(unix_ms()),
        )
        .await
        {
            if let Ok(mut lock) = collector.lock() {
                lock.ingest_snapshots(vec![snapshot]);
            }
        }
    }
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}
