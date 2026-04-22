use crate::config::Config;
use crate::mev::execution::payload_builder::ExecutionPayload;
use ethers::types::{Bytes, TxHash, U256};

#[derive(Debug, Clone)]
pub struct BundleSimulationRequest {
    pub victim_tx: TxHash,
    pub payload: ExecutionPayload,
    pub target_block: u64,
}

#[derive(Debug, Clone)]
pub struct BundleSimulationResult {
    pub simulation_success: bool,
    pub realized_profit_wei: U256,
    pub gas_used: u64,
    pub revert_risk: bool,
    pub reason: Option<String>,
}

pub struct BundleSimulator;

impl BundleSimulator {
    pub fn deterministic_preflight(
        config: &Config,
        request: &BundleSimulationRequest,
        min_profit_wei: U256,
    ) -> BundleSimulationResult {
        if request.payload.tx.0.is_empty() {
            return Self::reject("missing signed transaction bytes");
        }
        if request.payload.expected_profit_wei < min_profit_wei {
            return Self::reject("expected profit below threshold");
        }
        if request.payload.gas_estimate > config.mev.max_gas_per_tx {
            return Self::reject("gas estimate exceeds max gas per tx");
        }
        if request.payload.simulated_profit_wei < min_profit_wei {
            return Self::reject("simulated realized profit below threshold");
        }
        if request.payload.revert_risk {
            return Self::reject("payload marked as revert risk");
        }

        BundleSimulationResult {
            simulation_success: true,
            realized_profit_wei: request.payload.simulated_profit_wei,
            gas_used: request.payload.gas_estimate,
            revert_risk: false,
            reason: None,
        }
    }

    pub fn simulated_signed_bytes(raw: Vec<u8>) -> Bytes {
        Bytes::from(raw)
    }

    fn reject(reason: &str) -> BundleSimulationResult {
        BundleSimulationResult {
            simulation_success: false,
            realized_profit_wei: U256::zero(),
            gas_used: 0,
            revert_risk: true,
            reason: Some(reason.to_string()),
        }
    }
}
