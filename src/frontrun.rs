use crate::config::{BotMode, Config};
use crate::dashboard::DashboardHandle;
use ethers::abi::{self, ParamType, Token};
use ethers::middleware::SignerMiddleware;
use ethers::providers::{Middleware, Provider, StreamExt, Ws};
use ethers::signers::{LocalWallet, Signer};
use ethers::types::transaction::eip2718::TypedTransaction;
use ethers::types::{
    transaction::eip2718::TypedTransaction::Eip1559, Address, Bytes, NameOrAddress, Transaction,
    TxHash, U256, U64,
};
use ethers_flashbots::{BundleRequest, FlashbotsMiddleware};
use std::sync::Arc;
use tracing::{debug, info, warn};
use url::Url;

const SWAP_EXACT_TOKENS_FOR_TOKENS: [u8; 4] = [0x38, 0xed, 0x17, 0x39];
const SWAP_EXACT_ETH_FOR_TOKENS: [u8; 4] = [0x7f, 0xf3, 0x6a, 0xb5];
const SWAP_EXACT_TOKENS_FOR_ETH: [u8; 4] = [0x18, 0xcb, 0xaf, 0xe5];

#[derive(Debug, Clone)]
pub struct FrontrunSwap {
    pub selector: [u8; 4],
    pub amount_in: U256,
    pub amount_out_min: U256,
    pub path: Vec<Address>,
    pub recipient: Address,
    pub deadline: U256,
}

#[derive(Debug, Clone)]
pub struct FrontrunOpportunity {
    pub victim_hash: TxHash,
    pub router: Address,
    pub frontrun_amount: U256,
    pub swap: FrontrunSwap,
}

pub async fn start_mempool_monitor(
    config: Arc<Config>,
    dashboard: DashboardHandle,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(ws_url) = config.mempool_ws_url() else {
        let message = "mempool monitor enabled but no websocket URL is available".to_string();
        dashboard.event("warn", message.clone());
        return Err(message.into());
    };

    let ws = Ws::connect(ws_url.clone()).await?;
    let provider = Arc::new(Provider::new(ws));
    let mut stream = provider.subscribe_pending_txs().await?;

    let searcher_wallet = config
        .sender_private_key
        .parse::<LocalWallet>()?
        .with_chain_id(config.chain_id);

    info!("generic frontrun monitor connected to {}", ws_url);
    dashboard.event(
        "info",
        format!("generic frontrun monitor connected to {}", ws_url),
    );

    while let Some(tx_hash) = stream.next().await {
        match provider.get_transaction(tx_hash).await {
            Ok(Some(tx)) => {
                if !is_supported_frontrun_tx(&tx) {
                    continue;
                }

                debug!("frontrun candidate detected: {:?}", tx.hash);

                let Some(opportunity) = calculate_frontrun_opportunity(&tx, &config) else {
                    continue;
                };

                if opportunity.swap.selector != SWAP_EXACT_ETH_FOR_TOKENS {
                    debug!(
                        "skipping unsupported live frontrun selector for {:?}",
                        opportunity.victim_hash
                    );
                    continue;
                }

                match submit_frontrun_bundle(
                    provider.clone(),
                    searcher_wallet.clone(),
                    &config,
                    &dashboard,
                    &tx,
                    opportunity,
                )
                .await
                {
                    Ok(bundle_hash) => {
                        dashboard.event(
                            "success",
                            format!(
                                "frontrun bundle submitted victim={:?} bundle={:?}",
                                tx.hash, bundle_hash
                            ),
                        );
                    }
                    Err(err) => {
                        dashboard.event(
                            "warn",
                            format!(
                                "frontrun bundle submission failed victim={:?}: {}",
                                tx.hash, err
                            ),
                        );
                    }
                }
            }
            Ok(None) => {}
            Err(err) => warn!("pending tx lookup failed for {:?}: {}", tx_hash, err),
        }
    }

    Ok(())
}

pub fn is_supported_frontrun_tx(tx: &Transaction) -> bool {
    matches!(
        selector(tx),
        Some(SWAP_EXACT_TOKENS_FOR_TOKENS | SWAP_EXACT_ETH_FOR_TOKENS | SWAP_EXACT_TOKENS_FOR_ETH)
    )
}

pub fn calculate_frontrun_opportunity(
    tx: &Transaction,
    config: &Config,
) -> Option<FrontrunOpportunity> {
    let swap = decode_swap(tx)?;
    let router = tx.to?;
    let frontrun_amount = apply_slippage_bps(swap.amount_in, config.frontrun_slippage_bps);
    if frontrun_amount.is_zero() {
        return None;
    }

    Some(FrontrunOpportunity {
        victim_hash: tx.hash,
        router,
        frontrun_amount,
        swap,
    })
}

pub fn build_frontrun_bundle(
    block_number: U64,
    frontrun_tx: Bytes,
    original_tx: Transaction,
) -> BundleRequest {
    BundleRequest::new()
        .set_block(block_number + 1)
        .push_transaction(frontrun_tx)
        .push_revertible_transaction(original_tx)
}

pub fn build_frontrun_tx(
    wallet: Address,
    router: Address,
    calldata: Bytes,
    value: U256,
    gas_limit: u64,
    gas_price: U256,
    nonce: U256,
) -> TypedTransaction {
    ethers::types::Eip1559TransactionRequest::new()
        .from(wallet)
        .to(NameOrAddress::Address(router))
        .data(calldata)
        .value(value)
        .gas(gas_limit)
        .max_fee_per_gas(gas_price)
        .max_priority_fee_per_gas(gas_price)
        .nonce(nonce)
        .into()
}

fn decode_swap(tx: &Transaction) -> Option<FrontrunSwap> {
    let selector = selector(tx)?;
    let input = tx.input.as_ref();
    let args = &input[4..];

    match selector {
        SWAP_EXACT_TOKENS_FOR_TOKENS => {
            let decoded = abi::decode(
                &[
                    ParamType::Uint(256),
                    ParamType::Uint(256),
                    ParamType::Array(Box::new(ParamType::Address)),
                    ParamType::Address,
                    ParamType::Uint(256),
                ],
                args,
            )
            .ok()?;
            Some(FrontrunSwap {
                selector,
                amount_in: decoded.get(0).and_then(token_as_uint)?,
                amount_out_min: decoded.get(1).and_then(token_as_uint)?,
                path: decoded.get(2).and_then(token_as_address_vec)?,
                recipient: decoded.get(3).and_then(token_as_address)?,
                deadline: decoded.get(4).and_then(token_as_uint)?,
            })
        }
        SWAP_EXACT_ETH_FOR_TOKENS => {
            let decoded = abi::decode(
                &[
                    ParamType::Uint(256),
                    ParamType::Array(Box::new(ParamType::Address)),
                    ParamType::Address,
                    ParamType::Uint(256),
                ],
                args,
            )
            .ok()?;
            Some(FrontrunSwap {
                selector,
                amount_in: tx.value,
                amount_out_min: decoded.first().and_then(token_as_uint)?,
                path: decoded.get(1).and_then(token_as_address_vec)?,
                recipient: decoded.get(2).and_then(token_as_address)?,
                deadline: decoded.get(3).and_then(token_as_uint)?,
            })
        }
        SWAP_EXACT_TOKENS_FOR_ETH => {
            let decoded = abi::decode(
                &[
                    ParamType::Uint(256),
                    ParamType::Uint(256),
                    ParamType::Array(Box::new(ParamType::Address)),
                    ParamType::Address,
                    ParamType::Uint(256),
                ],
                args,
            )
            .ok()?;
            Some(FrontrunSwap {
                selector,
                amount_in: decoded.get(0).and_then(token_as_uint)?,
                amount_out_min: decoded.get(1).and_then(token_as_uint)?,
                path: decoded.get(2).and_then(token_as_address_vec)?,
                recipient: decoded.get(3).and_then(token_as_address)?,
                deadline: decoded.get(4).and_then(token_as_uint)?,
            })
        }
        _ => None,
    }
}

async fn submit_frontrun_bundle(
    provider: Arc<Provider<Ws>>,
    searcher_wallet: LocalWallet,
    config: &Config,
    dashboard: &DashboardHandle,
    victim_tx: &Transaction,
    opportunity: FrontrunOpportunity,
) -> Result<TxHash, Box<dyn std::error::Error>> {
    if matches!(config.bot_mode, BotMode::Shadow) || !config.allow_send {
        dashboard.event(
            "info",
            format!(
                "shadow/policy prevented live frontrun submission for {:?}",
                victim_tx.hash
            ),
        );
        return Err("live frontrun disabled by BOT_MODE/ALLOW_SEND".into());
    }

    let calldata = build_eth_for_tokens_calldata(&opportunity.swap)?;
    let block_number = provider.get_block_number().await?;
    let nonce = provider
        .get_transaction_count(searcher_wallet.address(), None)
        .await?;
    let gas_limit = victim_tx.gas.as_u64().max(250_000);
    let gas_price = bumped_gas_price(victim_tx, config.frontrun_gas_bump_bps)?;

    let frontrun_tx = build_frontrun_tx(
        searcher_wallet.address(),
        opportunity.router,
        calldata,
        opportunity.frontrun_amount,
        gas_limit,
        gas_price,
        nonce,
    );

    let signature = searcher_wallet.sign_transaction(&frontrun_tx).await?;
    let signed_frontrun = match frontrun_tx {
        Eip1559(ref tx) => tx.rlp_signed(&signature),
        _ => return Err("unexpected non-EIP1559 frontrun transaction".into()),
    };

    let relay_url = Url::parse(&config.flashbots_relay)?;
    let relay_signer = searcher_wallet.clone();
    let flashbots_client = SignerMiddleware::new(provider.clone(), searcher_wallet);
    let flashbots = FlashbotsMiddleware::new(flashbots_client, relay_url, relay_signer);
    let bundle = build_frontrun_bundle(block_number, signed_frontrun, victim_tx.clone());

    let pending_bundle = flashbots.send_bundle(&bundle).await?;
    let bundle_hash = pending_bundle
        .bundle_hash
        .ok_or("flashbots relay returned no bundle hash")?;

    dashboard.event(
        "info",
        format!(
            "frontrun submitted victim={:?} value={} min_out={} path={}",
            opportunity.victim_hash,
            opportunity.frontrun_amount,
            opportunity.swap.amount_out_min,
            format_path(&opportunity.swap.path)
        ),
    );

    Ok(bundle_hash)
}

fn build_eth_for_tokens_calldata(swap: &FrontrunSwap) -> Result<Bytes, Box<dyn std::error::Error>> {
    if swap.selector != SWAP_EXACT_ETH_FOR_TOKENS {
        return Err("only swapExactETHForTokens is supported for live frontrun".into());
    }

    let mut data = Vec::with_capacity(4 + 32 * 4);
    data.extend_from_slice(&SWAP_EXACT_ETH_FOR_TOKENS);
    data.extend(abi::encode(&[
        Token::Uint(swap.amount_out_min),
        Token::Array(swap.path.iter().copied().map(Token::Address).collect()),
        Token::Address(swap.recipient),
        Token::Uint(swap.deadline),
    ]));

    Ok(Bytes::from(data))
}

fn bumped_gas_price(
    victim_tx: &Transaction,
    gas_bump_bps: u64,
) -> Result<U256, Box<dyn std::error::Error>> {
    let base = victim_tx
        .max_fee_per_gas
        .or(victim_tx.gas_price)
        .ok_or("victim tx missing gas price data")?;

    Ok(base.saturating_mul(U256::from(gas_bump_bps)) / U256::from(10_000u64))
}

fn apply_slippage_bps(amount: U256, slippage_bps: u64) -> U256 {
    let remaining_bps = 10_000u64.saturating_sub(slippage_bps.min(10_000));
    amount.saturating_mul(U256::from(remaining_bps)) / U256::from(10_000u64)
}

fn selector(tx: &Transaction) -> Option<[u8; 4]> {
    let input = tx.input.as_ref();
    if input.len() < 4 {
        return None;
    }
    Some([input[0], input[1], input[2], input[3]])
}

fn token_as_uint(token: &Token) -> Option<U256> {
    match token {
        Token::Uint(value) => Some(*value),
        _ => None,
    }
}

fn token_as_address(token: &Token) -> Option<Address> {
    match token {
        Token::Address(value) => Some(*value),
        _ => None,
    }
}

fn token_as_address_vec(token: &Token) -> Option<Vec<Address>> {
    match token {
        Token::Array(values) => values.iter().map(token_as_address).collect(),
        _ => None,
    }
}

fn format_path(path: &[Address]) -> String {
    path.iter()
        .map(|address| format!("{address:?}"))
        .collect::<Vec<_>>()
        .join(" -> ")
}
