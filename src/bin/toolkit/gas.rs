#![allow(dead_code)]

use ethers::prelude::*;
use ethers::types::transaction::eip2718::TypedTransaction;
use std::error::Error;

#[derive(Clone, Debug)]
pub struct GasPolicy {
    pub max_gas_price_wei: U256,
    pub max_gas_limit: U256,
}

#[derive(Clone, Debug)]
pub struct GasEstimate {
    pub gas_price_wei: U256,
    pub gas_limit: U256,
    pub gas_cost_wei: U256,
}

pub fn gwei_to_wei(gwei: u64) -> U256 {
    U256::from(gwei).saturating_mul(U256::exp10(9))
}

pub async fn estimate_legacy_transaction(
    provider: &Provider<Http>,
    tx: &TypedTransaction,
    policy: &GasPolicy,
) -> Result<GasEstimate, Box<dyn Error>> {
    let gas_price_wei = provider.get_gas_price().await?;
    if gas_price_wei.is_zero() {
        return Err("gas sanity check failed: gas price is zero".into());
    }
    if gas_price_wei > policy.max_gas_price_wei {
        return Err(format!(
            "gas sanity check failed: gas price {} exceeds cap {}",
            gas_price_wei, policy.max_gas_price_wei
        )
        .into());
    }

    let gas_limit = provider.estimate_gas(tx, None).await?;
    if gas_limit.is_zero() {
        return Err("gas sanity check failed: estimated gas is zero".into());
    }
    if gas_limit > policy.max_gas_limit {
        return Err(format!(
            "gas sanity check failed: gas limit {} exceeds cap {}",
            gas_limit, policy.max_gas_limit
        )
        .into());
    }

    Ok(GasEstimate {
        gas_cost_wei: gas_price_wei.saturating_mul(gas_limit),
        gas_limit,
        gas_price_wei,
    })
}
