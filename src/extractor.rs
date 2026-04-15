use crate::cache::RuntimeCache;
use crate::config::{BotMode, Config};
use crate::contract::Simple7702Delegate;
use crate::dashboard::DashboardHandle;
use crate::queue::ResidualCandidate;
use crate::rpc::RpcFleet;
use ethers::middleware::SignerMiddleware;
use ethers::prelude::*;
use ethers::types::transaction::eip2718::TypedTransaction;
use ethers_flashbots::{BundleRequest, FlashbotsMiddleware};
use std::sync::Arc;
use tracing::{error, info, warn};
use url::Url;

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

pub async fn sweep(
    wallet: LocalWallet,
    candidate: ResidualCandidate,
    contract_addr: Address,
    config: &Config,
    rpc_fleet: Arc<RpcFleet>,
    runtime_cache: Arc<RuntimeCache>,
    dashboard: DashboardHandle,
) -> Result<(), Box<dyn std::error::Error>> {
    let sweep_started = std::time::Instant::now();
    let amount = candidate.total_residual_wei;
    let estimated_profit_wei = candidate.estimated_net_profit_wei;
    let estimated_cost_wei = candidate.estimated_cost_wei;
    let profit_eth = wei_to_eth_f64(estimated_profit_wei);
    let roi_bps = candidate.roi_bps();

    info!(
        "starting residual sweep wallet={:?} class={} total={:.6} profit={:.6} roi={}bps",
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

    let (calldata, gas_price) = if config.mock_contract_mode && config.bot_mode == BotMode::Shadow {
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
        )
    } else {
        let started = std::time::Instant::now();
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
        let contract = Simple7702Delegate::new(contract_addr, provider.clone());

        if contract_state.frozen {
            rpc_fleet.report_success(endpoint.id, started.elapsed());
            warn!(
                "contract is frozen, aborting sweep for {:?}",
                wallet.address()
            );
            dashboard.event(
                "warn",
                format!("contract frozen, sweep aborted for {:?}", wallet.address()),
            );
            return Ok(());
        }

        match contract_state.destination {
            Some(destination)
                if destination == config.control_address || destination == config.forwarder => {}
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

        let filtered_tokens = if config.enable_token_sweep {
            let native_eth = wei_to_eth_f64(candidate.native_balance);
            let token_eth = wei_to_eth_f64(candidate.token_value_wei);
            if token_eth > native_eth * config.max_token_value_ratio {
                warn!(
                    "token value ({:.4} ETH) exceeds {:.1}x native balance ({:.4} ETH), skipping tokens",
                    token_eth, config.max_token_value_ratio, native_eth
                );
                Vec::new()
            } else {
                contract_state.tokens
            }
        } else {
            Vec::new()
        };

        let calldata = contract
            .sweep_all(filtered_tokens)
            .calldata()
            .ok_or("failed to build sweep calldata")?;

        let gas_price = match runtime_cache
            .gas_price(provider.clone(), &config.network)
            .await
        {
            Ok(price) => {
                rpc_fleet.report_success(endpoint.id, started.elapsed());
                price
            }
            Err(err) => {
                warn!("gas price fetch failed on {}: {}", endpoint.name, err);
                rpc_fleet.report_failure(endpoint.id);
                U256::from(15_000_000_000u64)
            }
        };

        (calldata, gas_price)
    };

    let final_cost_wei = gas_price.saturating_mul(U256::from(config.estimated_sweep_gas));
    let final_profit_wei = amount.saturating_sub(final_cost_wei);
    let final_profit_eth = wei_to_eth_f64(final_profit_wei);
    let min_profit_eth = candidate_min_profit_eth(config, &candidate.asset_class);
    let min_profit_wei = ethers::utils::parse_ether(min_profit_eth.to_string())?;

    if final_profit_wei < min_profit_wei {
        let message = format!(
            "residual sweep no longer profitable: profit={:.6} ETH < min={:.6} ETH",
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

    let tx: TypedTransaction = TransactionRequest::new()
        .to(contract_addr)
        .data(calldata)
        .value(amount)
        .gas(config.estimated_sweep_gas)
        .gas_price(gas_price)
        .from(wallet.address())
        .into();

    dashboard.record_latency(
        "tx_prepare",
        sweep_started.elapsed().as_millis(),
        Some(&format!("{:?}", wallet.address())),
        Some(&endpoint.name),
    );

    match config.bot_mode {
        BotMode::Shadow => {
            dashboard.event(
                "info",
                format!(
                    "shadow mode: residual sweep prepared for {:?} via {} | value={} ETH | profit={:.6} ETH | ROI={} bps",
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
                        "paper mode without ALLOW_SEND: residual sweep for {:?} blocked",
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

    let sponsor_wallet = config
        .sender_private_key
        .parse::<LocalWallet>()?
        .with_chain_id(config.chain_id);
    let signature = wallet.sign_transaction(&tx).await?;
    let signed_bundle_tx = tx.rlp_signed(&signature);
    let sponsored_mode = config.use_external_gas_sponsor;

    // Build bundle: sponsor -> sweep (no approve)
    let bundle = if sponsored_mode {
        let sponsor_funding = sponsor_funding_wei(final_cost_wei);
        let sponsor_tx: TypedTransaction = TransactionRequest::new()
            .to(wallet.address())
            .value(sponsor_funding)
            .gas(21_000u64)
            .gas_price(gas_price)
            .from(sponsor_wallet.address())
            .into();
        let sponsor_signature = sponsor_wallet.sign_transaction(&sponsor_tx).await?;
        let signed_sponsor_tx = sponsor_tx.rlp_signed(&sponsor_signature);
        BundleRequest::new()
            .push_transaction(signed_sponsor_tx)
            .push_transaction(signed_bundle_tx)
    } else {
        BundleRequest::new().push_transaction(signed_bundle_tx)
    };

    let tx_signer = wallet.clone().with_chain_id(config.chain_id);
    let relay_signer = sponsor_wallet.clone();
    let flashbots_client = SignerMiddleware::new(provider.clone(), sponsor_wallet.clone());
    let public_client = Arc::new(SignerMiddleware::new(provider.clone(), tx_signer));
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
                    "residual sweep succeeded for {:?} | profit={:.6} ETH | ROI={} bps{}",
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

            match public_client.send_transaction(tx, None).await {
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