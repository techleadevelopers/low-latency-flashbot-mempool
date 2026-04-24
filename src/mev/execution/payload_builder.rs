use crate::config::Config;
use crate::mev::amm::uniswap_v2::{amount_out_exact_in, find_roi_optimal_input};
use crate::mev::execution::contract_encoder::EncodedSwapStep;
use crate::mev::execution::flashloan_builder::build_v2_flashswap_call;
use crate::mev::opportunity::wei_to_eth_f64;
use crate::mev::simulation::state_simulator::{AmmState, StateSimulator};
use ethers::types::{Address, Bytes, U256};

#[derive(Debug, Clone)]
pub struct ExecutionPayload {
    pub tx: Bytes,
    pub calldata: Bytes,
    pub target_contract: Address,
    pub value: U256,
    pub amount_in: U256,
    pub min_amount_out: U256,
    pub expected_profit_wei: U256,
    pub simulated_profit_wei: U256,
    pub expected_profit: f64,
    pub gas_estimate: u64,
    pub gas_limit: u64,
    pub price_impact_bps: u64,
    pub revert_risk: bool,
    pub pool_address: Address,
    pub token0: Address,
    pub token1: Address,
    pub trade_input_token: Address,
    pub trade_output_token: Address,
    pub profit_token: Address,
    pub profit_recipient: Address,
    pub expected_amount_out: U256,
    pub expected_execution_price: f64,
}

#[derive(Debug, Clone)]
pub struct BackrunBuildInput {
    pub router: Address,
    pub pair: Address,
    pub recipient: Address,
    pub token_in: Address,
    pub token_out: Address,
    pub victim_amount_in: U256,
    pub state_before: AmmState,
    pub capital_available_wei: U256,
    pub gas_price_wei: U256,
}

pub struct PayloadBuilder;

impl PayloadBuilder {
    pub fn build_backrun_v2(
        config: &Config,
        input: BackrunBuildInput,
    ) -> Result<ExecutionPayload, String> {
        let post_victim = StateSimulator::simulate_victim_exact_in(
            input.state_before,
            input.token_in,
            input.token_out,
            input.victim_amount_in,
        )
        .ok_or_else(|| "victim post-swap simulation failed".to_string())?;

        let AmmState::UniswapV2(pool_after) = post_victim.state_after else {
            return Err("build_backrun_v2 received non-v2 pool state".to_string());
        };

        if post_victim.slippage_impact_bps > config.mev.max_price_impact_bps {
            return Err(format!(
                "victim price impact too high: {}bps",
                post_victim.slippage_impact_bps
            ));
        }

        let (reserve_in, reserve_out) = pool_after
            .reserves_for(input.token_out, input.token_in)
            .ok_or_else(|| "pool after victim does not support reverse path".to_string())?;

        let gas_estimate = config.mev.max_gas_per_tx.min(
            config
                .estimated_exec_gas
                .saturating_add(config.estimated_bundle_overhead_gas)
                .max(180_000),
        );
        let gas_cost = input
            .gas_price_wei
            .saturating_mul(U256::from(gas_estimate))
            .saturating_mul(U256::from(config.mev.gas_safety_margin_bps))
            / U256::from(10_000u64);

        let (amount_in, simulated_profit_wei) = find_roi_optimal_input(
            reserve_in,
            reserve_out,
            input.capital_available_wei,
            gas_cost,
            pool_after.fee_bps,
        )
        .ok_or_else(|| "no ROI-positive trade size after gas".to_string())?;

        let amount_out =
            amount_out_exact_in(amount_in, reserve_in, reserve_out, pool_after.fee_bps)
                .ok_or_else(|| "backrun output quote failed".to_string())?;
        let min_amount_out = amount_out.saturating_mul(U256::from(
            10_000u64.saturating_sub(config.mev.slippage_protection_bps),
        )) / U256::from(10_000u64);
        let price_impact_bps = post_victim.slippage_impact_bps;
        let min_profit_wei = ethers::utils::parse_ether(config.mev.min_net_profit_eth.to_string())
            .map_err(|err| err.to_string())?;

        if simulated_profit_wei < min_profit_wei {
            return Err(format!(
                "simulated profit {:.6} ETH below minimum {:.6} ETH",
                wei_to_eth_f64(simulated_profit_wei),
                config.mev.min_net_profit_eth
            ));
        }

        let executor = config.mev.mev_executor.ok_or_else(|| {
            "MEV_EXECUTOR_ADDRESS is required to build atomic payload".to_string()
        })?;
        let step = EncodedSwapStep {
            router: input.router,
            path: vec![input.token_out, input.token_in],
            amount_in: U256::MAX,
            min_out: min_amount_out,
        };
        let call = build_v2_flashswap_call(
            executor,
            input.pair,
            input.token_out,
            amount_in,
            min_profit_wei,
            input.token_in,
            &[step],
        );

        Ok(ExecutionPayload {
            tx: Bytes::new(),
            calldata: call.calldata,
            target_contract: call.target_contract,
            value: U256::zero(),
            amount_in,
            min_amount_out,
            expected_profit_wei: simulated_profit_wei,
            simulated_profit_wei,
            expected_profit: wei_to_eth_f64(simulated_profit_wei),
            gas_estimate,
            gas_limit: gas_estimate,
            price_impact_bps,
            revert_risk: false,
            pool_address: input.pair,
            token0: pool_after.token0,
            token1: pool_after.token1,
            trade_input_token: input.token_out,
            trade_output_token: input.token_in,
            profit_token: input.token_in,
            profit_recipient: input.recipient,
            expected_amount_out: amount_out,
            expected_execution_price: {
                let amount_in_f = amount_in.to_string().parse::<f64>().unwrap_or(0.0);
                let amount_out_f = amount_out.to_string().parse::<f64>().unwrap_or(0.0);
                if amount_in_f > 0.0 {
                    amount_out_f / amount_in_f
                } else {
                    0.0
                }
            },
        })
    }
}
