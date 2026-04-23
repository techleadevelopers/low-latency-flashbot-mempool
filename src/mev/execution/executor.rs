use crate::config::{BotMode, Config};
use crate::dashboard::DashboardHandle;
use crate::mev::analytics::missed_opportunities::{MissReason, MissedOpportunityTracker};
use crate::mev::block_loop::{ExecutionEvent, LearningRuntime};
use crate::mev::capital::CapitalManager;
use crate::mev::execution::nonce_manager::{NonceManager, NonceReservation};
use crate::mev::execution::replacement_engine::ReplacementPolicy;
use crate::mev::execution::tx_lifecycle::TxLifecycleManager;
use crate::mev::feedback::{ExecutionFeedback, FailureReason, FeedbackEngine};
use crate::mev::inclusion::{InclusionContext, InclusionEngine};
use crate::mev::inclusion_truth::{InclusionTruthEngine, PendingBundleRecord};
use crate::mev::market_truth::edge_survival::{EdgeSurvivalEngine, EdgeSurvivalInput};
use crate::mev::opportunity::{wei_to_eth_f64, MevOpportunity};
use crate::mev::pnl::tracker::PnlTracker;
use crate::mev::simulation::bundle_simulator::{BundleSimulationRequest, BundleSimulator};
use crate::mev::state::recovery::RecoveryEngine;
use crate::mev::state::snapshot::drift_checker::{compare, DriftSeverity};
use crate::mev::state::snapshot::snapshot_daemon::{
    build_live_snapshot, spawn_snapshot_daemon, state_dir as mev_state_dir, SnapshotDaemonConfig,
};
use crate::mev::state::snapshot::{RiskStateSummary, SnapshotStore};
use crate::mev::survival::survival_gate::{
    SurvivalDropReason, SurvivalGate, SurvivalGateConfig, SurvivalGateDecision, SurvivalGateInput,
};
use crate::rpc::RpcFleet;
use ethers::prelude::*;
use ethers::types::transaction::eip2718::TypedTransaction;
use ethers_flashbots::{BundleRequest, FlashbotsMiddleware};
use std::sync::{Arc, Mutex};
use tracing::warn;
use url::Url;

pub struct ExecutionEngine {
    config: Arc<Config>,
    rpc_fleet: Arc<RpcFleet>,
    dashboard: DashboardHandle,
    capital: Arc<Mutex<CapitalManager>>,
    missed: Arc<Mutex<MissedOpportunityTracker>>,
    pnl: Arc<Mutex<PnlTracker>>,
    inclusion: Arc<Mutex<InclusionEngine>>,
    feedback: Arc<Mutex<FeedbackEngine>>,
    truth: Arc<Mutex<InclusionTruthEngine>>,
    nonce: Arc<Mutex<NonceManager>>,
    lifecycle: Arc<Mutex<TxLifecycleManager>>,
    learning: Option<LearningRuntime>,
}

impl ExecutionEngine {
    pub fn new(
        config: Arc<Config>,
        rpc_fleet: Arc<RpcFleet>,
        dashboard: DashboardHandle,
        capital: Arc<Mutex<CapitalManager>>,
    ) -> Self {
        let mut nonce = NonceManager::new(256);
        let mut lifecycle = TxLifecycleManager::new(256);
        let _durability = install_recovery_hook(&config, &dashboard, &mut nonce, &mut lifecycle);
        let nonce = Arc::new(Mutex::new(nonce));
        let lifecycle = Arc::new(Mutex::new(lifecycle));
        Self {
            config: config.clone(),
            rpc_fleet,
            dashboard,
            capital,
            missed: Arc::new(Mutex::new(MissedOpportunityTracker::default())),
            pnl: Arc::new(Mutex::new(PnlTracker::default())),
            inclusion: Arc::new(Mutex::new(InclusionEngine::from_config(&config))),
            feedback: Arc::new(Mutex::new(FeedbackEngine::new(256))),
            truth: Arc::new(Mutex::new(InclusionTruthEngine::new(256, 2))),
            nonce,
            lifecycle,
            learning: None,
        }
    }

    pub fn with_learning_runtime(mut self, learning: LearningRuntime) -> Self {
        let nonce_handle = learning.nonce();
        let lifecycle_handle = learning.lifecycle();
        if let (Ok(mut nonce), Ok(mut lifecycle)) = (nonce_handle.lock(), lifecycle_handle.lock()) {
            if let Some(event_store) =
                install_recovery_hook(&self.config, &self.dashboard, &mut nonce, &mut lifecycle)
            {
                start_snapshot_daemon(
                    &self.config,
                    event_store,
                    nonce_handle.clone(),
                    lifecycle_handle.clone(),
                );
            }
        }
        self.feedback = learning.feedback();
        self.inclusion = learning.inclusion();
        self.nonce = learning.nonce();
        self.lifecycle = learning.lifecycle();
        self.learning = Some(learning);
        self
    }

    pub async fn handle(
        &self,
        opportunity: MevOpportunity,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if opportunity.age_ms() > u128::from(self.config.mev.max_pending_age_ms) {
            self.dashboard.event(
                "info",
                format!(
                    "MEV {} rejected victim={:?}: stale pending tx age={}ms",
                    opportunity.kind.as_str(),
                    opportunity.victim_tx,
                    opportunity.age_ms()
                ),
            );
            self.record_miss(MissReason::LatencyExceeded);
            return Ok(());
        }

        let decision = {
            let mut capital = self.capital.lock().expect("capital manager lock");
            capital.evaluate(&opportunity)
        };

        if !decision.accepted {
            self.dashboard.event(
                "info",
                format!(
                    "MEV {} rejected victim={:?}: {} drawdown={}bps",
                    opportunity.kind.as_str(),
                    opportunity.victim_tx,
                    decision.reason,
                    decision.drawdown_bps
                ),
            );
            self.record_miss(MissReason::CapitalLimit);
            return Ok(());
        }

        match self.config.bot_mode {
            BotMode::Shadow => {
                self.dashboard.event(
                    "info",
                    format!(
                    "shadow MEV accepted kind={} victim={:?} profit={:.6} ETH roi={}bps confidence={} risk={} competition={} allocation={:.6} ETH",
                    opportunity.kind.as_str(),
                    opportunity.victim_tx,
                    wei_to_eth_f64(opportunity.score.slippage_adjusted_profit_wei),
                        opportunity.score.roi_bps,
                        opportunity.score.confidence_score,
                        opportunity.score.risk_score,
                        opportunity.score.competition_score,
                        wei_to_eth_f64(decision.allocation_wei)
                ),
            );
                self.dashboard.event(
                    "info",
                    format!(
                        "shadow MEV route id={} router={:?} in={:?} out={:?} gas_limit={} gross_expected={:.6} ETH",
                        opportunity.id,
                        opportunity.target,
                        opportunity.input_token,
                        opportunity.output_token,
                        opportunity.gas_limit,
                        wei_to_eth_f64(opportunity.score.expected_profit_wei)
                    ),
                );
                Ok(())
            }
            BotMode::Paper => {
                self.paper_execute(opportunity).await;
                Ok(())
            }
            BotMode::Live => self.live_execute(opportunity).await,
        }
    }

    async fn paper_execute(&self, opportunity: MevOpportunity) {
        let filled = opportunity.score.confidence_score as u64 * 100
            >= self.config.mev.paper_fill_probability_bps.min(10_000);
        let pnl_wei = if filled {
            opportunity.score.slippage_adjusted_profit_wei.as_u128() as i128
        } else {
            -(opportunity.score.execution_cost_wei.as_u128() as i128)
        };

        {
            let mut capital = self.capital.lock().expect("capital manager lock");
            capital.reserve_execution(opportunity.score.execution_cost_wei);
            capital.record_pnl(pnl_wei);
            self.dashboard.event(
                if filled { "success" } else { "warn" },
                format!(
                    "paper MEV {} victim={:?} filled={} pnl={:.6} ETH capital={:.6} ETH daily_pnl={:.6} ETH",
                    opportunity.kind.as_str(),
                    opportunity.victim_tx,
                    filled,
                    pnl_wei as f64 / 1e18,
                    capital.current_capital_eth(),
                    capital.daily_pnl_eth()
                ),
            );
        }
    }

    async fn live_execute(
        &self,
        opportunity: MevOpportunity,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !self.config.allow_send {
            self.dashboard
                .event("warn", "live MEV blocked: ALLOW_SEND=false".to_string());
            return Ok(());
        }

        if self
            .learning
            .as_ref()
            .map(|learning| learning.survival_mode())
            .unwrap_or(false)
        {
            self.dashboard.event(
                "warn",
                format!(
                    "live MEV blocked victim={:?}: survival mode active",
                    opportunity.victim_tx
                ),
            );
            self.record_miss(MissReason::CapitalLimit);
            return Ok(());
        }

        if !opportunity.private_only && !self.config.mev.allow_public_mempool {
            self.dashboard.event(
                "warn",
                format!(
                    "live MEV blocked victim={:?}: public mempool fallback disabled",
                    opportunity.victim_tx
                ),
            );
            return Ok(());
        }

        let feedback_metrics = self
            .feedback
            .lock()
            .expect("feedback engine lock")
            .metrics();
        let pressure = self.learning.as_ref().and_then(|learning| {
            learning
                .pressure()
                .lock()
                .ok()
                .map(|pressure| pressure.pressure(opportunity.target))
        });
        let forecast = self.learning.as_ref().map(|learning| {
            learning.competition_forecast(
                opportunity.target,
                opportunity.input_token,
                opportunity.output_token,
                opportunity
                    .victim_transaction
                    .as_ref()
                    .and_then(tx_selector)
                    .unwrap_or([0u8; 4]),
            )
        });
        if let (Some(learning), Some(forecast)) = (&self.learning, forecast) {
            if let Ok(mut pressure) = learning.pressure().lock() {
                pressure.record_forecast(opportunity.target, forecast);
            }
        }

        let max_pending_age_ms = self.config.mev.max_pending_age_ms.max(1);
        let estimated_latency_ms = opportunity.age_ms().min(u128::from(u64::MAX)) as u64;
        let forecast_probability = forecast
            .map(|forecast| forecast.pressure_probability)
            .unwrap_or(opportunity.score.competition_score as f64 / 100.0)
            .clamp(0.0, 1.0);
        let mempool_congestion = pressure
            .map(|pressure| pressure.mempool_congestion)
            .or_else(|| forecast.map(|forecast| forecast.mempool_density))
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
        let pressure_heat = pressure
            .map(|pressure| pressure.heat.max(pressure.forward_pressure))
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
        let historical_markout_degradation =
            (1.0 - feedback_metrics.expected_vs_real_ratio).clamp(0.0, 1.0);
        let edge_survival = EdgeSurvivalEngine::compute(EdgeSurvivalInput {
            competition_pressure: forecast_probability.max(pressure_heat),
            mempool_congestion,
            historical_markout_degradation,
            latency_ms: opportunity.age_ms(),
        });
        let competitor_capture_likelihood = (forecast_probability * 0.60
            + pressure_heat * 0.25
            + if forecast
                .map(|forecast| forecast.likely_outbid)
                .unwrap_or(false)
            {
                0.20
            } else {
                0.0
            })
        .clamp(0.0, 1.0);
        let latency_risk_score =
            (estimated_latency_ms as f64 / max_pending_age_ms as f64).clamp(0.0, 1.0);
        let similar_pending_pressure = forecast
            .map(|forecast| forecast.similar_pending as f64 / 32.0)
            .unwrap_or(0.0)
            .clamp(0.0, 1.0);
        let mempool_staleness_score = (latency_risk_score * 0.65
            + mempool_congestion * 0.20
            + similar_pending_pressure * 0.15)
            .clamp(0.0, 1.0);
        let pool_state_freshness_score =
            (1.0 - pressure_heat * 0.55 - mempool_staleness_score * 0.45).clamp(0.0, 1.0);
        let survival_input = SurvivalGateInput {
            edge_survival_probability: edge_survival.survival_probability,
            execution_viability_window_ms: edge_survival.execution_viability_window_ms,
            estimated_latency_ms,
            competitor_capture_likelihood,
            latency_risk_score,
            mempool_staleness_score,
            pool_state_freshness_score,
        };

        match SurvivalGate::evaluate(SurvivalGateConfig::from_env(), survival_input) {
            SurvivalGateDecision::Allow => {}
            SurvivalGateDecision::Drop(reason) => {
                self.dashboard.event(
                    "info",
                    format!(
                        "survival gate dropped victim={:?} reason={} survival={:.3} window_ms={} estimated_latency_ms={} capture={:.3} latency_risk={:.3} mempool_stale={:.3} pool_fresh={:.3}",
                        opportunity.victim_tx,
                        reason.as_str(),
                        survival_input.edge_survival_probability,
                        survival_input.execution_viability_window_ms,
                        survival_input.estimated_latency_ms,
                        survival_input.competitor_capture_likelihood,
                        survival_input.latency_risk_score,
                        survival_input.mempool_staleness_score,
                        survival_input.pool_state_freshness_score
                    ),
                );
                self.record_miss(survival_miss_reason(reason));
                self.record_feedback(ExecutionFeedback {
                    success: false,
                    included: false,
                    profit_expected: wei_to_eth_f64(opportunity.score.slippage_adjusted_profit_wei)
                        * self.config.mev.eth_usd_price,
                    profit_realized: 0.0,
                    gas_used: wei_to_eth_f64(opportunity.score.execution_cost_wei)
                        * self.config.mev.eth_usd_price,
                    tip_used: 0.0,
                    competition_score: forecast_probability
                        .max(opportunity.score.competition_score as f64 / 100.0),
                    confidence_score: opportunity.score.confidence_score as f64 / 100.0,
                    relay_used: self.config.flashbots_relay.clone(),
                    block_delay: 0,
                    failure_reason: Some(survival_failure_reason(reason)),
                });
                return Ok(());
            }
        }

        let Some(mut payload) = opportunity.execution_payload.clone() else {
            self.dashboard.event(
                "warn",
                format!(
                    "live MEV blocked victim={:?}: no execution payload attached",
                    opportunity.victim_tx
                ),
            );
            self.record_miss(MissReason::PayloadUnavailable);
            return Ok(());
        };

        let endpoint = self.rpc_fleet.send_endpoint();
        let provider = endpoint.provider.clone();
        let adaptive_thresholds = self
            .feedback
            .lock()
            .expect("feedback engine lock")
            .thresholds();
        let pressure_tip_factor = pressure
            .map(|pressure| 1.0 + pressure.heat * 0.65 + pressure.mempool_congestion * 0.25)
            .unwrap_or(1.0);
        let forecast_tip_factor = forecast
            .map(|forecast| forecast.tip_multiplier)
            .unwrap_or(1.0);
        let pressure_frequency_factor = pressure
            .map(|pressure| pressure.execution_frequency_factor)
            .unwrap_or(1.0);
        let expected_profit_usd =
            wei_to_eth_f64(payload.expected_profit_wei) * self.config.mev.eth_usd_price;
        let gas_cost_usd =
            wei_to_eth_f64(opportunity.score.execution_cost_wei) * self.config.mev.eth_usd_price;
        let forecast_probability = forecast
            .map(|forecast| forecast.pressure_probability)
            .unwrap_or(opportunity.score.competition_score as f64 / 100.0);
        let selectivity_multiplier = pressure
            .map(|pressure| pressure.selectivity_multiplier)
            .unwrap_or(1.0);
        let predicted_ev_usd =
            (expected_profit_usd - gas_cost_usd) * (1.0 - forecast_probability * 0.55);
        let adjusted_min_ev_usd = self.config.mev.inclusion_min_ev_usd * selectivity_multiplier;
        if forecast
            .map(|forecast| forecast.likely_outbid)
            .unwrap_or(false)
            && predicted_ev_usd < adjusted_min_ev_usd * 1.6
        {
            self.dashboard.event(
                "info",
                format!(
                    "predictive competition blocked victim={:?} likely_outbid=true predicted_ev={:.4} min_ev={:.4}",
                    opportunity.victim_tx, predicted_ev_usd, adjusted_min_ev_usd
                ),
            );
            self.record_miss(MissReason::HighCompetition);
            return Ok(());
        }
        if predicted_ev_usd < adjusted_min_ev_usd {
            self.dashboard.event(
                "info",
                format!(
                    "predictive EV blocked victim={:?} predicted_ev={:.4} min_ev={:.4} pressure={:.3}",
                    opportunity.victim_tx, predicted_ev_usd, adjusted_min_ev_usd, forecast_probability
                ),
            );
            self.record_miss(MissReason::HighCompetition);
            return Ok(());
        }
        if pressure_frequency_factor < 0.25
            && opportunity.score.confidence_score < self.config.mev.min_confidence_score + 10
        {
            self.dashboard.event(
                "info",
                format!(
                    "competition pressure blocked victim={:?} pressure_freq={:.3}",
                    opportunity.victim_tx, pressure_frequency_factor
                ),
            );
            self.record_miss(MissReason::HighCompetition);
            return Ok(());
        }
        let inclusion_plan = {
            let inclusion = self.inclusion.lock().expect("inclusion engine lock");
            inclusion.plan(
                InclusionContext {
                    expected_profit_usd,
                    gas_cost_usd,
                    competition_score: forecast_probability
                        .max(opportunity.score.competition_score as f64 / 100.0),
                    confidence_score: opportunity.score.confidence_score as f64 / 100.0,
                    urgency: (opportunity.age_ms() as f64
                        / self.config.mev.max_pending_age_ms.max(1) as f64)
                        .clamp(0.0, 1.0),
                    recent_failures: feedback_metrics.bad_streak,
                    tip_scaling_factor: adaptive_thresholds.tip_scaling_factor
                        * pressure_tip_factor
                        * forecast_tip_factor,
                },
                self.config.mev.eth_usd_price,
            )
        };
        if !inclusion_plan.execute {
            self.dashboard.event(
                "info",
                format!(
                    "inclusion gate blocked victim={:?} adjusted_ev={:.4} prob={:.3} tip_usd={:.4}",
                    opportunity.victim_tx,
                    inclusion_plan.adjusted_ev_usd,
                    inclusion_plan.inclusion_probability,
                    inclusion_plan.tip_usd
                ),
            );
            self.record_miss(MissReason::HighCompetition);
            self.record_feedback(ExecutionFeedback {
                success: false,
                included: false,
                profit_expected: wei_to_eth_f64(payload.expected_profit_wei)
                    * self.config.mev.eth_usd_price,
                profit_realized: 0.0,
                gas_used: wei_to_eth_f64(opportunity.score.execution_cost_wei)
                    * self.config.mev.eth_usd_price,
                tip_used: inclusion_plan.tip_usd,
                competition_score: opportunity.score.competition_score as f64 / 100.0,
                confidence_score: opportunity.score.confidence_score as f64 / 100.0,
                relay_used: self.config.flashbots_relay.clone(),
                block_delay: 0,
                failure_reason: Some(FailureReason::NotIncluded),
            });
            return Ok(());
        }
        let wallet = self
            .config
            .sender_private_key
            .parse::<LocalWallet>()?
            .with_chain_id(self.config.chain_id);
        let wallet_address = wallet.address();
        let mut reserved_nonce: Option<NonceReservation> = None;
        let mut signed_gas_price = U256::zero();
        if payload.tx.0.is_empty() {
            let chain_nonce = provider
                .get_transaction_count(wallet.address(), Some(BlockNumber::Pending.into()))
                .await?;
            let reservation = {
                let mut nonce = self.nonce.lock().expect("nonce manager lock");
                nonce.reserve(wallet.address(), chain_nonce)
            };
            let gas_price = provider.get_gas_price().await?;
            let tip_per_gas = if payload.gas_limit > 0 {
                inclusion_plan.tip_wei / U256::from(payload.gas_limit)
            } else {
                U256::zero()
            };
            signed_gas_price = gas_price + tip_per_gas;
            let replacement_policy = ReplacementPolicy::conservative(
                signed_gas_price.saturating_mul(U256::from(self.config.mev.gas_safety_margin_bps))
                    / U256::from(10_000u64),
            );
            let replacement_decision = replacement_policy.decide(signed_gas_price, 0, true);
            if !replacement_decision.replace && replacement_decision.reason != "fee bump allowed" {
                self.nonce
                    .lock()
                    .expect("nonce manager lock")
                    .release_unsubmitted(reservation);
                self.dashboard.event(
                    "warn",
                    format!(
                        "live MEV blocked victim={:?}: replacement policy rejected initial tx: {}",
                        opportunity.victim_tx, replacement_decision.reason
                    ),
                );
                return Ok(());
            }
            payload.tx = match sign_executor_transaction(
                &wallet,
                &payload,
                reservation.nonce,
                signed_gas_price,
            )
            .await
            {
                Ok(raw) => raw,
                Err(err) => {
                    self.nonce
                        .lock()
                        .expect("nonce manager lock")
                        .release_unsubmitted(reservation);
                    return Err(err);
                }
            };
            reserved_nonce = Some(reservation);
        }
        let relay_url = Url::parse(&self.config.flashbots_relay)?;
        let relay_signer = wallet.clone();
        let flashbots_client = SignerMiddleware::new(provider.clone(), wallet);
        let flashbots = FlashbotsMiddleware::new(flashbots_client, relay_url, relay_signer);
        let block = provider.get_block_number().await?;
        let simulation = BundleSimulator::deterministic_preflight(
            &self.config,
            &BundleSimulationRequest {
                victim_tx: opportunity.victim_tx,
                payload: payload.clone(),
                target_block: block.as_u64() + 1,
            },
            ethers::utils::parse_ether(self.config.mev.min_net_profit_eth.to_string())?,
        );
        if !simulation.simulation_success || simulation.revert_risk {
            self.dashboard.event(
                "warn",
                format!(
                    "live MEV blocked victim={:?}: bundle simulation failed: {}",
                    opportunity.victim_tx,
                    simulation.reason.unwrap_or_else(|| "unknown".to_string())
                ),
            );
            self.record_miss(MissReason::SimulationFailed);
            self.record_feedback(ExecutionFeedback {
                success: false,
                included: false,
                profit_expected: payload.expected_profit,
                profit_realized: 0.0,
                gas_used: wei_to_eth_f64(opportunity.score.execution_cost_wei)
                    * self.config.mev.eth_usd_price,
                tip_used: inclusion_plan.tip_usd,
                competition_score: opportunity.score.competition_score as f64 / 100.0,
                confidence_score: opportunity.score.confidence_score as f64 / 100.0,
                relay_used: self.config.flashbots_relay.clone(),
                block_delay: 0,
                failure_reason: Some(FailureReason::SimulationFailed),
            });
            return Ok(());
        }
        let mut bundle = BundleRequest::new().set_block(block + 1);
        if let Some(victim) = opportunity.victim_transaction.clone() {
            bundle = bundle.push_revertible_transaction(victim);
        }
        bundle = bundle.push_transaction(payload.tx.clone());

        {
            let mut capital = self.capital.lock().expect("capital manager lock");
            capital.reserve_execution(opportunity.score.execution_cost_wei);
        }

        let started = std::time::Instant::now();
        let bundle_result = flashbots.send_bundle(&bundle).await;
        match bundle_result {
            Ok(pending) => {
                let tx_hash = signed_tx_hash(&payload.tx);
                let reservation = reserved_nonce.unwrap_or(NonceReservation {
                    address: wallet_address,
                    nonce: U256::zero(),
                    generation: 0,
                });
                let event = ExecutionEvent {
                    opportunity_id: opportunity.id.clone(),
                    bundle_hash: pending.bundle_hash,
                    tx_hash,
                    wallet: wallet_address,
                    nonce: reservation.nonce,
                    nonce_generation: reservation.generation,
                    target_block: (block + 1).as_u64(),
                    submitted_at: std::time::Instant::now(),
                    relay: self.config.flashbots_relay.clone(),
                    tip_wei: inclusion_plan.tip_wei,
                    gas_price_wei: signed_gas_price,
                    expected_profit_usd: payload.expected_profit * self.config.mev.eth_usd_price,
                    competition_score: opportunity.score.competition_score as f64 / 100.0,
                };
                if let Some(learning) = &self.learning {
                    learning.register_execution(event);
                } else {
                    self.lifecycle
                        .lock()
                        .expect("tx lifecycle lock")
                        .register_signed(
                            event.opportunity_id.clone(),
                            event.tx_hash,
                            event.wallet,
                            event.nonce,
                            event.target_block,
                            event.gas_price_wei,
                            event.tip_wei,
                        );
                    self.lifecycle
                        .lock()
                        .expect("tx lifecycle lock")
                        .mark_submitted(event.tx_hash);
                    self.nonce
                        .lock()
                        .expect("nonce manager lock")
                        .mark_submitted(
                            NonceReservation {
                                address: event.wallet,
                                nonce: event.nonce,
                                generation: event.nonce_generation,
                            },
                            event.tx_hash,
                        );
                    self.truth
                        .lock()
                        .expect("truth engine lock")
                        .register(PendingBundleRecord {
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
                self.dashboard.record_latency(
                    "mev_bundle_attempt",
                    started.elapsed().as_millis(),
                    None,
                    Some(&endpoint.name),
                );
                self.dashboard.event(
                    "success",
                    format!(
                        "live MEV bundle submitted victim={:?} bundle={:?} expected_profit={:.6} ETH",
                        opportunity.victim_tx,
                        pending.bundle_hash,
                        wei_to_eth_f64(opportunity.score.slippage_adjusted_profit_wei)
                    ),
                );
                self.pnl.lock().expect("pnl tracker lock").record(
                    &crate::mev::pnl::tracker::ExecutionResult {
                        expected_profit: payload.expected_profit,
                        realized_profit: wei_to_eth_f64(simulation.realized_profit_wei),
                        gas_used: simulation.gas_used,
                        success: true,
                    },
                );
                self.record_feedback(ExecutionFeedback {
                    success: true,
                    included: true,
                    profit_expected: payload.expected_profit,
                    profit_realized: wei_to_eth_f64(simulation.realized_profit_wei)
                        * self.config.mev.eth_usd_price,
                    gas_used: wei_to_eth_f64(opportunity.score.execution_cost_wei)
                        * self.config.mev.eth_usd_price,
                    tip_used: inclusion_plan.tip_usd,
                    competition_score: opportunity.score.competition_score as f64 / 100.0,
                    confidence_score: opportunity.score.confidence_score as f64 / 100.0,
                    relay_used: self.config.flashbots_relay.clone(),
                    block_delay: 0,
                    failure_reason: None,
                });
                Ok(())
            }
            Err(err) => {
                warn!("MEV bundle failed: {}", err);
                self.dashboard.event(
                    "error",
                    format!(
                        "live MEV bundle failed victim={:?}: {}",
                        opportunity.victim_tx, err
                    ),
                );
                self.record_feedback(ExecutionFeedback {
                    success: false,
                    included: false,
                    profit_expected: payload.expected_profit,
                    profit_realized: 0.0,
                    gas_used: wei_to_eth_f64(opportunity.score.execution_cost_wei)
                        * self.config.mev.eth_usd_price,
                    tip_used: inclusion_plan.tip_usd,
                    competition_score: opportunity.score.competition_score as f64 / 100.0,
                    confidence_score: opportunity.score.confidence_score as f64 / 100.0,
                    relay_used: self.config.flashbots_relay.clone(),
                    block_delay: 0,
                    failure_reason: Some(FailureReason::RelayError),
                });
                Err(err.into())
            }
        }
    }

    fn record_miss(&self, reason: MissReason) {
        if let Ok(mut missed) = self.missed.lock() {
            missed.record(reason);
        }
        self.dashboard.event(
            "info",
            format!("missed_opportunity reason={}", reason.as_str()),
        );
    }

    fn record_feedback(&self, feedback: ExecutionFeedback) {
        let (metrics, thresholds) = {
            let mut engine = self.feedback.lock().expect("feedback engine lock");
            engine.record(feedback);
            (engine.metrics(), engine.thresholds())
        };
        self.dashboard.event(
            "info",
            format!(
                "feedback updated inclusion_rate={:.3} success_rate={:.3} expected_real={:.3} tip_eff={:.3} min_profit_x={:.2} tip_x={:.2} freq_x={:.2}",
                metrics.inclusion_rate,
                metrics.success_rate,
                metrics.expected_vs_real_ratio,
                metrics.tip_efficiency,
                thresholds.min_profit_multiplier,
                thresholds.tip_scaling_factor,
                thresholds.execution_frequency_factor
            ),
        );
    }
}

fn survival_miss_reason(reason: SurvivalDropReason) -> MissReason {
    match reason {
        SurvivalDropReason::CompetitorCapture => MissReason::HighCompetition,
        SurvivalDropReason::EdgeDecay
        | SurvivalDropReason::ViabilityWindowExceeded
        | SurvivalDropReason::LatencyRisk
        | SurvivalDropReason::MempoolStale
        | SurvivalDropReason::PoolStateDecayed => MissReason::LatencyExceeded,
    }
}

fn survival_failure_reason(reason: SurvivalDropReason) -> FailureReason {
    match reason {
        SurvivalDropReason::CompetitorCapture => FailureReason::Outbid,
        SurvivalDropReason::EdgeDecay
        | SurvivalDropReason::ViabilityWindowExceeded
        | SurvivalDropReason::LatencyRisk
        | SurvivalDropReason::MempoolStale
        | SurvivalDropReason::PoolStateDecayed => FailureReason::StaleState,
    }
}

fn signed_tx_hash(raw: &Bytes) -> H256 {
    H256::from(ethers::utils::keccak256(raw.as_ref()))
}

fn install_recovery_hook(
    config: &Config,
    dashboard: &DashboardHandle,
    nonce: &mut NonceManager,
    lifecycle: &mut TxLifecycleManager,
) -> Option<Arc<crate::mev::state::event_store::EventStore>> {
    let state_dir = mev_state_dir(&config.storage_path);
    match RecoveryEngine::open(&state_dir) {
        Ok(recovery) => {
            let event_store = recovery.event_store();
            match recovery.recover_managers(nonce, lifecycle) {
                Ok(report) => dashboard.event(
                    "info",
                    format!(
                        "MEV recovery loaded snapshot={} snapshot_seq={} replayed={} last_seq={}",
                        report.snapshot_loaded,
                        report.snapshot_sequence,
                        report.events_replayed,
                        report.last_sequence
                    ),
                ),
                Err(err) => dashboard.event(
                    "error",
                    format!("MEV recovery failed, starting guarded empty state: {err}"),
                ),
            }
            run_startup_drift_check(
                config,
                dashboard,
                &state_dir,
                event_store.current_sequence(),
                nonce,
                lifecycle,
            );
            nonce.set_event_store(event_store.clone());
            lifecycle.set_event_store(event_store.clone());
            Some(event_store)
        }
        Err(err) => {
            dashboard.event(
                "error",
                format!("MEV recovery store unavailable, in-memory state only: {err}"),
            );
            None
        }
    }
}

fn run_startup_drift_check(
    config: &Config,
    dashboard: &DashboardHandle,
    state_dir: &std::path::Path,
    last_event_sequence: u64,
    nonce: &NonceManager,
    lifecycle: &TxLifecycleManager,
) {
    let Ok(snapshot_store) = SnapshotStore::new(state_dir.join("snapshots")) else {
        return;
    };
    let Ok(Some(snapshot)) = snapshot_store.load_latest() else {
        return;
    };
    let live = build_live_snapshot(
        nonce,
        lifecycle,
        0,
        last_event_sequence,
        RiskStateSummary::default(),
    );
    let report = compare(&snapshot, &live);
    if report.drift_detected {
        dashboard.event(
            if matches!(report.severity, DriftSeverity::High) {
                "warn"
            } else {
                "info"
            },
            format!(
                "MEV recovery drift severity={:?} mismatches={}",
                report.severity,
                report.mismatches.len()
            ),
        );
    }
    if matches!(report.severity, DriftSeverity::High) {
        let forced = build_live_snapshot(
            nonce,
            lifecycle,
            0,
            last_event_sequence,
            RiskStateSummary::default(),
        );
        let _ = snapshot_store.save(&forced);
        dashboard.event(
            "warn",
            format!(
                "MEV recovery forced snapshot resync storage={}",
                config.storage_path.display()
            ),
        );
    }
}

fn start_snapshot_daemon(
    config: &Config,
    event_store: Arc<crate::mev::state::event_store::EventStore>,
    nonce: Arc<Mutex<NonceManager>>,
    lifecycle: Arc<Mutex<TxLifecycleManager>>,
) {
    let daemon_config = SnapshotDaemonConfig::from_env();
    let _handle = spawn_snapshot_daemon(
        mev_state_dir(&config.storage_path),
        daemon_config,
        event_store,
        nonce,
        lifecycle,
    );
}

fn tx_selector(tx: &Transaction) -> Option<[u8; 4]> {
    let input = tx.input.as_ref();
    (input.len() >= 4).then(|| [input[0], input[1], input[2], input[3]])
}

async fn sign_executor_transaction(
    wallet: &LocalWallet,
    payload: &crate::mev::execution::payload_builder::ExecutionPayload,
    nonce: U256,
    gas_price: U256,
) -> Result<Bytes, Box<dyn std::error::Error>> {
    let tx: TypedTransaction = TransactionRequest::new()
        .to(payload.target_contract)
        .data(payload.calldata.clone())
        .value(payload.value)
        .gas(payload.gas_limit)
        .gas_price(gas_price)
        .nonce(nonce)
        .from(wallet.address())
        .into();
    let signature = wallet.sign_transaction(&tx).await?;
    Ok(tx.rlp_signed(&signature))
}

#[allow(dead_code)]
fn gas_with_safety_margin(config: &Config, gas_price: U256) -> U256 {
    gas_price.saturating_mul(U256::from(config.mev.gas_safety_margin_bps)) / U256::from(10_000u64)
}

#[allow(dead_code)]
fn empty_tx_for_payload_guard(_: &TypedTransaction) {}
