use crate::config::{Config, MonitoredTokenConfig};
use crate::dashboard::DashboardHandle;
use crate::mev::amm::uniswap_v2::V2PoolState;
use crate::mev::block_loop::LearningRuntime;
use crate::mev::capital::CapitalManager;
use crate::mev::execution::payload_builder::{BackrunBuildInput, PayloadBuilder};
use crate::mev::execution::ExecutionEngine;
use crate::mev::meta_decision::{
    MetaDecision, MetaDecisionConfig, MetaDecisionEngine, MetaOpportunity,
};
use crate::mev::opportunity::{
    clamp_score, roi_bps, wei_to_eth_f64, MevOpportunity, OpportunityKind, OpportunityScore,
};
use crate::rpc::RpcFleet;
use ethers::abi::{self, ParamType, Token};
use ethers::providers::{Middleware, Provider, StreamExt, Ws};
use ethers::types::{Address, Transaction, U256};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing::{debug, warn};

const SWAP_EXACT_TOKENS_FOR_TOKENS: [u8; 4] = [0x38, 0xed, 0x17, 0x39];
const SWAP_EXACT_ETH_FOR_TOKENS: [u8; 4] = [0x7f, 0xf3, 0x6a, 0xb5];
const SWAP_EXACT_TOKENS_FOR_ETH: [u8; 4] = [0x18, 0xcb, 0xaf, 0xe5];
const SWAP_EXACT_TOKENS_FOR_TOKENS_SUPPORTING_FEE: [u8; 4] = [0x5c, 0x11, 0xd7, 0x95];
const SWAP_EXACT_ETH_FOR_TOKENS_SUPPORTING_FEE: [u8; 4] = [0xb6, 0xf9, 0xde, 0x95];
const SWAP_EXACT_TOKENS_FOR_ETH_SUPPORTING_FEE: [u8; 4] = [0x79, 0x1a, 0xc9, 0x47];

#[derive(Debug, Clone)]
struct SwapSignal {
    selector: [u8; 4],
    amount_in: U256,
    notional_wei: U256,
    path: Vec<Address>,
    router: Address,
}

ethers::contract::abigen!(
    UniswapV2Factory,
    r#"[
        function getPair(address tokenA, address tokenB) external view returns (address pair)
    ]"#,
);

ethers::contract::abigen!(
    UniswapV2Pair,
    r#"[
        function token0() external view returns (address)
        function token1() external view returns (address)
        function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast)
    ]"#,
);

pub async fn run(
    config: Arc<Config>,
    rpc_fleet: Arc<RpcFleet>,
    dashboard: DashboardHandle,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(ws_url) = config.mempool_ws_url() else {
        let message = "MEV backrun enabled but no websocket URL is available".to_string();
        dashboard.event("warn", message.clone());
        return Err(message.into());
    };

    let min_profit_wei = ethers::utils::parse_ether(config.mev.min_net_profit_eth.to_string())?;
    let min_large_swap_wei = ethers::utils::parse_ether(config.mev.min_large_swap_eth.to_string())?;
    let ws = Ws::connect(ws_url.clone()).await?;
    let provider = Arc::new(Provider::new(ws));
    let mut stream = provider.subscribe_pending_txs().await?;
    let capital = Arc::new(Mutex::new(CapitalManager::from_config(&config.mev)?));
    let learning_runtime = LearningRuntime::new(&config);
    let pending_learning = learning_runtime.clone();
    let executor = ExecutionEngine::new(
        config.clone(),
        rpc_fleet.clone(),
        dashboard.clone(),
        capital,
    )
    .with_learning_runtime(learning_runtime.clone());
    let loop_config = config.clone();
    let loop_fleet = rpc_fleet.clone();
    let loop_dashboard = dashboard.clone();
    tokio::spawn(async move {
        crate::mev::block_loop::run_block_loop(
            learning_runtime,
            loop_config,
            loop_fleet,
            loop_dashboard,
        )
        .await;
    });
    let meta_engine = MetaDecisionEngine::new(MetaDecisionConfig {
        profit_multiplier: 1.8,
        max_price_impact: config.mev.max_price_impact_bps as f64 / 10_000.0,
        min_liquidity: config.mev.min_liquidity_eth,
        max_slippage: config.mev.slippage_protection_bps as f64 / 10_000.0,
        max_competition_threshold: config.mev.max_competition_score as f64 / 100.0,
        min_real_profit: config.mev.min_profit_usd,
        execution_threshold: config.mev.min_confidence_score as f64 / 100.0,
        ..MetaDecisionConfig::default()
    });

    dashboard.event(
        "info",
        format!(
            "MEV backrun monitor connected to {} min_large_swap={:.3} ETH min_profit={:.6} ETH",
            ws_url, config.mev.min_large_swap_eth, config.mev.min_net_profit_eth
        ),
    );

    while let Some(tx_hash) = stream.next().await {
        let lookup_started = Instant::now();
        let tx = match provider.get_transaction(tx_hash).await {
            Ok(Some(tx)) => tx,
            Ok(None) => continue,
            Err(err) => {
                warn!("pending tx lookup failed for {:?}: {}", tx_hash, err);
                continue;
            }
        };
        pending_learning.observe_pending_transaction(&tx);
        dashboard.record_latency(
            "mev_pending_lookup",
            lookup_started.elapsed().as_millis(),
            None,
            None,
        );

        let Some(signal) = decode_large_swap(&tx, &config.monitored_tokens, min_large_swap_wei)
        else {
            continue;
        };

        let gas_price = tx
            .max_fee_per_gas
            .or(tx.gas_price)
            .unwrap_or_else(U256::zero);
        if gas_price.is_zero() {
            debug!(
                "MEV backrun candidate skipped {:?}: missing gas price",
                tx.hash
            );
            continue;
        }

        let Some(mut opportunity) =
            score_backrun(signal.clone(), &tx, gas_price, &config, min_profit_wei)
        else {
            continue;
        };

        if !opportunity.score.passes(
            min_profit_wei,
            config.mev.min_roi_bps,
            config.mev.max_risk_score,
            config.mev.max_competition_score,
            config.mev.min_confidence_score,
        ) {
            if config.hot_path_info_events {
                dashboard.event(
                    "info",
                    format!(
                        "MEV backrun rejected victim={:?} profit={:.6} ETH roi={}bps confidence={} risk={} competition={}",
                        opportunity.victim_tx,
                        wei_to_eth_f64(opportunity.score.slippage_adjusted_profit_wei),
                        opportunity.score.roi_bps,
                        opportunity.score.confidence_score,
                        opportunity.score.risk_score,
                        opportunity.score.competition_score
                    ),
                );
            }
            continue;
        }

        let meta_opportunity = MetaOpportunity {
            expected_profit: wei_to_eth_f64(opportunity.score.slippage_adjusted_profit_wei)
                * config.mev.eth_usd_price,
            gas_cost: wei_to_eth_f64(opportunity.score.execution_cost_wei)
                * config.mev.eth_usd_price,
            slippage_estimate: config.mev.slippage_protection_bps as f64 / 10_000.0,
            price_impact: opportunity.score.risk_score as f64 / 10_000.0,
            liquidity_depth: wei_to_eth_f64(signal.notional_wei) * config.mev.eth_usd_price,
            latency_estimate_ms: lookup_started.elapsed().as_secs_f64() * 1_000.0,
            pool_address: signal.router,
            victim_tx_hash: Some(tx.hash),
            mempool_density: (opportunity.score.competition_score as f64 / 100.0).min(1.0),
            tx_popularity: if wei_to_eth_f64(signal.notional_wei) >= 250.0 {
                0.8
            } else {
                0.35
            },
            pool_activity_recent: (opportunity.score.risk_score as f64 / 100.0).min(1.0),
            historical_failure_rate: 0.10,
            block_delay_ms: 0.0,
            simulation_age_ms: opportunity.age_ms() as f64,
            victim_confirmed_or_replaced: false,
            capital_gas_window_remaining: config.mev.max_gas_spend_window_eth
                * config.mev.eth_usd_price,
            daily_drawdown_remaining: config.mev.max_daily_loss_eth * config.mev.eth_usd_price,
            trade_allocation_remaining: config.mev.capital_eth
                * (config.mev.max_allocation_bps as f64 / 10_000.0)
                * config.mev.eth_usd_price,
        };
        match meta_engine.decide(meta_opportunity) {
            MetaDecision::Execute { score, .. } => {
                if config.hot_path_info_events {
                    dashboard.event(
                        "info",
                        format!(
                            "meta gate accepted victim={:?} ev={:.4} confidence={:.3} competition={:.3} realistic_profit={:.4}",
                            tx.hash,
                            score.expected_value,
                            score.confidence_score,
                            score.competition_score,
                            score.realistic_profit
                        ),
                    );
                }
            }
            MetaDecision::Skip(reason) => {
                dashboard.event(
                    "info",
                    format!(
                        "meta gate skipped victim={:?} reason={}",
                        tx.hash,
                        reason.as_str()
                    ),
                );
                continue;
            }
        }

        if let Some(payload) = build_v2_payload_if_possible(
            provider.clone(),
            signal,
            gas_price,
            &config,
            opportunity.score.execution_cost_wei,
        )
        .await
        {
            opportunity.score.expected_profit_wei = payload.expected_profit_wei;
            opportunity.score.slippage_adjusted_profit_wei = payload.simulated_profit_wei;
            opportunity.execution_payload = Some(payload);
        } else if matches!(config.bot_mode, crate::config::BotMode::Live) {
            dashboard.event(
                "info",
                format!(
                    "MEV backrun live rejected victim={:?}: no deterministic V2 payload",
                    tx.hash
                ),
            );
            continue;
        }

        executor.handle(opportunity).await?;
    }

    Ok(())
}

fn decode_large_swap(
    tx: &Transaction,
    monitored_tokens: &[MonitoredTokenConfig],
    min_large_swap_wei: U256,
) -> Option<SwapSignal> {
    let selector = selector(tx)?;
    let router = tx.to?;
    let args = &tx.input.as_ref()[4..];

    let mut signal = match selector {
        SWAP_EXACT_ETH_FOR_TOKENS | SWAP_EXACT_ETH_FOR_TOKENS_SUPPORTING_FEE => {
            let decoded = abi::decode(
                &[
                    ParamType::Uint(256),
                    ParamType::Array(Box::new(ParamType::Address)),
                    ParamType::Address,
                    ParamType::Uint(256),
                ],
                args,
            )
            .ok()?;
            SwapSignal {
                selector,
                amount_in: tx.value,
                notional_wei: tx.value,
                path: decoded.get(1).and_then(token_as_address_vec)?,
                router,
            }
        }
        SWAP_EXACT_TOKENS_FOR_TOKENS
        | SWAP_EXACT_TOKENS_FOR_ETH
        | SWAP_EXACT_TOKENS_FOR_TOKENS_SUPPORTING_FEE
        | SWAP_EXACT_TOKENS_FOR_ETH_SUPPORTING_FEE => {
            let decoded = abi::decode(
                &[
                    ParamType::Uint(256),
                    ParamType::Uint(256),
                    ParamType::Array(Box::new(ParamType::Address)),
                    ParamType::Address,
                    ParamType::Uint(256),
                ],
                args,
            )
            .ok()?;
            SwapSignal {
                selector,
                amount_in: decoded.first().and_then(token_as_uint)?,
                notional_wei: U256::zero(),
                path: decoded.get(2).and_then(token_as_address_vec)?,
                router,
            }
        }
        _ => return None,
    };

    let notional_wei = estimate_notional_wei(&signal, monitored_tokens)?;
    if notional_wei < min_large_swap_wei {
        return None;
    }

    signal.notional_wei = notional_wei;
    Some(signal)
}

fn score_backrun(
    signal: SwapSignal,
    tx: &Transaction,
    gas_price: U256,
    config: &Config,
    min_profit_wei: U256,
) -> Option<MevOpportunity> {
    if signal.path.len() < 2 {
        return None;
    }

    let gas_limit = config
        .estimated_exec_gas
        .saturating_add(config.estimated_bundle_overhead_gas)
        .max(180_000);
    let execution_cost_wei = gas_price
        .saturating_mul(U256::from(gas_limit))
        .saturating_mul(U256::from(config.mev.gas_safety_margin_bps))
        / U256::from(10_000u64);

    let notional_eth = wei_to_eth_f64(signal.notional_wei);
    let residual_edge_bps = residual_edge_bps(signal.selector, notional_eth);
    let gross_profit_wei = signal
        .notional_wei
        .saturating_mul(U256::from(residual_edge_bps))
        / U256::from(10_000u64);
    let expected_profit_wei = gross_profit_wei.saturating_sub(execution_cost_wei);
    let slippage_adjusted_profit_wei =
        expected_profit_wei.saturating_mul(U256::from(8_000u64)) / U256::from(10_000u64);

    if slippage_adjusted_profit_wei < min_profit_wei / U256::from(2u64) {
        return None;
    }

    let competition_score = competition_score(notional_eth, tx);
    let risk_score = risk_score(notional_eth, competition_score, tx);
    let roi = roi_bps(slippage_adjusted_profit_wei, execution_cost_wei);
    let confidence_score = confidence_score(
        slippage_adjusted_profit_wei,
        execution_cost_wei,
        competition_score,
    );

    Some(MevOpportunity {
        id: format!("backrun:{:?}", tx.hash),
        kind: OpportunityKind::Backrun,
        detected_at: Instant::now(),
        victim_tx: tx.hash,
        victim_transaction: Some(tx.clone()),
        target: signal.router,
        input_token: signal.path[0],
        output_token: *signal.path.last()?,
        notional_wei: signal.notional_wei,
        gas_limit,
        private_only: true,
        score: OpportunityScore {
            expected_profit_wei,
            execution_cost_wei,
            slippage_adjusted_profit_wei,
            roi_bps: roi,
            risk_score,
            competition_score,
            confidence_score,
        },
        execution_payload: None,
    })
}

async fn build_v2_payload_if_possible(
    provider: Arc<Provider<Ws>>,
    signal: SwapSignal,
    gas_price: U256,
    config: &Config,
    _estimated_cost_wei: U256,
) -> Option<crate::mev::execution::payload_builder::ExecutionPayload> {
    let factory = config.mev.uniswap_v2_factory?;
    let recipient = config
        .mev
        .searcher_recipient
        .unwrap_or(config.control_address);
    let token_in = *signal.path.first()?;
    let token_out = *signal.path.get(1)?;
    let factory = UniswapV2Factory::new(factory, provider.clone());
    let pair = factory.get_pair(token_in, token_out).call().await.ok()?;
    if pair == Address::zero() {
        return None;
    }
    let pair_contract = UniswapV2Pair::new(pair, provider.clone());
    let token0 = pair_contract.token_0().call().await.ok()?;
    let token1 = pair_contract.token_1().call().await.ok()?;
    let reserves = pair_contract.get_reserves().call().await.ok()?;
    let pool = V2PoolState {
        pair,
        token0,
        token1,
        reserve0: U256::from(reserves.0),
        reserve1: U256::from(reserves.1),
        fee_bps: 30,
    };
    let capital_available_wei =
        ethers::utils::parse_ether(config.mev.capital_eth.to_string()).ok()?;
    PayloadBuilder::build_backrun_v2(
        config,
        BackrunBuildInput {
            router: signal.router,
            pair,
            recipient,
            token_in,
            token_out,
            victim_amount_in: signal.amount_in,
            state_before: crate::mev::simulation::state_simulator::AmmState::UniswapV2(pool),
            capital_available_wei,
            gas_price_wei: gas_price,
        },
    )
    .ok()
}

fn estimate_notional_wei(
    signal: &SwapSignal,
    monitored_tokens: &[MonitoredTokenConfig],
) -> Option<U256> {
    if matches!(
        signal.selector,
        SWAP_EXACT_ETH_FOR_TOKENS | SWAP_EXACT_ETH_FOR_TOKENS_SUPPORTING_FEE
    ) {
        return Some(signal.amount_in);
    }

    let input = signal.path.first()?;
    let token = monitored_tokens
        .iter()
        .find(|token| token.address == *input)?;
    let decimals_factor = 10f64.powi(i32::from(token.decimals));
    let normalized = signal.amount_in.to_string().parse::<f64>().ok()? / decimals_factor;
    let value_eth = normalized * token.price_eth;
    ethers::utils::parse_ether(value_eth.to_string()).ok()
}

fn residual_edge_bps(selector: [u8; 4], notional_eth: f64) -> u64 {
    let base = if matches!(
        selector,
        SWAP_EXACT_ETH_FOR_TOKENS_SUPPORTING_FEE
            | SWAP_EXACT_TOKENS_FOR_TOKENS_SUPPORTING_FEE
            | SWAP_EXACT_TOKENS_FOR_ETH_SUPPORTING_FEE
    ) {
        4
    } else if notional_eth >= 250.0 {
        12
    } else if notional_eth >= 100.0 {
        9
    } else {
        6
    };
    base
}

fn competition_score(notional_eth: f64, tx: &Transaction) -> u16 {
    let gas_gwei = tx
        .max_priority_fee_per_gas
        .or(tx.gas_price)
        .map(wei_to_eth_f64)
        .unwrap_or(0.0)
        * 1e9;
    let notional_component = if notional_eth >= 500.0 {
        45
    } else if notional_eth >= 150.0 {
        30
    } else if notional_eth >= 50.0 {
        20
    } else {
        10
    };
    let gas_component = if gas_gwei >= 10.0 {
        20
    } else if gas_gwei >= 3.0 {
        10
    } else {
        0
    };
    clamp_score(notional_component + gas_component)
}

fn risk_score(notional_eth: f64, competition: u16, tx: &Transaction) -> u16 {
    let pathless_penalty = if tx.input.len() < 4 { 40 } else { 0 };
    let size_penalty = if notional_eth >= 500.0 { 20 } else { 0 };
    clamp_score(i64::from(competition / 2) + size_penalty + pathless_penalty)
}

fn confidence_score(profit_wei: U256, cost_wei: U256, competition: u16) -> u16 {
    let roi = roi_bps(profit_wei, cost_wei);
    let profit_eth = wei_to_eth_f64(profit_wei);
    let profit_component = if profit_eth >= 0.02 {
        35
    } else if profit_eth >= 0.01 {
        25
    } else {
        15
    };
    let roi_component = if roi >= 30_000 {
        40
    } else if roi >= 15_000 {
        30
    } else if roi >= 7_500 {
        20
    } else {
        5
    };
    clamp_score(profit_component + roi_component + 30 - i64::from(competition))
}

fn selector(tx: &Transaction) -> Option<[u8; 4]> {
    let input = tx.input.as_ref();
    if input.len() < 4 {
        return None;
    }
    Some([input[0], input[1], input[2], input[3]])
}

fn token_as_uint(token: &Token) -> Option<U256> {
    match token {
        Token::Uint(value) => Some(*value),
        _ => None,
    }
}

fn token_as_address(token: &Token) -> Option<Address> {
    match token {
        Token::Address(value) => Some(*value),
        _ => None,
    }
}

fn token_as_address_vec(token: &Token) -> Option<Vec<Address>> {
    match token {
        Token::Array(values) => values.iter().map(token_as_address).collect(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_penalizes_competition() {
        let high = confidence_score(U256::from(20_000u64), U256::from(1_000u64), 10);
        let low = confidence_score(U256::from(20_000u64), U256::from(1_000u64), 80);
        assert!(high > low);
    }

    #[test]
    fn edge_is_conservative_for_fee_on_transfer_swaps() {
        assert_eq!(
            residual_edge_bps(SWAP_EXACT_ETH_FOR_TOKENS_SUPPORTING_FEE, 300.0),
            4
        );
    }
}
