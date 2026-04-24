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
    ensure_allowlisted, load_address_allowlist, load_wallets, normalize_private_key,
    parse_checksummed_address, parse_h256, parse_wei, ExecutionMode,
};
use ethers::prelude::*;
use ethers::types::transaction::eip2930::AccessList;
use ethers::utils::{keccak256, rlp::RlpStream};
use gas::{gwei_to_wei, GasPolicy};
use guards::{
    ensure_max_executions, ensure_value_cap, mode_allows_execution, not_applicable_profit,
    CooldownGuard,
};
use serde_json::json;
use std::error::Error;
use std::path::PathBuf;
use validation::{code_hash, ensure_contract_code, extract_eip7702_delegate};

#[derive(Parser, Debug)]
#[command(name = "predelegate")]
#[command(about = "Deterministic one-shot EIP-7702 delegation installer")]
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
    delegate_contract: String,

    #[arg(long)]
    delegate_allowlist: PathBuf,

    #[arg(long)]
    sponsor_private_key: String,

    #[arg(long, value_enum, default_value_t = ExecutionMode::DryRun)]
    mode: ExecutionMode,

    #[arg(long, default_value = "0")]
    max_value_per_tx_wei: String,

    #[arg(long, default_value_t = 50u64)]
    max_gas_price_gwei: u64,

    #[arg(long, default_value_t = 250_000u64)]
    max_gas_limit: u64,

    #[arg(long, default_value_t = 200_000u64)]
    gas_limit: u64,

    #[arg(long, default_value_t = 1usize)]
    max_executions_per_run: usize,

    #[arg(long)]
    cooldown_per_wallet_seconds: Option<u64>,

    #[arg(long)]
    cooldown_state_file: Option<PathBuf>,

    #[arg(long)]
    expected_code_hash: Option<String>,

    #[arg(long)]
    target_nonce: Option<u64>,
}

#[derive(Clone)]
struct Eip7702Authorization {
    chain_id: U256,
    delegate_address: Address,
    nonce: U256,
    signature: Signature,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();

    let provider = Provider::<Http>::try_from(cli.rpc_url.as_str())?;
    let delegate_allowlist = load_address_allowlist(&cli.delegate_allowlist)?;
    let delegate_contract = parse_checksummed_address(&cli.delegate_contract, "delegate contract")?;
    ensure_allowlisted("delegate contract", delegate_contract, &delegate_allowlist)?;

    let delegate_code =
        ensure_contract_code(&provider, delegate_contract, "delegate contract").await?;
    let observed_hash = code_hash(&delegate_code);
    if let Some(expected_hash_raw) = cli.expected_code_hash.as_deref() {
        let expected_hash = parse_h256(expected_hash_raw, "expected code")?;
        if observed_hash != expected_hash {
            return Err(format!(
                "delegate contract code hash mismatch: expected {:?}, got {:?}",
                expected_hash, observed_hash
            )
            .into());
        }
    }

    let sponsor_wallet = normalize_private_key(&cli.sponsor_private_key)?
        .parse::<LocalWallet>()?
        .with_chain_id(cli.chain_id);
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
        if let Some(guard) = cooldown_guard.as_ref() {
            if let Err(err) = guard.ensure_ready(wallet_address, cli.cooldown_per_wallet_seconds) {
                log_decision(
                    "SKIP",
                    &cli.mode,
                    wallet_address,
                    delegate_contract,
                    None,
                    err.to_string(),
                    None,
                    None,
                );
                continue;
            }
        }

        if let Err(err) = ensure_value_cap(U256::zero(), max_value_per_tx_wei) {
            log_decision(
                "SKIP",
                &cli.mode,
                wallet_address,
                delegate_contract,
                None,
                err.to_string(),
                None,
                None,
            );
            continue;
        }

        let target_nonce = match cli.target_nonce {
            Some(value) => U256::from(value),
            None => provider.get_transaction_count(wallet_address, None).await?,
        };
        let sponsor_nonce = provider
            .get_transaction_count(sponsor_wallet.address(), None)
            .await?;
        let gas_price = provider.get_gas_price().await?;
        if gas_price.is_zero() || gas_price > gas_policy.max_gas_price_wei {
            log_decision(
                "SKIP",
                &cli.mode,
                wallet_address,
                delegate_contract,
                None,
                format!(
                    "gas sanity check failed: gas price {} exceeds cap {}",
                    gas_price, gas_policy.max_gas_price_wei
                ),
                None,
                None,
            );
            continue;
        }
        if U256::from(cli.gas_limit) > gas_policy.max_gas_limit {
            log_decision(
                "SKIP",
                &cli.mode,
                wallet_address,
                delegate_contract,
                None,
                format!(
                    "gas sanity check failed: gas limit {} exceeds cap {}",
                    cli.gas_limit, gas_policy.max_gas_limit
                ),
                None,
                None,
            );
            continue;
        }

        let sponsor_balance = provider.get_balance(sponsor_wallet.address(), None).await?;
        let gas_limit = U256::from(cli.gas_limit);
        let gas_cost_wei = gas_price.saturating_mul(gas_limit);
        if sponsor_balance < gas_cost_wei {
            log_decision(
                "SKIP",
                &cli.mode,
                wallet_address,
                delegate_contract,
                Some((gas_price, gas_limit, gas_cost_wei)),
                "insufficient sponsor balance for gas".to_string(),
                None,
                None,
            );
            continue;
        }

        if let Err(err) = ensure_max_executions(executed, cli.max_executions_per_run) {
            log_decision(
                "SKIP",
                &cli.mode,
                wallet_address,
                delegate_contract,
                Some((gas_price, gas_limit, gas_cost_wei)),
                err.to_string(),
                None,
                None,
            );
            continue;
        }

        let auth =
            build_eip7702_authorization(&wallet, cli.chain_id, delegate_contract, target_nonce)?;
        let raw_tx = build_eip7702_install_tx(
            &sponsor_wallet,
            sponsor_nonce,
            gas_price,
            cli.gas_limit,
            sponsor_wallet.address(),
            &[auth],
        )?;

        if !mode_allows_execution(cli.mode) {
            log_decision(
                "SKIP",
                &cli.mode,
                wallet_address,
                delegate_contract,
                Some((gas_price, gas_limit, gas_cost_wei)),
                "dry-run mode".to_string(),
                None,
                Some(observed_hash),
            );
            continue;
        }

        let pending = provider.send_raw_transaction(raw_tx).await?;
        let tx_hash = pending.tx_hash();
        let receipt = pending.await?;
        match receipt {
            Some(receipt) if receipt.status == Some(U64::from(1u64)) => {
                let delegated_code = provider.get_code(wallet_address, None).await?;
                let installed_delegate = extract_eip7702_delegate(&delegated_code);
                if installed_delegate != Some(delegate_contract) {
                    return Err(format!(
                        "delegation verification failed for {:?}: expected {:?}, got {:?}",
                        wallet_address, delegate_contract, installed_delegate
                    )
                    .into());
                }

                executed += 1;
                if let Some(guard) = cooldown_guard.as_mut() {
                    guard.record_execution(wallet_address)?;
                }
                log_decision(
                    "EXECUTE",
                    &cli.mode,
                    wallet_address,
                    delegate_contract,
                    Some((gas_price, gas_limit, gas_cost_wei)),
                    "delegation installed".to_string(),
                    Some(tx_hash),
                    Some(observed_hash),
                );
            }
            Some(receipt) => {
                return Err(format!(
                    "delegation transaction {:?} failed with status {:?}",
                    tx_hash, receipt.status
                )
                .into());
            }
            None => {
                return Err(format!("delegation transaction {:?} receipt missing", tx_hash).into());
            }
        }
    }

    Ok(())
}

fn build_eip7702_authorization(
    wallet: &LocalWallet,
    chain_id: u64,
    delegate_address: Address,
    nonce: U256,
) -> Result<Eip7702Authorization, Box<dyn Error>> {
    let chain_id = U256::from(chain_id);
    let mut auth_payload = RlpStream::new_list(3);
    auth_payload.append(&chain_id);
    auth_payload.append(&delegate_address);
    auth_payload.append(&nonce);

    let mut preimage = vec![0x05];
    preimage.extend_from_slice(auth_payload.out().as_ref());

    let signature = wallet.sign_hash(H256::from(keccak256(preimage)))?;
    Ok(Eip7702Authorization {
        chain_id,
        delegate_address,
        nonce,
        signature,
    })
}

fn build_eip7702_install_tx(
    sponsor_wallet: &LocalWallet,
    sponsor_nonce: U256,
    gas_price: U256,
    gas_limit: u64,
    destination: Address,
    authorizations: &[Eip7702Authorization],
) -> Result<Bytes, Box<dyn Error>> {
    let chain_id = U256::from(sponsor_wallet.chain_id());
    let gas_limit = U256::from(gas_limit);
    let access_list = AccessList::default();
    let data: &[u8] = &[];

    let mut unsigned = RlpStream::new_list(10);
    unsigned.append(&chain_id);
    unsigned.append(&sponsor_nonce);
    unsigned.append(&gas_price);
    unsigned.append(&gas_price);
    unsigned.append(&gas_limit);
    unsigned.append(&destination);
    unsigned.append(&U256::zero());
    unsigned.append(&data);
    unsigned.append(&access_list);
    append_authorization_list(&mut unsigned, authorizations)?;

    let mut sighash_preimage = vec![0x04];
    sighash_preimage.extend_from_slice(unsigned.out().as_ref());
    let outer_sig = sponsor_wallet.sign_hash(H256::from(keccak256(sighash_preimage)))?;
    let outer_y_parity = signature_y_parity(&outer_sig)?;

    let mut signed = RlpStream::new_list(13);
    signed.append(&chain_id);
    signed.append(&sponsor_nonce);
    signed.append(&gas_price);
    signed.append(&gas_price);
    signed.append(&gas_limit);
    signed.append(&destination);
    signed.append(&U256::zero());
    signed.append(&data);
    signed.append(&access_list);
    append_authorization_list(&mut signed, authorizations)?;
    signed.append(&outer_y_parity);
    signed.append(&outer_sig.r);
    signed.append(&outer_sig.s);

    let mut encoded = vec![0x04];
    encoded.extend_from_slice(signed.out().as_ref());
    Ok(Bytes::from(encoded))
}

fn append_authorization_list(
    rlp: &mut RlpStream,
    authorizations: &[Eip7702Authorization],
) -> Result<(), Box<dyn Error>> {
    rlp.begin_list(authorizations.len());
    for auth in authorizations {
        let y_parity = signature_y_parity(&auth.signature)?;
        rlp.begin_list(6);
        rlp.append(&auth.chain_id);
        rlp.append(&auth.delegate_address);
        rlp.append(&auth.nonce);
        rlp.append(&y_parity);
        rlp.append(&auth.signature.r);
        rlp.append(&auth.signature.s);
    }
    Ok(())
}

fn signature_y_parity(signature: &Signature) -> Result<u8, Box<dyn Error>> {
    match signature.v {
        27 | 28 => Ok((signature.v - 27) as u8),
        0 | 1 => Ok(signature.v as u8),
        other => Err(format!("unsupported signature v value: {}", other).into()),
    }
}

fn log_decision(
    action: &str,
    mode: &ExecutionMode,
    wallet: Address,
    delegate_contract: Address,
    gas: Option<(U256, U256, U256)>,
    reason: String,
    tx_hash: Option<H256>,
    delegate_code_hash: Option<H256>,
) {
    let profit = not_applicable_profit();
    println!(
        "{}",
        json!({
            "tool": "predelegate",
            "mode": mode.as_str(),
            "wallet": format!("{wallet:?}"),
            "token": "delegation",
            "delegate_contract": format!("{delegate_contract:?}"),
            "value_detected_wei": "0",
            "estimated_gas": gas.as_ref().map(|(_, gas_limit, _)| gas_limit.to_string()),
            "gas_price_wei": gas.as_ref().map(|(gas_price, _, _)| gas_price.to_string()),
            "gas_cost_wei": gas.as_ref().map(|(_, _, gas_cost)| gas_cost.to_string()),
            "net_profit_wei": profit.net_profit_wei.map(|value| value.to_string()),
            "roi_bps": profit.roi_bps,
            "action": action,
            "reason": reason,
            "tx_hash": tx_hash.map(|hash| format!("{hash:?}")),
            "delegate_code_hash": delegate_code_hash.map(|hash| format!("{hash:?}")),
        })
    );
}
