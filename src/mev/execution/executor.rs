use crate::config::{BotMode, Config};
use crate::dashboard::DashboardHandle;
use crate::mev::capital::CapitalManager;
use crate::mev::analytics::missed_opportunities::{MissReason, MissedOpportunityTracker};
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
}

impl ExecutionEngine {
    pub fn new(
        config: Arc<Config>,
        rpc_fleet: Arc<RpcFleet>,
        dashboard: DashboardHandle,
        capital: Arc<Mutex<CapitalManager>>,
    ) -> Self {
        Self {
            config,
            rpc_fleet,
            dashboard,
            capital,
            missed: Arc::new(Mutex::new(MissedOpportunityTracker::default())),
            pnl: Arc::new(Mutex::new(PnlTracker::default())),
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

        let Some(payload) = opportunity.execution_payload.clone() else {
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

        if payload.tx.0.is_empty() {
            self.dashboard.event(
                "warn",
                format!(
                    "live MEV blocked victim={:?}: payload has no signed tx bytes",
                    opportunity.victim_tx
                ),
            );
            self.record_miss(MissReason::PayloadUnavailable);
            return Ok(());
        }

        let endpoint = self.rpc_fleet.send_endpoint();
        let provider = endpoint.provider.clone();
        let wallet = self
            .config
            .sender_private_key
            .parse::<LocalWallet>()?
            .with_chain_id(self.config.chain_id);
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
            return Ok(());
        }
        let bundle = BundleRequest::new()
            .set_block(block + 1)
            .push_transaction(payload.tx.clone());

        {
            let mut capital = self.capital.lock().expect("capital manager lock");
            capital.reserve_execution(opportunity.score.execution_cost_wei);
        }

        let started = std::time::Instant::now();
        let bundle_result = flashbots.send_bundle(&bundle).await;
        match bundle_result {
            Ok(pending) => {
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
                Err(err.into())
            }
        }
    }

    fn record_miss(&self, reason: MissReason) {
        if let Ok(mut missed) = self.missed.lock() {
            missed.record(reason);
        }
        self.dashboard
            .event("info", format!("missed_opportunity reason={}", reason.as_str()));
    }
}

#[allow(dead_code)]
fn gas_with_safety_margin(config: &Config, gas_price: U256) -> U256 {
    gas_price.saturating_mul(U256::from(config.mev.gas_safety_margin_bps)) / U256::from(10_000u64)
}

#[allow(dead_code)]
fn empty_tx_for_payload_guard(_: &TypedTransaction) {}
