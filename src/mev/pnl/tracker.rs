use crate::mev::opportunity::wei_to_eth_f64;
use ethers::providers::{Http, Middleware, Provider};
use ethers::types::{TransactionReceipt, TxHash, U256};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct ExecutionResult {
    pub expected_profit: f64,
    pub realized_profit: f64,
    pub gas_used: u64,
    pub success: bool,
}

#[derive(Debug, Default)]
pub struct PnlTracker {
    pub daily_pnl_eth: f64,
    pub realized_profit_eth: f64,
    pub realized_loss_eth: f64,
    pub executions: u64,
    pub failures: u64,
}

impl PnlTracker {
    pub fn record(&mut self, result: &ExecutionResult) {
        self.executions = self.executions.saturating_add(1);
        self.daily_pnl_eth += result.realized_profit;
        if result.success {
            self.realized_profit_eth += result.realized_profit.max(0.0);
        } else {
            self.failures = self.failures.saturating_add(1);
            self.realized_loss_eth += result.realized_profit.min(0.0).abs();
        }
    }

    pub async fn from_receipt(
        provider: Arc<Provider<Http>>,
        tx_hash: TxHash,
        expected_profit_wei: U256,
        tokens_received_wei: U256,
        tokens_spent_wei: U256,
    ) -> Result<Option<ExecutionResult>, Box<dyn std::error::Error>> {
        let Some(receipt) = provider.get_transaction_receipt(tx_hash).await? else {
            return Ok(None);
        };
        Ok(Some(result_from_receipt(
            &receipt,
            expected_profit_wei,
            tokens_received_wei,
            tokens_spent_wei,
        )))
    }
}

pub fn result_from_receipt(
    receipt: &TransactionReceipt,
    expected_profit_wei: U256,
    tokens_received_wei: U256,
    tokens_spent_wei: U256,
) -> ExecutionResult {
    let gas_used = receipt.gas_used.unwrap_or_default();
    let effective_gas_price = receipt.effective_gas_price.unwrap_or_default();
    let gas_paid = gas_used.saturating_mul(effective_gas_price);
    let gross = tokens_received_wei.saturating_sub(tokens_spent_wei);
    let realized = gross.saturating_sub(gas_paid);
    let success = receipt
        .status
        .map(|status| status.as_u64() == 1)
        .unwrap_or(false);

    ExecutionResult {
        expected_profit: wei_to_eth_f64(expected_profit_wei),
        realized_profit: if success {
            wei_to_eth_f64(realized)
        } else {
            -wei_to_eth_f64(gas_paid)
        },
        gas_used: gas_used.as_u64(),
        success,
    }
}
