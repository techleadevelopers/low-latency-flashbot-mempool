#[path = "toolkit/config.rs"]
mod config;
#[path = "toolkit/gas.rs"]
mod gas;
#[path = "toolkit/guards.rs"]
mod guards;
#[path = "toolkit/validation.rs"]
mod validation;

use clap::Parser;
use config::{
    ensure_allowlisted, load_address_allowlist, load_wallets, parse_checksummed_address, parse_wei,
    ExecutionMode,
};
use ethers::prelude::*;
use ethers::types::transaction::eip2718::TypedTransaction;
use gas::{estimate_legacy_transaction, gwei_to_wei, GasPolicy};
use guards::{
    ensure_max_executions, ensure_value_cap, mode_allows_execution, not_applicable_profit,
    CooldownGuard,
};
use serde_json::json;
use std::collections::HashSet;
use std::error::Error;
use std::path::PathBuf;
use validation::ensure_contract_code;

abigen!(
    ApprovalErc20,
    r#"[
        function approve(address spender, uint256 amount) external returns (bool)
        function allowance(address owner, address spender) external view returns (uint256)
    ]"#
);

#[derive(Parser, Debug)]
#[command(name = "preapprove")]
#[command(about = "Deterministic one-shot ERC-20 approval provisioner")]
struct Cli {
    #[arg(long)]
    rpc_url: String,

    #[arg(long)]
    chain_id: u64,

    #[arg(long = "wallet-key")]
    wallet_keys: Vec<String>,

    #[arg(long)]
    wallets_file: Option<PathBuf>,

    #[arg(long)]
    spender_address: String,

    #[arg(long)]
    spender_allowlist: PathBuf,

    #[arg(long)]
    token_allowlist: PathBuf,

    #[arg(
        long = "token",
        help = "Format: SYMBOL:CHECKSUMMED_TOKEN_ADDRESS:AMOUNT_WEI or SYMBOL:CHECKSUMMED_TOKEN_ADDRESS:max"
    )]
    tokens: Vec<String>,

    #[arg(long, value_enum, default_value_t = ExecutionMode::DryRun)]
    mode: ExecutionMode,

    #[arg(long, default_value = "100000000000000000")]
    max_value_per_tx_wei: String,

    #[arg(long, default_value_t = 50u64)]
    max_gas_price_gwei: u64,

    #[arg(long, default_value_t = 120_000u64)]
    max_gas_limit: u64,

    #[arg(long, default_value_t = 1usize)]
    max_executions_per_run: usize,

    #[arg(long)]
    cooldown_per_wallet_seconds: Option<u64>,

    #[arg(long)]
    cooldown_state_file: Option<PathBuf>,

    #[arg(long, default_value_t = false)]
    allow_unlimited_approval: bool,
}

#[derive(Clone)]
struct TokenApprovalSpec {
    symbol: String,
    address: Address,
    amount: U256,
    is_unlimited: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    if cli.tokens.is_empty() {
        return Err("at least one --token specification is required".into());
    }

    let provider = Provider::<Http>::try_from(cli.rpc_url.as_str())?;
    let spender_allowlist = load_address_allowlist(&cli.spender_allowlist)?;
    let token_allowlist = load_address_allowlist(&cli.token_allowlist)?;
    let spender_address = parse_checksummed_address(&cli.spender_address, "spender address")?;
    ensure_allowlisted("spender address", spender_address, &spender_allowlist)?;

    let token_specs = cli
        .tokens
        .iter()
        .map(|raw| parse_token_spec(raw, &token_allowlist, cli.allow_unlimited_approval))
        .collect::<Result<Vec<_>, _>>()?;
    let wallets = load_wallets(&cli.wallet_keys, cli.wallets_file.as_ref(), cli.chain_id)?;
    let mut cooldown_guard = CooldownGuard::load(cli.cooldown_state_file.as_ref())?;

    let max_value_per_tx_wei = parse_wei(&cli.max_value_per_tx_wei, "max_value_per_tx_wei")?;
    let gas_policy = GasPolicy {
        max_gas_price_wei: gwei_to_wei(cli.max_gas_price_gwei),
        max_gas_limit: U256::from(cli.max_gas_limit),
    };
    let mut executed = 0usize;

    for wallet in wallets {
        let wallet_address = wallet.address();
        for token in &token_specs {
            if let Some(guard) = cooldown_guard.as_ref() {
                if let Err(err) =
                    guard.ensure_ready(wallet_address, cli.cooldown_per_wallet_seconds)
                {
                    log_decision(
                        "SKIP",
                        &cli.mode,
                        wallet_address,
                        token,
                        spender_address,
                        None,
                        err.to_string(),
                        None,
                    );
                    continue;
                }
            }

            ensure_contract_code(&provider, token.address, "token").await?;
            let contract = ApprovalErc20::new(token.address, std::sync::Arc::new(provider.clone()));
            let current_allowance = contract
                .allowance(wallet_address, spender_address)
                .call()
                .await
                .unwrap_or_default();
            if current_allowance >= token.amount {
                log_decision(
                    "SKIP",
                    &cli.mode,
                    wallet_address,
                    token,
                    spender_address,
                    None,
                    "allowance already sufficient".to_string(),
                    None,
                );
                continue;
            }

            if !token.is_unlimited {
                if let Err(err) = ensure_value_cap(token.amount, max_value_per_tx_wei) {
                    log_decision(
                        "SKIP",
                        &cli.mode,
                        wallet_address,
                        token,
                        spender_address,
                        None,
                        err.to_string(),
                        None,
                    );
                    continue;
                }
            }

            let nonce = provider.get_transaction_count(wallet_address, None).await?;
            let calldata = contract
                .approve(spender_address, token.amount)
                .calldata()
                .ok_or("failed to build approval calldata")?;
            let tx: TypedTransaction = TransactionRequest::new()
                .from(wallet_address)
                .to(token.address)
                .data(calldata)
                .value(U256::zero())
                .nonce(nonce)
                .into();

            let gas_estimate = match estimate_legacy_transaction(&provider, &tx, &gas_policy).await
            {
                Ok(estimate) => estimate,
                Err(err) => {
                    log_decision(
                        "SKIP",
                        &cli.mode,
                        wallet_address,
                        token,
                        spender_address,
                        None,
                        err.to_string(),
                        None,
                    );
                    continue;
                }
            };

            let native_balance = provider.get_balance(wallet_address, None).await?;
            if native_balance < gas_estimate.gas_cost_wei {
                log_decision(
                    "SKIP",
                    &cli.mode,
                    wallet_address,
                    token,
                    spender_address,
                    Some(&gas_estimate),
                    "insufficient native balance for gas".to_string(),
                    None,
                );
                continue;
            }

            if let Err(err) = ensure_max_executions(executed, cli.max_executions_per_run) {
                log_decision(
                    "SKIP",
                    &cli.mode,
                    wallet_address,
                    token,
                    spender_address,
                    Some(&gas_estimate),
                    err.to_string(),
                    None,
                );
                continue;
            }

            if !mode_allows_execution(cli.mode) {
                log_decision(
                    "SKIP",
                    &cli.mode,
                    wallet_address,
                    token,
                    spender_address,
                    Some(&gas_estimate),
                    "dry-run mode".to_string(),
                    None,
                );
                continue;
            }

            let final_tx: TypedTransaction = TransactionRequest::new()
                .from(wallet_address)
                .to(token.address)
                .data(
                    contract
                        .approve(spender_address, token.amount)
                        .calldata()
                        .ok_or("failed to rebuild approval calldata")?,
                )
                .value(U256::zero())
                .gas(gas_estimate.gas_limit)
                .gas_price(gas_estimate.gas_price_wei)
                .nonce(nonce)
                .into();
            let tx_hash = send_signed_transaction(&provider, &wallet, &final_tx).await?;
            executed += 1;
            if let Some(guard) = cooldown_guard.as_mut() {
                guard.record_execution(wallet_address)?;
            }

            log_decision(
                "EXECUTE",
                &cli.mode,
                wallet_address,
                token,
                spender_address,
                Some(&gas_estimate),
                "approval executed".to_string(),
                Some(tx_hash),
            );
        }
    }

    Ok(())
}

fn parse_token_spec(
    raw: &str,
    token_allowlist: &HashSet<Address>,
    allow_unlimited_approval: bool,
) -> Result<TokenApprovalSpec, Box<dyn Error>> {
    let parts: Vec<&str> = raw.split(':').map(str::trim).collect();
    if parts.len() != 3 {
        return Err(format!(
            "invalid token spec '{raw}', expected SYMBOL:CHECKSUMMED_ADDRESS:AMOUNT"
        )
        .into());
    }

    let address = parse_checksummed_address(parts[1], "token")?;
    ensure_allowlisted("token", address, token_allowlist)?;

    let (amount, is_unlimited) = if parts[2].eq_ignore_ascii_case("max") {
        if !allow_unlimited_approval {
            return Err("unlimited approval requested without --allow-unlimited-approval".into());
        }
        (U256::MAX, true)
    } else {
        (parse_wei(parts[2], "approval amount")?, false)
    };

    Ok(TokenApprovalSpec {
        symbol: parts[0].to_string(),
        address,
        amount,
        is_unlimited,
    })
}

async fn send_signed_transaction(
    provider: &Provider<Http>,
    wallet: &LocalWallet,
    tx: &TypedTransaction,
) -> Result<H256, Box<dyn Error>> {
    let signature = wallet.sign_transaction(tx).await?;
    let raw_tx = tx.rlp_signed(&signature);
    let pending = provider.send_raw_transaction(raw_tx).await?;
    let tx_hash = pending.tx_hash();
    let receipt = pending.await?;
    match receipt {
        Some(receipt) if receipt.status == Some(U64::from(1u64)) => Ok(tx_hash),
        Some(receipt) => Err(format!(
            "transaction {:?} reverted with status {:?}",
            tx_hash, receipt.status
        )
        .into()),
        None => Err(format!("transaction {:?} receipt missing", tx_hash).into()),
    }
}

fn log_decision(
    action: &str,
    mode: &ExecutionMode,
    wallet: Address,
    token: &TokenApprovalSpec,
    spender_address: Address,
    gas_estimate: Option<&gas::GasEstimate>,
    reason: String,
    tx_hash: Option<H256>,
) {
    let profit = not_applicable_profit();
    println!(
        "{}",
        json!({
            "tool": "preapprove",
            "mode": mode.as_str(),
            "wallet": format!("{wallet:?}"),
            "token": token.symbol,
            "token_address": format!("{:?}", token.address),
            "spender": format!("{spender_address:?}"),
            "value_detected_wei": if token.is_unlimited { serde_json::Value::String("unlimited".to_string()) } else { serde_json::Value::String(token.amount.to_string()) },
            "estimated_gas": gas_estimate.map(|estimate| estimate.gas_limit.to_string()),
            "gas_price_wei": gas_estimate.map(|estimate| estimate.gas_price_wei.to_string()),
            "gas_cost_wei": gas_estimate.map(|estimate| estimate.gas_cost_wei.to_string()),
            "net_profit_wei": profit.net_profit_wei.map(|value| value.to_string()),
            "roi_bps": profit.roi_bps,
            "action": action,
            "reason": reason,
            "tx_hash": tx_hash.map(|hash| format!("{hash:?}")),
        })
    );
}
