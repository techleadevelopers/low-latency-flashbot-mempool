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
    ensure_max_executions, ensure_value_cap, evaluate_profit, mode_allows_execution, CooldownGuard,
};
use serde_json::json;
use std::error::Error;
use std::path::PathBuf;
use validation::ensure_contract_code;

abigen!(
    CustodyErc20,
    r#"[
        function balanceOf(address owner) external view returns (uint256)
        function transfer(address to, uint256 amount) external returns (bool)
    ]"#
);

#[derive(Parser, Debug)]
#[command(name = "sweeper")]
#[command(about = "Deterministic one-shot custody sweeper")]
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
    control_address: String,

    #[arg(long)]
    control_allowlist: PathBuf,

    #[arg(
        long,
        default_value = "native",
        help = "Use 'native' or a checksummed ERC-20 address"
    )]
    token: String,

    #[arg(
        long,
        help = "Required for ERC-20 sweeps. Native-value quote used for profit guards."
    )]
    quoted_value_wei: Option<String>,

    #[arg(long, help = "Required for ERC-20 sweeps.")]
    token_allowlist: Option<PathBuf>,

    #[arg(long, value_enum, default_value_t = ExecutionMode::DryRun)]
    mode: ExecutionMode,

    #[arg(long, default_value = "100000000000000000")]
    max_value_per_tx_wei: String,

    #[arg(long, default_value = "1000000000000000")]
    min_net_profit_wei: String,

    #[arg(long, default_value_t = 500u64)]
    min_roi_bps: u64,

    #[arg(long, default_value_t = 50u64)]
    max_gas_price_gwei: u64,

    #[arg(long, default_value_t = 150_000u64)]
    max_gas_limit: u64,

    #[arg(long, default_value_t = 1usize)]
    max_executions_per_run: usize,

    #[arg(long)]
    cooldown_per_wallet_seconds: Option<u64>,

    #[arg(long)]
    cooldown_state_file: Option<PathBuf>,
}

enum SweepAsset {
    Native,
    Erc20(Address),
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();

    let provider = Provider::<Http>::try_from(cli.rpc_url.as_str())?;
    let control_allowlist = load_address_allowlist(&cli.control_allowlist)?;
    let control_address = parse_checksummed_address(&cli.control_address, "control address")?;
    ensure_allowlisted("control address", control_address, &control_allowlist)?;

    let asset = parse_asset(&cli)?;
    let wallets = load_wallets(&cli.wallet_keys, cli.wallets_file.as_ref(), cli.chain_id)?;
    let mut cooldown_guard = CooldownGuard::load(cli.cooldown_state_file.as_ref())?;

    let max_value_per_tx_wei = parse_wei(&cli.max_value_per_tx_wei, "max_value_per_tx_wei")?;
    let min_net_profit_wei = parse_wei(&cli.min_net_profit_wei, "min_net_profit_wei")?;
    let gas_policy = GasPolicy {
        max_gas_price_wei: gwei_to_wei(cli.max_gas_price_gwei),
        max_gas_limit: U256::from(cli.max_gas_limit),
    };

    let mut executed = 0usize;

    for wallet in wallets {
        let wallet_address = wallet.address();
        if let Some(guard) = cooldown_guard.as_ref() {
            if let Err(err) = guard.ensure_ready(wallet_address, cli.cooldown_per_wallet_seconds) {
                log_decision(
                    "SKIP",
                    &cli.mode,
                    wallet_address,
                    cli.token.as_str(),
                    Some(control_address),
                    None,
                    None,
                    None,
                    None,
                    err.to_string(),
                    None,
                );
                continue;
            }
        }

        match asset {
            SweepAsset::Native => {
                process_native_wallet(
                    &provider,
                    &wallet,
                    control_address,
                    &cli,
                    &gas_policy,
                    max_value_per_tx_wei,
                    min_net_profit_wei,
                    &mut executed,
                    cooldown_guard.as_mut(),
                )
                .await?;
            }
            SweepAsset::Erc20(token_address) => {
                process_erc20_wallet(
                    &provider,
                    &wallet,
                    token_address,
                    control_address,
                    &cli,
                    &gas_policy,
                    max_value_per_tx_wei,
                    min_net_profit_wei,
                    &mut executed,
                    cooldown_guard.as_mut(),
                )
                .await?;
            }
        }
    }

    Ok(())
}

async fn process_native_wallet(
    provider: &Provider<Http>,
    wallet: &LocalWallet,
    control_address: Address,
    cli: &Cli,
    gas_policy: &GasPolicy,
    max_value_per_tx_wei: U256,
    min_net_profit_wei: U256,
    executed: &mut usize,
    cooldown_guard: Option<&mut CooldownGuard>,
) -> Result<(), Box<dyn Error>> {
    let wallet_address = wallet.address();
    let balance = provider.get_balance(wallet_address, None).await?;
    let nonce = provider.get_transaction_count(wallet_address, None).await?;

    let probe_tx: TypedTransaction = TransactionRequest::new()
        .from(wallet_address)
        .to(control_address)
        .value(U256::one())
        .nonce(nonce)
        .into();

    let gas_estimate = match estimate_legacy_transaction(provider, &probe_tx, gas_policy).await {
        Ok(estimate) => estimate,
        Err(err) => {
            log_decision(
                "SKIP",
                &cli.mode,
                wallet_address,
                "native",
                Some(control_address),
                Some(balance),
                None,
                None,
                None,
                err.to_string(),
                None,
            );
            return Ok(());
        }
    };

    if balance <= gas_estimate.gas_cost_wei {
        log_decision(
            "SKIP",
            &cli.mode,
            wallet_address,
            "native",
            Some(control_address),
            Some(balance),
            Some(&gas_estimate),
            None,
            None,
            "insufficient native balance after gas".to_string(),
            None,
        );
        return Ok(());
    }

    let sweep_value = balance.saturating_sub(gas_estimate.gas_cost_wei);
    if let Err(err) = ensure_value_cap(sweep_value, max_value_per_tx_wei) {
        log_decision(
            "SKIP",
            &cli.mode,
            wallet_address,
            "native",
            Some(control_address),
            Some(sweep_value),
            Some(&gas_estimate),
            None,
            None,
            err.to_string(),
            None,
        );
        return Ok(());
    }

    let profit = match evaluate_profit(
        sweep_value,
        gas_estimate.gas_cost_wei,
        min_net_profit_wei,
        cli.min_roi_bps,
    ) {
        Ok(outcome) => outcome,
        Err(err) => {
            log_decision(
                "SKIP",
                &cli.mode,
                wallet_address,
                "native",
                Some(control_address),
                Some(sweep_value),
                Some(&gas_estimate),
                None,
                None,
                err.to_string(),
                None,
            );
            return Ok(());
        }
    };

    if let Err(err) = ensure_max_executions(*executed, cli.max_executions_per_run) {
        log_decision(
            "SKIP",
            &cli.mode,
            wallet_address,
            "native",
            Some(control_address),
            Some(sweep_value),
            Some(&gas_estimate),
            profit.net_profit_wei,
            profit.roi_bps,
            err.to_string(),
            None,
        );
        return Ok(());
    }

    if !mode_allows_execution(cli.mode) {
        log_decision(
            "SKIP",
            &cli.mode,
            wallet_address,
            "native",
            Some(control_address),
            Some(sweep_value),
            Some(&gas_estimate),
            profit.net_profit_wei,
            profit.roi_bps,
            "dry-run mode".to_string(),
            None,
        );
        return Ok(());
    }

    let tx: TypedTransaction = TransactionRequest::new()
        .from(wallet_address)
        .to(control_address)
        .value(sweep_value)
        .gas(gas_estimate.gas_limit)
        .gas_price(gas_estimate.gas_price_wei)
        .nonce(nonce)
        .into();
    let tx_hash = send_signed_transaction(provider, wallet, &tx).await?;
    *executed += 1;
    if let Some(guard) = cooldown_guard {
        guard.record_execution(wallet_address)?;
    }

    log_decision(
        "EXECUTE",
        &cli.mode,
        wallet_address,
        "native",
        Some(control_address),
        Some(sweep_value),
        Some(&gas_estimate),
        profit.net_profit_wei,
        profit.roi_bps,
        "native sweep executed".to_string(),
        Some(tx_hash),
    );

    Ok(())
}

async fn process_erc20_wallet(
    provider: &Provider<Http>,
    wallet: &LocalWallet,
    token_address: Address,
    control_address: Address,
    cli: &Cli,
    gas_policy: &GasPolicy,
    max_value_per_tx_wei: U256,
    min_net_profit_wei: U256,
    executed: &mut usize,
    cooldown_guard: Option<&mut CooldownGuard>,
) -> Result<(), Box<dyn Error>> {
    let wallet_address = wallet.address();
    ensure_contract_code(provider, token_address, "token").await?;
    let contract = CustodyErc20::new(token_address, std::sync::Arc::new(provider.clone()));
    let token_balance = contract.balance_of(wallet_address).call().await?;
    if token_balance.is_zero() {
        log_decision(
            "SKIP",
            &cli.mode,
            wallet_address,
            &format!("{token_address:?}"),
            Some(control_address),
            Some(U256::zero()),
            None,
            None,
            None,
            "token balance is zero".to_string(),
            None,
        );
        return Ok(());
    }

    let quoted_value_wei = parse_wei(
        cli.quoted_value_wei
            .as_deref()
            .ok_or("ERC-20 sweeps require --quoted-value-wei")?,
        "quoted_value_wei",
    )?;
    if let Err(err) = ensure_value_cap(quoted_value_wei, max_value_per_tx_wei) {
        log_decision(
            "SKIP",
            &cli.mode,
            wallet_address,
            &format!("{token_address:?}"),
            Some(control_address),
            Some(quoted_value_wei),
            None,
            None,
            None,
            err.to_string(),
            None,
        );
        return Ok(());
    }

    let nonce = provider.get_transaction_count(wallet_address, None).await?;
    let calldata = contract
        .transfer(control_address, token_balance)
        .calldata()
        .ok_or("failed to build ERC-20 transfer calldata")?;
    let tx: TypedTransaction = TransactionRequest::new()
        .from(wallet_address)
        .to(token_address)
        .data(calldata)
        .value(U256::zero())
        .nonce(nonce)
        .into();

    let gas_estimate = match estimate_legacy_transaction(provider, &tx, gas_policy).await {
        Ok(estimate) => estimate,
        Err(err) => {
            log_decision(
                "SKIP",
                &cli.mode,
                wallet_address,
                &format!("{token_address:?}"),
                Some(control_address),
                Some(quoted_value_wei),
                None,
                None,
                None,
                err.to_string(),
                None,
            );
            return Ok(());
        }
    };

    let native_balance = provider.get_balance(wallet_address, None).await?;
    if native_balance < gas_estimate.gas_cost_wei {
        log_decision(
            "SKIP",
            &cli.mode,
            wallet_address,
            &format!("{token_address:?}"),
            Some(control_address),
            Some(quoted_value_wei),
            Some(&gas_estimate),
            None,
            None,
            "insufficient native balance for ERC-20 gas".to_string(),
            None,
        );
        return Ok(());
    }

    let profit = match evaluate_profit(
        quoted_value_wei,
        gas_estimate.gas_cost_wei,
        min_net_profit_wei,
        cli.min_roi_bps,
    ) {
        Ok(outcome) => outcome,
        Err(err) => {
            log_decision(
                "SKIP",
                &cli.mode,
                wallet_address,
                &format!("{token_address:?}"),
                Some(control_address),
                Some(quoted_value_wei),
                Some(&gas_estimate),
                None,
                None,
                err.to_string(),
                None,
            );
            return Ok(());
        }
    };

    if let Err(err) = ensure_max_executions(*executed, cli.max_executions_per_run) {
        log_decision(
            "SKIP",
            &cli.mode,
            wallet_address,
            &format!("{token_address:?}"),
            Some(control_address),
            Some(quoted_value_wei),
            Some(&gas_estimate),
            profit.net_profit_wei,
            profit.roi_bps,
            err.to_string(),
            None,
        );
        return Ok(());
    }

    if !mode_allows_execution(cli.mode) {
        log_decision(
            "SKIP",
            &cli.mode,
            wallet_address,
            &format!("{token_address:?}"),
            Some(control_address),
            Some(quoted_value_wei),
            Some(&gas_estimate),
            profit.net_profit_wei,
            profit.roi_bps,
            "dry-run mode".to_string(),
            None,
        );
        return Ok(());
    }

    let final_tx: TypedTransaction = TransactionRequest::new()
        .from(wallet_address)
        .to(token_address)
        .data(
            contract
                .transfer(control_address, token_balance)
                .calldata()
                .ok_or("failed to rebuild ERC-20 transfer calldata")?,
        )
        .value(U256::zero())
        .gas(gas_estimate.gas_limit)
        .gas_price(gas_estimate.gas_price_wei)
        .nonce(nonce)
        .into();
    let tx_hash = send_signed_transaction(provider, wallet, &final_tx).await?;
    *executed += 1;
    if let Some(guard) = cooldown_guard {
        guard.record_execution(wallet_address)?;
    }

    log_decision(
        "EXECUTE",
        &cli.mode,
        wallet_address,
        &format!("{token_address:?}"),
        Some(control_address),
        Some(quoted_value_wei),
        Some(&gas_estimate),
        profit.net_profit_wei,
        profit.roi_bps,
        format!("erc20 sweep executed; token_amount={token_balance}"),
        Some(tx_hash),
    );

    Ok(())
}

fn parse_asset(cli: &Cli) -> Result<SweepAsset, Box<dyn Error>> {
    if cli.token.eq_ignore_ascii_case("native") {
        return Ok(SweepAsset::Native);
    }

    let token_address = parse_checksummed_address(&cli.token, "token")?;
    let token_allowlist = cli
        .token_allowlist
        .as_ref()
        .ok_or("ERC-20 sweeps require --token-allowlist")?;
    let allowlist = load_address_allowlist(token_allowlist)?;
    ensure_allowlisted("token", token_address, &allowlist)?;
    Ok(SweepAsset::Erc20(token_address))
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
    token: &str,
    destination: Option<Address>,
    value_detected_wei: Option<U256>,
    gas_estimate: Option<&gas::GasEstimate>,
    net_profit_wei: Option<U256>,
    roi_bps: Option<u64>,
    reason: String,
    tx_hash: Option<H256>,
) {
    println!(
        "{}",
        json!({
            "tool": "sweeper",
            "mode": mode.as_str(),
            "wallet": format!("{wallet:?}"),
            "token": token,
            "destination": destination.map(|address| format!("{address:?}")),
            "value_detected_wei": value_detected_wei.map(|value| value.to_string()),
            "estimated_gas": gas_estimate.map(|estimate| estimate.gas_limit.to_string()),
            "gas_price_wei": gas_estimate.map(|estimate| estimate.gas_price_wei.to_string()),
            "gas_cost_wei": gas_estimate.map(|estimate| estimate.gas_cost_wei.to_string()),
            "net_profit_wei": net_profit_wei.map(|value| value.to_string()),
            "roi_bps": roi_bps,
            "action": action,
            "reason": reason,
            "tx_hash": tx_hash.map(|hash| format!("{hash:?}")),
        })
    );
}
