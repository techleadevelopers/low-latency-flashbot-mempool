use crate::config::{BotMode, Config};
use crate::dashboard::DashboardHandle;
use crate::mev::analytics::missed_opportunities::{MissReason, MissedOpportunityTracker};
use crate::mev::capital::CapitalManager;
use crate::mev::feedback::{ExecutionFeedback, FailureReason, FeedbackEngine};
use crate::mev::inclusion::{InclusionContext, InclusionEngine};
use crate::mev::inclusion_truth::{InclusionTruthEngine, PendingBundleRecord};
use crate::mev::opportunity::{wei_to_eth_f64, MevOpportunity};
use crate::mev::pnl::tracker::PnlTracker;
use crate::mev::simulation::bundle_simulator::{BundleSimulationRequest, BundleSimulator};
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
}

impl ExecutionEngine {
    pub fn new(
        config: Arc<Config>,
        rpc_fleet: Arc<RpcFleet>,
        dashboard: DashboardHandle,
        capital: Arc<Mutex<CapitalManager>>,
    ) -> Self {
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
        }
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
        let feedback_metrics = self
            .feedback
            .lock()
            .expect("feedback engine lock")
            .metrics();
        let inclusion_plan = {
            let inclusion = self.inclusion.lock().expect("inclusion engine lock");
            inclusion.plan(
                InclusionContext {
                    expected_profit_usd: wei_to_eth_f64(payload.expected_profit_wei)
                        * self.config.mev.eth_usd_price,
                    gas_cost_usd: wei_to_eth_f64(opportunity.score.execution_cost_wei)
                        * self.config.mev.eth_usd_price,
                    competition_score: opportunity.score.competition_score as f64 / 100.0,
                    confidence_score: opportunity.score.confidence_score as f64 / 100.0,
                    urgency: (opportunity.age_ms() as f64
                        / self.config.mev.max_pending_age_ms.max(1) as f64)
                        .clamp(0.0, 1.0),
                    recent_failures: feedback_metrics.bad_streak,
                    tip_scaling_factor: adaptive_thresholds.tip_scaling_factor,
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
        if payload.tx.0.is_empty() {
            let nonce = provider
                .get_transaction_count(wallet.address(), Some(BlockNumber::Pending.into()))
                .await?;
            let gas_price = provider.get_gas_price().await?;
            let tip_per_gas = if payload.gas_limit > 0 {
                inclusion_plan.tip_wei / U256::from(payload.gas_limit)
            } else {
                U256::zero()
            };
            payload.tx =
                sign_executor_transaction(&wallet, &payload, nonce, gas_price + tip_per_gas)
                    .await?;
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
                self.truth
                    .lock()
                    .expect("truth engine lock")
                    .register(PendingBundleRecord {
                        bundle_hash: pending.bundle_hash,
                        tx_hash,
                        target_block: (block + 1).as_u64(),
                        submitted_at: std::time::Instant::now(),
                        relay: self.config.flashbots_relay.clone(),
                        tip_wei: inclusion_plan.tip_wei,
                        expected_profit_usd: payload.expected_profit
                            * self.config.mev.eth_usd_price,
                        competition_score: opportunity.score.competition_score as f64 / 100.0,
                    });
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

fn signed_tx_hash(raw: &Bytes) -> H256 {
    H256::from(ethers::utils::keccak256(raw.as_ref()))
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
