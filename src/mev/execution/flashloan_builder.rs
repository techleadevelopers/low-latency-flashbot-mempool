use crate::mev::execution::contract_encoder::{encode_start_v2_flash_swap, EncodedSwapStep};
use ethers::types::{Address, Bytes, U256};

#[derive(Debug, Clone)]
pub struct V2FlashSwapCall {
    pub target_contract: Address,
    pub calldata: Bytes,
    pub borrow_token: Address,
    pub borrow_amount: U256,
    pub min_profit: U256,
}

pub fn build_v2_flashswap_call(
    executor: Address,
    pair: Address,
    borrow_token: Address,
    borrow_amount: U256,
    min_profit: U256,
    profit_token: Address,
    steps: &[EncodedSwapStep],
) -> V2FlashSwapCall {
    V2FlashSwapCall {
        target_contract: executor,
        calldata: encode_start_v2_flash_swap(
            pair,
            borrow_token,
            borrow_amount,
            min_profit,
            profit_token,
            steps,
        ),
        borrow_token,
        borrow_amount,
        min_profit,
    }
}
