use clap::Parser;
use dotenvy::dotenv;
use ethers::prelude::*;
use ethers::types::transaction::eip2718::TypedTransaction;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

abigen!(
    ERC20ApproveToken,
    r#"[
        function approve(address spender, uint256 amount) external returns (bool)
        function allowance(address owner, address spender) external view returns (uint256)
    ]"#
);

#[derive(Parser, Debug)]
#[command(name = "preapprove-spender")]
#[command(about = "Provisiona approvals ERC-20 de fallback para o contrato spender")]
struct Cli {
    #[arg(long, default_value = "keys.txt")]
    wallets: PathBuf,

    #[arg(long)]
    rpc_url: String,

    #[arg(long)]
    chain_id: u64,

    #[arg(long)]
    spender_contract: Address,

    #[arg(long)]
    sponsor_private_key: String,

    #[arg(
        long,
        help = "Override opcional do nonce pendente inicial da wallet-alvo"
    )]
    wallet_nonce: Option<u64>,

    #[arg(
        long = "token",
        help = "Formato: SYMBOL:ADDRESS:AMOUNT, onde AMOUNT pode ser numero decimal bruto ou 'max'"
    )]
    tokens: Vec<String>,
}

#[derive(Clone)]
struct TokenApprovalSpec {
    symbol: String,
    address: Address,
    amount: U256,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv().ok();
    let cli = Cli::parse();

    if cli.tokens.is_empty() {
        return Err("at least one --token SYMBOL:ADDRESS:AMOUNT is required".into());
    }

    let provider = Provider::<Http>::try_from(cli.rpc_url.as_str())?;
    let provider = std::sync::Arc::new(provider);
    let sponsor_wallet = normalize_private_key(&cli.sponsor_private_key)?
        .parse::<LocalWallet>()?
        .with_chain_id(cli.chain_id);
    let wallets = load_wallets(&cli.wallets, cli.chain_id)?;
    let token_specs = cli
        .tokens
        .iter()
        .map(|raw| parse_token_spec(raw))
        .collect::<Result<Vec<_>, _>>()?;

    let gas_price = bumped_gas_price(provider.get_gas_price().await?);

    println!("wallets read: {}", wallets.len());
    println!("spender contract: {:?}", cli.spender_contract);
    println!("sponsor: {:?}", sponsor_wallet.address());
    println!("chain id: {}", cli.chain_id);
    println!("gas price: {} wei", gas_price);

    for wallet in wallets {
        let sponsor_nonce = provider
            .get_transaction_count(sponsor_wallet.address(), Some(pending_block_id()))
            .await?;
        let mut nonce = match cli.wallet_nonce {
            Some(value) => U256::from(value),
            None => {
                provider
                    .get_transaction_count(wallet.address(), Some(pending_block_id()))
                    .await?
            }
        };
        println!(
            "wallet {:?} starting nonce {} sponsor nonce {}",
            wallet.address(),
            nonce,
            sponsor_nonce
        );

        let wallet_balance = provider.get_balance(wallet.address(), None).await?;
        let needed_gas_units = U256::from((token_specs.len() as u64) * 65_000u64);
        let needed_funding = gas_price.saturating_mul(needed_gas_units);

        if wallet_balance < needed_funding {
            let top_up = needed_funding.saturating_sub(wallet_balance);
            let fund_gas = provider
                .estimate_gas(
                    &TransactionRequest::new()
                        .to(wallet.address())
                        .value(top_up)
                        .from(sponsor_wallet.address())
                        .into(),
                    None,
                )
                .await
                .unwrap_or_else(|_| U256::from(100_000u64))
                .max(U256::from(50_000u64));
            let fund_tx: TypedTransaction = TransactionRequest::new()
                .to(wallet.address())
                .value(top_up)
                .gas(fund_gas)
                .gas_price(gas_price)
                .nonce(sponsor_nonce)
                .from(sponsor_wallet.address())
                .into();
            let fund_sig = sponsor_wallet.sign_transaction(&fund_tx).await?;
            let fund_raw = fund_tx.rlp_signed(&fund_sig);
            let pending = provider.send_raw_transaction(fund_raw).await?;
            let tx_hash = pending.tx_hash();
            let receipt = pending.await?;

            match receipt {
                Some(receipt) if receipt.status == Some(U64::from(1u64)) => {
                    println!(
                        "funded wallet={:?} tx={:?} sponsor_nonce={} amount_wei={}",
                        wallet.address(),
                        tx_hash,
                        sponsor_nonce,
                        top_up
                    );
                }
                Some(receipt) => {
                    return Err(format!(
                        "funding failed for wallet {:?} tx {:?} status {:?}",
                        wallet.address(),
                        tx_hash,
                        receipt.status
                    )
                    .into());
                }
                None => {
                    return Err(format!(
                        "funding receipt missing for wallet {:?} tx {:?}",
                        wallet.address(),
                        tx_hash
                    )
                    .into());
                }
            }
        }

        nonce = match cli.wallet_nonce {
            Some(value) => U256::from(value),
            None => {
                provider
                    .get_transaction_count(wallet.address(), Some(pending_block_id()))
                    .await?
            }
        };
        println!(
            "wallet {:?} refreshed pending nonce {} before approvals",
            wallet.address(),
            nonce
        );

        for token in &token_specs {
            let contract = ERC20ApproveToken::new(token.address, provider.clone());
            let current_allowance = contract
                .allowance(wallet.address(), cli.spender_contract)
                .call()
                .await
                .unwrap_or_default();

            if current_allowance >= token.amount {
                println!(
                    "skip wallet={:?} token={} allowance={} already >= target={}",
                    wallet.address(),
                    token.symbol,
                    current_allowance,
                    token.amount
                );
                continue;
            }

            let wallet_balance = provider.get_balance(wallet.address(), None).await?;
            let gas_price = bumped_gas_price(provider.get_gas_price().await?);
            let approve_gas_cost = gas_price.saturating_mul(U256::from(65_000u64));
            if wallet_balance < approve_gas_cost {
                let top_up = approve_gas_cost
                    .saturating_sub(wallet_balance)
                    .saturating_add(gas_price.saturating_mul(U256::from(25_000u64)));
                let fund_gas = provider
                    .estimate_gas(
                        &TransactionRequest::new()
                            .to(wallet.address())
                            .value(top_up)
                            .from(sponsor_wallet.address())
                            .into(),
                        None,
                    )
                    .await
                    .unwrap_or_else(|_| U256::from(100_000u64))
                    .max(U256::from(50_000u64));
                let sponsor_nonce = provider
                    .get_transaction_count(sponsor_wallet.address(), Some(pending_block_id()))
                    .await?;
                let fund_tx: TypedTransaction = TransactionRequest::new()
                    .to(wallet.address())
                    .value(top_up)
                    .gas(fund_gas)
                    .gas_price(gas_price)
                    .nonce(sponsor_nonce)
                    .from(sponsor_wallet.address())
                    .into();
                let fund_sig = sponsor_wallet.sign_transaction(&fund_tx).await?;
                let fund_raw = fund_tx.rlp_signed(&fund_sig);
                let pending = provider.send_raw_transaction(fund_raw).await?;
                let tx_hash = pending.tx_hash();
                let receipt = pending.await?;

                match receipt {
                    Some(receipt) if receipt.status == Some(U64::from(1u64)) => {
                        println!(
                            "refunded wallet={:?} token={} tx={:?} sponsor_nonce={} amount_wei={}",
                            wallet.address(),
                            token.symbol,
                            tx_hash,
                            sponsor_nonce,
                            top_up
                        );
                    }
                    Some(receipt) => {
                        return Err(format!(
                            "refunding failed for wallet {:?} token {} tx {:?} status {:?}",
                            wallet.address(),
                            token.symbol,
                            tx_hash,
                            receipt.status
                        )
                        .into());
                    }
                    None => {
                        return Err(format!(
                            "refunding receipt missing for wallet {:?} token {} tx {:?}",
                            wallet.address(),
                            token.symbol,
                            tx_hash
                        )
                        .into());
                    }
                }
            }

            let approve_call = contract.approve(cli.spender_contract, token.amount);
            let approve_data = approve_call
                .calldata()
                .ok_or("failed to build approve calldata")?;
            nonce = match cli.wallet_nonce {
                Some(_) => nonce,
                None => {
                    provider
                        .get_transaction_count(wallet.address(), Some(pending_block_id()))
                        .await?
                }
            };
            let gas_price = bumped_gas_price(provider.get_gas_price().await?);
            let tx: TypedTransaction = TransactionRequest::new()
                .to(token.address)
                .data(approve_data)
                .value(U256::zero())
                .gas(65_000u64)
                .gas_price(gas_price)
                .nonce(nonce)
                .from(wallet.address())
                .into();
            let signature = wallet.sign_transaction(&tx).await?;
            let raw_tx = tx.rlp_signed(&signature);

            let pending = provider.send_raw_transaction(raw_tx).await?;
            let tx_hash = pending.tx_hash();
            let receipt = pending.await?;

            match receipt {
                Some(receipt) if receipt.status == Some(U64::from(1u64)) => {
                    let new_allowance = contract
                        .allowance(wallet.address(), cli.spender_contract)
                        .call()
                        .await
                        .unwrap_or_default();
                    println!(
                        "ok wallet={:?} token={} tx={:?} nonce={} new_allowance={}",
                        wallet.address(),
                        token.symbol,
                        tx_hash,
                        nonce,
                        new_allowance
                    );
                }
                Some(receipt) => {
                    println!(
                        "failed wallet={:?} token={} tx={:?} nonce={} status={:?}",
                        wallet.address(),
                        token.symbol,
                        tx_hash,
                        nonce,
                        receipt.status
                    );
                }
                None => {
                    println!(
                        "pending wallet={:?} token={} tx={:?} nonce={}",
                        wallet.address(),
                        token.symbol,
                        tx_hash,
                        nonce
                    );
                }
            }

            nonce = match cli.wallet_nonce {
                Some(_) => nonce.saturating_add(U256::one()),
                None => {
                    provider
                        .get_transaction_count(wallet.address(), Some(pending_block_id()))
                        .await?
                }
            };
        }
    }

    Ok(())
}

fn pending_block_id() -> BlockId {
    BlockId::Number(BlockNumber::Pending)
}

fn bumped_gas_price(base: U256) -> U256 {
    let bump_bps = U256::from(10_500u64);
    let bumped = base.saturating_mul(bump_bps) / U256::from(10_000u64);
    bumped.max(base.saturating_add(U256::from(1_000_000u64)))
}

fn parse_token_spec(raw: &str) -> Result<TokenApprovalSpec, Box<dyn std::error::Error>> {
    let parts: Vec<&str> = raw.split(':').map(str::trim).collect();
    if parts.len() != 3 {
        return Err(format!(
            "invalid token spec '{}', expected SYMBOL:ADDRESS:AMOUNT",
            raw
        )
        .into());
    }

    let amount = if parts[2].eq_ignore_ascii_case("max") {
        U256::MAX
    } else {
        U256::from_dec_str(parts[2])?
    };

    Ok(TokenApprovalSpec {
        symbol: parts[0].to_string(),
        address: parts[1].parse::<Address>()?,
        amount,
    })
}

fn load_wallets(
    path: &PathBuf,
    chain_id: u64,
) -> Result<Vec<LocalWallet>, Box<dyn std::error::Error>> {
    let content = fs::read_to_string(path)?;
    let mut seen = HashSet::new();
    let mut wallets = Vec::new();

    for line in content.lines() {
        let normalized = match normalize_private_key(line) {
            Ok(value) => value,
            Err(_) => continue,
        };

        if !seen.insert(normalized.clone()) {
            continue;
        }

        wallets.push(normalized.parse::<LocalWallet>()?.with_chain_id(chain_id));
    }

    Ok(wallets)
}

fn normalize_private_key(value: &str) -> Result<String, Box<dyn std::error::Error>> {
    let trimmed = value.trim().strip_prefix("0x").unwrap_or(value.trim());
    if trimmed.len() != 64 || !trimmed.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err("invalid private key".into());
    }
    Ok(format!("0x{}", trimmed.to_lowercase()))
}
