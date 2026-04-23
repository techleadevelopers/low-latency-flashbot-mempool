use crate::mev::capital::CapitalManager;
use crate::mev::execution::nonce_manager::NonceManager;
use crate::mev::execution::tx_lifecycle::{ManagedExecution, TxLifecycleManager};
use crate::mev::inclusion_truth::{BundleOutcome, InclusionTruth};

#[derive(Debug, Clone)]
pub struct FinalizedExecution {
    pub execution: ManagedExecution,
    pub capital_delta_wei: i128,
}

#[derive(Debug, Default)]
pub struct ExecutionFinalizer;

impl ExecutionFinalizer {
    pub fn finalize_truth(
        lifecycle: &mut TxLifecycleManager,
        nonce_manager: &mut NonceManager,
        capital: Option<&mut CapitalManager>,
        truth: &InclusionTruth,
    ) -> Option<FinalizedExecution> {
        let execution = lifecycle.finalize(truth.tx_hash, truth.outcome)?;
        nonce_manager.finalize(execution.wallet, execution.nonce);
        let capital_delta_wei = capital_delta_from_truth(truth);
        if let Some(capital) = capital {
            capital.record_pnl(capital_delta_wei);
        }
        Some(FinalizedExecution {
            execution,
            capital_delta_wei,
        })
    }
}

fn capital_delta_from_truth(truth: &InclusionTruth) -> i128 {
    match truth.outcome {
        BundleOutcome::Reverted => {
            -u256_to_i128_saturating(truth.gas_used.saturating_mul(truth.effective_gas_price))
        }
        BundleOutcome::Included
        | BundleOutcome::LateInclusion
        | BundleOutcome::Outbid
        | BundleOutcome::NotIncluded
        | BundleOutcome::Pending => 0,
    }
}

fn u256_to_i128_saturating(value: ethers::types::U256) -> i128 {
    if value > ethers::types::U256::from(i128::MAX as u128) {
        i128::MAX
    } else {
        value.as_u128() as i128
    }
}
