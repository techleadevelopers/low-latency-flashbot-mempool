use clap::Parser;
use dotenvy::dotenv;
use ethers::prelude::*;
use ethers::types::transaction::eip2930::AccessList;
use ethers::utils::{keccak256, rlp::RlpStream};
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "predelegate-7702")]
#[command(
    about = "Provisiona wallets com delegacao EIP-7702 apontando para um contrato ja deployado"
)]
struct Cli {
    #[arg(long, default_value = "keys.txt")]
    wallets: PathBuf,

    #[arg(long)]
    rpc_url: String,

    #[arg(long)]
    chain_id: u64,

    #[arg(long)]
    delegate_contract: Address,

    #[arg(long)]
    sponsor_private_key: String,

    #[arg(long, default_value_t = 200_000)]
    gas_limit: u64,
}

#[derive(Clone)]
struct Eip7702Authorization {
    chain_id: U256,
    delegate_address: Address,
    nonce: U256,
    signature: Signature,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv().ok();
    let cli = Cli::parse();

    let provider = Provider::<Http>::try_from(cli.rpc_url.as_str())?;
    let provider = std::sync::Arc::new(provider);
    let sponsor_wallet = normalize_private_key(&cli.sponsor_private_key)?
        .parse::<LocalWallet>()?
        .with_chain_id(cli.chain_id);
    let wallets = load_wallets(&cli.wallets, cli.chain_id)?;

    println!("wallets read: {}", wallets.len());
    println!("delegate contract: {:?}", cli.delegate_contract);
    println!("sponsor: {:?}", sponsor_wallet.address());
    println!("chain id: {}", cli.chain_id);

    let gas_price = bumped_gas_price(provider.get_gas_price().await?);
    println!("gas price: {} wei", gas_price);

    for wallet in wallets {
        let target_nonce = provider
            .get_transaction_count(wallet.address(), Some(pending_block_id()))
            .await?;
        let sponsor_nonce = provider
            .get_transaction_count(sponsor_wallet.address(), Some(pending_block_id()))
            .await?;

        let auth = build_eip7702_authorization(
            &wallet,
            cli.chain_id,
            cli.delegate_contract,
            target_nonce,
        )?;
        let raw_tx = build_eip7702_install_tx(
            &sponsor_wallet,
            sponsor_nonce,
            gas_price,
            cli.gas_limit,
            sponsor_wallet.address(),
            &[auth],
        )?;

        println!(
            "sending install for wallet {:?} with target nonce {} and sponsor nonce {}",
            wallet.address(),
            target_nonce,
            sponsor_nonce
        );
        let pending = provider.send_raw_transaction(raw_tx).await?;
        let tx_hash = pending.tx_hash();
        let receipt = pending.await?;

        match receipt {
            Some(receipt) if receipt.status == Some(U64::from(1u64)) => {
                let code = provider.get_code(wallet.address(), None).await?;
                println!(
                    "ok wallet={:?} tx={:?} block={} delegated_code_present={}",
                    wallet.address(),
                    tx_hash,
                    receipt.block_number.unwrap_or_default(),
                    !code.as_ref().is_empty()
                );
            }
            Some(receipt) => {
                println!(
                    "failed wallet={:?} tx={:?} status={:?}",
                    wallet.address(),
                    tx_hash,
                    receipt.status
                );
            }
            None => {
                println!("pending wallet={:?} tx={:?}", wallet.address(), tx_hash);
            }
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

fn build_eip7702_authorization(
    wallet: &LocalWallet,
    chain_id: u64,
    delegate_address: Address,
    nonce: U256,
) -> Result<Eip7702Authorization, Box<dyn std::error::Error>> {
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
) -> Result<Bytes, Box<dyn std::error::Error>> {
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
    append_eip7702_authorization_list(&mut unsigned, authorizations);

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
    append_eip7702_authorization_list(&mut signed, authorizations);
    signed.append(&outer_y_parity);
    signed.append(&outer_sig.r);
    signed.append(&outer_sig.s);

    let mut encoded = vec![0x04];
    encoded.extend_from_slice(signed.out().as_ref());
    Ok(Bytes::from(encoded))
}

fn append_eip7702_authorization_list(rlp: &mut RlpStream, authorizations: &[Eip7702Authorization]) {
    rlp.begin_list(authorizations.len());
    for auth in authorizations {
        let y_parity = signature_y_parity(&auth.signature).unwrap_or(0u8);
        rlp.begin_list(6);
        rlp.append(&auth.chain_id);
        rlp.append(&auth.delegate_address);
        rlp.append(&auth.nonce);
        rlp.append(&y_parity);
        rlp.append(&auth.signature.r);
        rlp.append(&auth.signature.s);
    }
}

fn signature_y_parity(signature: &Signature) -> Result<u8, Box<dyn std::error::Error>> {
    match signature.v {
        27 | 28 => Ok((signature.v - 27) as u8),
        0 | 1 => Ok(signature.v as u8),
        other => Err(format!("unsupported signature v for y_parity: {}", other).into()),
    }
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
