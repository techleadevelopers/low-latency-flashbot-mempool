use crate::cache::RuntimeCache;
use crate::config::{BotMode, Config};
use crate::contract::{ERC20Token, Simple7702Delegate};
use crate::dashboard::DashboardHandle;
use crate::queue::{estimated_total_cost_wei, ExecutionMode, OperationType, ResidualCandidate};
use crate::rpc::RpcFleet;
use ethers::middleware::SignerMiddleware;
use ethers::prelude::*;
use ethers::types::transaction::eip2718::TypedTransaction;
use ethers::types::transaction::eip2930::AccessList;
use ethers::utils::{keccak256, rlp::RlpStream};
use ethers_flashbots::{BundleRequest, FlashbotsMiddleware};
use std::sync::Arc;
use tracing::{error, info, warn};
use url::Url;

fn pending_block_id() -> BlockId {
    BlockId::Number(BlockNumber::Pending)
}

fn bumped_gas_price(base: U256) -> U256 {
    let bump_bps = U256::from(10_500u64);
    let bumped = base.saturating_mul(bump_bps) / U256::from(10_000u64);
    bumped.max(base.saturating_add(U256::from(1_000_000u64)))
}

fn candidate_min_profit_eth(config: &Config, asset_class: &str) -> f64 {
    match asset_class {
        "stable" => config.stable_policy.min_net_profit_eth,
        "other-token" => config.other_token_policy.min_net_profit_eth,
        _ => config.native_policy.min_net_profit_eth,
    }
}

fn sponsor_funding_wei(cost_wei: U256) -> U256 {
    cost_wei
        .saturating_add(cost_wei / U256::from(5u64))
        .saturating_add(U256::from(10_000_000_000_000u64))
}

pub async fn execute_job(
    wallet: LocalWallet,
    candidate: ResidualCandidate,
    contract_addr: Address,
    config: &Config,
    rpc_fleet: Arc<RpcFleet>,
    runtime_cache: Arc<RuntimeCache>,
    dashboard: DashboardHandle,
) -> Result<(), Box<dyn std::error::Error>> {
    match candidate.operation {
        OperationType::Exec => {
            execute_exec_job(
                wallet,
                candidate,
                contract_addr,
                config,
                rpc_fleet,
                runtime_cache,
                dashboard,
            )
            .await
        }
        OperationType::Install => {
            execute_install_job(
                wallet,
                candidate,
                contract_addr,
                config,
                rpc_fleet,
                dashboard,
            )
            .await
        }
    }
}

async fn execute_install_job(
    wallet: LocalWallet,
    candidate: ResidualCandidate,
    contract_addr: Address,
    config: &Config,
    rpc_fleet: Arc<RpcFleet>,
    dashboard: DashboardHandle,
) -> Result<(), Box<dyn std::error::Error>> {
    if !matches!(candidate.execution_mode, ExecutionMode::Delegate7702) {
        let message = format!(
            "install requested for {:?} in unsupported mode {:?}",
            wallet.address(),
            candidate.execution_mode
        );
        dashboard.event("error", message.clone());
        return Err(message.into());
    }

    let sponsor_wallet = config
        .sender_private_key
        .parse::<LocalWallet>()?
        .with_chain_id(config.chain_id);

    if config.mock_contract_mode {
        let total_cost_wei =
            estimated_total_cost_wei(config, U256::from(15_000_000_000u64), &candidate);
        let total_cost_eth = wei_to_eth_f64(total_cost_wei);
        let profit_eth = wei_to_eth_f64(candidate.estimated_net_profit_wei);
        dashboard.event(
            "info",
            format!(
                "mock install/delegate prepared for {:?} | total_cost={:.6} ETH | expected_profit={:.6} ETH",
                wallet.address(),
                total_cost_eth,
                profit_eth
            ),
        );
        return Ok(());
    }

    let endpoint = rpc_fleet.send_endpoint();
    let provider = endpoint.provider.clone();
    let started = std::time::Instant::now();

    let gas_price = bumped_gas_price(
        provider
            .get_gas_price()
            .await
            .unwrap_or_else(|_| U256::from(15_000_000_000u64)),
    );
    let target_nonce = provider
        .get_transaction_count(wallet.address(), Some(pending_block_id()))
        .await?;
    let sponsor_nonce = provider
        .get_transaction_count(sponsor_wallet.address(), Some(pending_block_id()))
        .await?;
    let sweep_nonce = target_nonce.saturating_add(U256::one());
    let delegate_tokens = candidate.approval_tokens.clone();

    let auth = build_eip7702_authorization(&wallet, config.chain_id, contract_addr, target_nonce)?;
    let install_tx = build_eip7702_install_tx(
        &sponsor_wallet,
        sponsor_nonce.saturating_add(U256::one()),
        gas_price,
        config.estimated_install_gas,
        sponsor_wallet.address(),
        &[auth],
    )?;
    let exec_calldata = Simple7702Delegate::new(contract_addr, provider.clone())
        .delegate_sweep_all(config.control_address, delegate_tokens)
        .calldata()
        .ok_or("failed to build exec calldata for delegateSweepAll")?;
    let exec_tx: TypedTransaction = TransactionRequest::new()
        .to(wallet.address())
        .data(exec_calldata)
        .value(U256::zero())
        .gas(config.estimated_exec_gas)
        .gas_price(gas_price)
        .nonce(sweep_nonce)
        .from(wallet.address())
        .into();
    let exec_signature = wallet.sign_transaction(&exec_tx).await?;
    let signed_exec_tx = exec_tx.rlp_signed(&exec_signature);

    let exec_gas_cost = gas_price.saturating_mul(U256::from(config.estimated_exec_gas));
    let sponsor_funding = sponsor_funding_wei(exec_gas_cost);
    let sponsor_tx: TypedTransaction = TransactionRequest::new()
        .to(wallet.address())
        .value(sponsor_funding)
        .gas(21_000u64)
        .gas_price(gas_price)
        .nonce(sponsor_nonce)
        .from(sponsor_wallet.address())
        .into();
    let sponsor_signature = sponsor_wallet.sign_transaction(&sponsor_tx).await?;
    let signed_sponsor_tx = sponsor_tx.rlp_signed(&sponsor_signature);

    let total_cost_wei = estimated_total_cost_wei(config, gas_price, &candidate);
    let total_cost_eth = wei_to_eth_f64(total_cost_wei);
    let profit_eth = wei_to_eth_f64(candidate.estimated_net_profit_wei);

    let bundle = BundleRequest::new()
        .push_transaction(signed_sponsor_tx)
        .push_transaction(install_tx.clone())
        .push_transaction(signed_exec_tx);
    let relay_signer = sponsor_wallet.clone();
    let flashbots_client = SignerMiddleware::new(provider.clone(), sponsor_wallet.clone());
    let relay_url = Url::parse(&config.flashbots_relay)?;
    let flashbots = FlashbotsMiddleware::new(flashbots_client, relay_url, relay_signer);

    match config.bot_mode {
        BotMode::Shadow => {
            dashboard.event(
                "info",
                format!(
                    "shadow mode: 7702 bundle prepared for {:?} via {} | delegate={:?} | estimated_cost={:.6} ETH | expected_profit={:.6} ETH",
                    wallet.address(),
                    endpoint.name,
                    contract_addr,
                    total_cost_eth,
                    profit_eth
                ),
            );
            return Ok(());
        }
        BotMode::Paper => {
            if !config.allow_send {
                dashboard.event(
                    "info",
                    format!(
                        "paper mode without ALLOW_SEND: 7702 install for {:?} blocked",
                        wallet.address()
                    ),
                );
                return Ok(());
            }
        }
        BotMode::Live => {
            if !config.allow_send {
                dashboard.event(
                    "warn",
                    "live mode selected but ALLOW_SEND=false, blocking 7702 send".to_string(),
                );
                return Ok(());
            }
        }
    }

    let result = flashbots.send_bundle(&bundle).await;
    dashboard.record_latency(
        "bundle_attempt",
        started.elapsed().as_millis(),
        Some(&format!("{:?}", wallet.address())),
        Some(&endpoint.name),
    );

    match result {
        Ok(_) => {
            dashboard.event(
                "success",
                format!(
                    "7702 install sent for {:?} via {} | delegate={:?} | estimated_cost={:.6} ETH | expected_profit={:.6} ETH",
                    wallet.address(),
                    endpoint.name,
                    contract_addr,
                    total_cost_eth,
                    profit_eth
                ),
            );
            Ok(())
        }
        Err(err) => {
            error!("Flashbots install bundle failed: {}", err);

            if config.disable_public_fallback {
                let message = format!(
                    "Flashbots install failed and public fallback is disabled for {:?}: {}",
                    wallet.address(),
                    err
                );
                dashboard.event("error", message.clone());
                rpc_fleet.report_failure(endpoint.id);
                return Err(message.into());
            }

            match provider.send_raw_transaction(install_tx).await {
                Ok(pending_tx) => {
                    dashboard.event(
                        "warn",
                        format!(
                            "7702 install fell back to public send for {:?} via {} | tx={:?} | note=delegate sweep not included in fallback path",
                            wallet.address(),
                            endpoint.name,
                            pending_tx.tx_hash()
                        ),
                    );
                    Ok(())
                }
                Err(fallback_err) => {
                    rpc_fleet.report_failure(endpoint.id);
                    Err(format!(
                        "flashbots install failed and public fallback also failed on {}: {}",
                        endpoint.name, fallback_err
                    )
                    .into())
                }
            }
        }
    }
}

async fn execute_exec_job(
    wallet: LocalWallet,
    candidate: ResidualCandidate,
    contract_addr: Address,
    config: &Config,
    rpc_fleet: Arc<RpcFleet>,
    runtime_cache: Arc<RuntimeCache>,
    dashboard: DashboardHandle,
) -> Result<(), Box<dyn std::error::Error>> {
    let exec_started = std::time::Instant::now();
    let spender_mode = matches!(candidate.execution_mode, ExecutionMode::Spender);
    let delegate_mode = matches!(candidate.execution_mode, ExecutionMode::Delegate7702);
    let amount = candidate.total_residual_wei;
    let estimated_profit_wei = candidate.estimated_net_profit_wei;
    let estimated_cost_wei = candidate.estimated_cost_wei;
    let profit_eth = wei_to_eth_f64(estimated_profit_wei);
    let roi_bps = candidate.roi_bps();

    info!(
        "starting residual exec wallet={:?} class={} total={:.6} profit={:.6} roi={}bps",
        wallet.address(),
        candidate.asset_class,
        wei_to_eth_f64(amount),
        profit_eth,
        roi_bps
    );
    info!(
        "residual breakdown native={:.6} stable={:.6} other={:.6} total_tokens={:.6} estimated_cost={:.6}",
        wei_to_eth_f64(candidate.native_balance),
        wei_to_eth_f64(candidate.stable_token_value_wei),
        wei_to_eth_f64(candidate.other_token_value_wei),
        wei_to_eth_f64(candidate.token_value_wei),
        wei_to_eth_f64(estimated_cost_wei)
    );

    let endpoint = rpc_fleet.send_endpoint();
    let provider = endpoint.provider.clone();
    let sponsor_wallet = config
        .sender_private_key
        .parse::<LocalWallet>()?
        .with_chain_id(config.chain_id);

    let (calldata, gas_price, exec_value, exec_from, exec_to) = if config.mock_contract_mode
        && config.bot_mode == BotMode::Shadow
    {
        dashboard.event(
            "info",
            format!(
                "mock contract mode active for {:?}, skipping on-chain contract reads",
                wallet.address()
            ),
        );
        (
            Bytes::from_static(&[0xde, 0xad, 0xbe, 0xef]),
            U256::from(1_000_000_000u64),
            U256::zero(),
            sponsor_wallet.address(),
            contract_addr,
        )
    } else {
        let started = std::time::Instant::now();
        let contract = Simple7702Delegate::new(contract_addr, provider.clone());
        let filtered_tokens = candidate.approval_tokens.clone();

        if spender_mode {
            let contract_state = match runtime_cache
                .contract_state(provider.clone(), contract_addr, &config.network)
                .await
            {
                Ok(value) => value,
                Err(err) => {
                    rpc_fleet.report_failure(endpoint.id);
                    return Err(format!(
                        "failed to read cached contract state via {}: {}",
                        endpoint.name, err
                    )
                    .into());
                }
            };
            let contract_owner = contract.owner().call().await?;

            if contract_state.frozen {
                rpc_fleet.report_success(endpoint.id, started.elapsed());
                warn!(
                    "contract is frozen, aborting exec for {:?}",
                    wallet.address()
                );
                dashboard.event(
                    "warn",
                    format!("contract frozen, exec aborted for {:?}", wallet.address()),
                );
                return Ok(());
            }

            match contract_state.destination {
                Some(destination)
                    if destination == config.control_address || destination == config.forwarder => {
                }
                Some(destination) => {
                    let message = format!(
                        "destination guard failed: expected operational destination {:?} or forwarder {:?}, got {:?}",
                        config.control_address, config.forwarder, destination
                    );
                    dashboard.event("error", message.clone());
                    return Err(message.into());
                }
                None => {
                    let message = format!(
                        "destination unavailable from contract cache for {:?}",
                        contract_addr
                    );
                    dashboard.event("error", message.clone());
                    return Err(message.into());
                }
            }

            if contract_owner != sponsor_wallet.address() {
                let message = format!(
                    "contract owner mismatch: expected {:?}, got {:?}",
                    sponsor_wallet.address(),
                    contract_owner
                );
                dashboard.event("error", message.clone());
                return Err(message.into());
            }
        }

        let calldata = if spender_mode {
            contract
                .sweep_all_from(wallet.address(), filtered_tokens)
                .calldata()
                .ok_or("failed to build exec calldata for sweepAllFrom")?
        } else if delegate_mode {
            contract
                .delegate_sweep_all(config.control_address, filtered_tokens)
                .calldata()
                .ok_or("failed to build exec calldata for delegateSweepAll")?
        } else {
            return Err("unsupported execution mode".into());
        };

        let gas_price = match runtime_cache
            .gas_price(provider.clone(), &config.network)
            .await
        {
            Ok(price) => {
                rpc_fleet.report_success(endpoint.id, started.elapsed());
                bumped_gas_price(price)
            }
            Err(err) => {
                warn!("gas price fetch failed on {}: {}", endpoint.name, err);
                rpc_fleet.report_failure(endpoint.id);
                bumped_gas_price(U256::from(15_000_000_000u64))
            }
        };

        let exec_from = if delegate_mode {
            wallet.address()
        } else {
            sponsor_wallet.address()
        };
        let exec_to = if delegate_mode {
            wallet.address()
        } else {
            contract_addr
        };
        (calldata, gas_price, U256::zero(), exec_from, exec_to)
    };

    let final_cost_wei = estimated_total_cost_wei(config, gas_price, &candidate);
    let final_profit_wei = amount.saturating_sub(final_cost_wei);
    let final_profit_eth = wei_to_eth_f64(final_profit_wei);
    let min_profit_eth = candidate_min_profit_eth(config, &candidate.asset_class);
    let min_profit_wei = ethers::utils::parse_ether(min_profit_eth.to_string())?;

    if final_profit_wei < min_profit_wei {
        let message = format!(
            "residual exec no longer profitable: profit={:.6} ETH < min={:.6} ETH",
            final_profit_eth, min_profit_eth
        );
        warn!("{}", message);
        dashboard.event("warn", message.clone());
        return Err(message.into());
    }

    if final_profit_wei < estimated_profit_wei / U256::from(2u64) {
        dashboard.event(
            "warn",
            format!(
                "profit dropped significantly: {:.6} ETH -> {:.6} ETH, continuing",
                profit_eth, final_profit_eth
            ),
        );
    }

    let exec_tx: TypedTransaction = TransactionRequest::new()
        .to(exec_to)
        .data(calldata)
        .value(exec_value)
        .gas(config.estimated_exec_gas)
        .gas_price(gas_price)
        .from(exec_from)
        .into();

    dashboard.record_latency(
        "tx_prepare",
        exec_started.elapsed().as_millis(),
        Some(&format!("{:?}", wallet.address())),
        Some(&endpoint.name),
    );

    match config.bot_mode {
        BotMode::Shadow => {
            dashboard.event(
                "info",
                format!(
                    "shadow mode: residual exec prepared for {:?} via {} | value={} ETH | profit={:.6} ETH | ROI={} bps",
                    wallet.address(),
                    endpoint.name,
                    wei_to_eth_f64(amount),
                    profit_eth,
                    roi_bps
                ),
            );
            dashboard.record_latency(
                "bundle_attempt",
                0,
                Some(&format!("{:?}", wallet.address())),
                Some("shadow-skipped"),
            );
            return Ok(());
        }
        BotMode::Paper => {
            if !config.allow_send {
                dashboard.event(
                    "info",
                    format!(
                        "paper mode without ALLOW_SEND: residual exec for {:?} blocked",
                        wallet.address()
                    ),
                );
                return Ok(());
            }

            if !config.test_wallet_allowlist.is_empty()
                && !config.test_wallet_allowlist.contains(&wallet.address())
            {
                dashboard.event(
                    "warn",
                    format!(
                        "paper mode blocked {:?}: wallet not in TEST_WALLET_ALLOWLIST",
                        wallet.address()
                    ),
                );
                return Ok(());
            }

            if let Some(max_eth) = config.max_sweep_value_eth {
                let max_value = ethers::utils::parse_ether(max_eth.to_string())?;
                if amount > max_value {
                    dashboard.event(
                        "warn",
                        format!(
                            "paper mode blocked {:?}: amount {} ETH exceeds MAX_SWEEP_VALUE_ETH {}",
                            wallet.address(),
                            wei_to_eth_f64(amount),
                            max_eth
                        ),
                    );
                    return Ok(());
                }
            }
        }
        BotMode::Live => {
            if !config.allow_send {
                dashboard.event(
                    "warn",
                    "live mode selected but ALLOW_SEND=false, blocking send".to_string(),
                );
                return Ok(());
            }
        }
    }

    let mut signed_candidate_txs = Vec::new();

    for token in &candidate.approval_tokens {
        let approve_call =
            ERC20Token::new(*token, provider.clone()).approve(contract_addr, U256::MAX);
        let approve_data = approve_call
            .calldata()
            .ok_or("failed to build approve calldata")?;
        let approve_tx: TypedTransaction = TransactionRequest::new()
            .to(*token)
            .data(approve_data)
            .value(U256::zero())
            .gas(config.estimated_approve_gas)
            .gas_price(gas_price)
            .from(wallet.address())
            .into();
        let approve_signature = wallet.sign_transaction(&approve_tx).await?;
        signed_candidate_txs.push(approve_tx.rlp_signed(&approve_signature));
    }

    let exec_signature = if delegate_mode {
        wallet.sign_transaction(&exec_tx).await?
    } else {
        sponsor_wallet.sign_transaction(&exec_tx).await?
    };
    signed_candidate_txs.push(exec_tx.rlp_signed(&exec_signature));
    let sponsored_mode = config.use_external_gas_sponsor;

    let bundle = if sponsored_mode {
        let sponsor_funding = if spender_mode {
            let approval_cost_wei = gas_price.saturating_mul(U256::from(
                config
                    .estimated_approve_gas
                    .saturating_mul(candidate.approval_tokens.len() as u64),
            ));
            sponsor_funding_wei(approval_cost_wei)
        } else {
            sponsor_funding_wei(final_cost_wei)
        };
        let sponsor_tx: TypedTransaction = TransactionRequest::new()
            .to(wallet.address())
            .value(sponsor_funding)
            .gas(21_000u64)
            .gas_price(gas_price)
            .from(sponsor_wallet.address())
            .into();
        let sponsor_signature = sponsor_wallet.sign_transaction(&sponsor_tx).await?;
        let signed_sponsor_tx = sponsor_tx.rlp_signed(&sponsor_signature);
        let mut bundle = BundleRequest::new().push_transaction(signed_sponsor_tx);
        for signed_tx in signed_candidate_txs {
            bundle = bundle.push_transaction(signed_tx);
        }
        bundle
    } else {
        let mut bundle = BundleRequest::new();
        for signed_tx in signed_candidate_txs {
            bundle = bundle.push_transaction(signed_tx);
        }
        bundle
    };

    let relay_signer = sponsor_wallet.clone();
    let flashbots_client = SignerMiddleware::new(provider.clone(), sponsor_wallet.clone());
    let public_client = if delegate_mode {
        Arc::new(SignerMiddleware::new(provider.clone(), wallet.clone()))
    } else {
        Arc::new(SignerMiddleware::new(
            provider.clone(),
            sponsor_wallet.clone(),
        ))
    };
    let relay_url = Url::parse(&config.flashbots_relay)?;
    let flashbots = FlashbotsMiddleware::new(flashbots_client, relay_url, relay_signer);

    let bundle_started = std::time::Instant::now();
    let bundle_result = flashbots.send_bundle(&bundle).await;
    dashboard.record_latency(
        "bundle_attempt",
        bundle_started.elapsed().as_millis(),
        Some(&format!("{:?}", wallet.address())),
        Some(&endpoint.name),
    );

    match bundle_result {
        Ok(_) => {
            dashboard.mark_sweep_success_with_profit(
                &format!("{:?}", wallet.address()),
                &endpoint.name,
                final_profit_wei,
            );
            dashboard.event(
                "success",
                format!(
                    "residual exec succeeded for {:?} | profit={:.6} ETH | ROI={} bps{}",
                    wallet.address(),
                    final_profit_eth,
                    roi_bps,
                    if sponsored_mode {
                        format!(
                            " | sponsored_gas={:.6} ETH",
                            wei_to_eth_f64(sponsor_funding_wei(final_cost_wei))
                        )
                    } else {
                        String::new()
                    }
                ),
            );
            Ok(())
        }
        Err(err) => {
            error!("Flashbots bundle failed: {}", err);

            if config.disable_public_fallback {
                let message = format!(
                    "Flashbots failed and public fallback is disabled for {:?}: {}",
                    wallet.address(),
                    err
                );
                dashboard.event("error", &message);
                rpc_fleet.report_failure(endpoint.id);
                return Err(message.into());
            }

            if sponsored_mode {
                let message = format!(
                    "Flashbots sponsored bundle failed for {:?} and public fallback cannot preserve sponsor semantics: {}",
                    wallet.address(),
                    err
                );
                dashboard.event("error", &message);
                rpc_fleet.report_failure(endpoint.id);
                return Err(message.into());
            }

            warn!(
                "Flashbots failed, trying public mempool fallback via {}",
                endpoint.name
            );
            dashboard.event(
                "warn",
                format!(
                    "flashbots relay failed for {:?}, trying public fallback via {}",
                    wallet.address(),
                    endpoint.name
                ),
            );

            match public_client.send_transaction(exec_tx, None).await {
                Ok(pending_tx) => {
                    info!("fallback mempool tx sent: {:?}", pending_tx.tx_hash());
                    dashboard.mark_sweep_success_with_profit(
                        &format!("{:?}", wallet.address()),
                        &endpoint.name,
                        final_profit_wei,
                    );
                    Ok(())
                }
                Err(fallback_err) => {
                    rpc_fleet.report_failure(endpoint.id);
                    Err(format!(
                        "flashbots failed and public fallback also failed on {}: {}",
                        endpoint.name, fallback_err
                    )
                    .into())
                }
            }
        }
    }
}

fn wei_to_eth_f64(wei: U256) -> f64 {
    wei.to_string().parse::<f64>().unwrap_or(0.0) / 1e18
}

#[derive(Clone)]
struct Eip7702Authorization {
    chain_id: U256,
    delegate_address: Address,
    nonce: U256,
    signature: Signature,
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
