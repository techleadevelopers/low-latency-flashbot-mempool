use crate::mev::amm::uniswap_v2::{V2PoolState, V2SwapResult};
use crate::mev::amm::uniswap_v3::{V3PoolState, V3SwapResult};
use ethers::types::{Address, U256};

#[derive(Debug, Clone)]
pub enum AmmState {
    UniswapV2(V2PoolState),
    UniswapV3(V3PoolState),
}

#[derive(Debug, Clone)]
pub enum AmmSwapResult {
    UniswapV2(V2SwapResult),
    UniswapV3(V3SwapResult),
}

#[derive(Debug, Clone)]
pub struct PostSwapSimulation {
    pub state_after: AmmState,
    pub result: AmmSwapResult,
    pub effective_price_x18: U256,
    pub slippage_impact_bps: u64,
}

pub struct StateSimulator;

impl StateSimulator {
    pub fn simulate_victim_exact_in(
        state: AmmState,
        token_in: Address,
        token_out: Address,
        amount_in: U256,
    ) -> Option<PostSwapSimulation> {
        match state {
            AmmState::UniswapV2(pool) => {
                let (next, result) = pool.apply_swap_exact_in(token_in, token_out, amount_in)?;
                Some(PostSwapSimulation {
                    state_after: AmmState::UniswapV2(next),
                    effective_price_x18: effective_price_x18(result.amount_in, result.amount_out),
                    slippage_impact_bps: result.price_impact_bps,
                    result: AmmSwapResult::UniswapV2(result),
                })
            }
            AmmState::UniswapV3(pool) => {
                let (next, result) = pool.simulate_exact_in(token_in, token_out, amount_in)?;
                Some(PostSwapSimulation {
                    state_after: AmmState::UniswapV3(next),
                    effective_price_x18: effective_price_x18(result.amount_in, result.amount_out),
                    slippage_impact_bps: result.price_impact_bps,
                    result: AmmSwapResult::UniswapV3(result),
                })
            }
        }
    }
}

fn effective_price_x18(amount_in: U256, amount_out: U256) -> U256 {
    if amount_out.is_zero() {
        return U256::zero();
    }
    amount_in.saturating_mul(U256::exp10(18)) / amount_out
}
