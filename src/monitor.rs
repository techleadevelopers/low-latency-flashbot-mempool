use crate::cache::RuntimeCache;
use crate::config::{AssetClass, AssetPolicy, Config, MockHotWalletMode};
use crate::contract::ERC20Token;
use crate::dashboard::{DashboardHandle, WalletSnapshot};
use crate::extractor;
use crate::queue::{
    estimated_total_cost_wei, OperationType, ResidualCandidate, SweepQueue,
};
use crate::rpc::RpcFleet;
use ethers::prelude::*;
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::{debug, error, info, warn};

#[derive(Default)]
struct WalletExecutionState {
    processing: bool,
    last_attempt: Option<std::time::Instant>,
    backoff_until: Option<std::time::Instant>,
    consecutive_failures: u32,
}

struct SweepTaskResult {
    wallet: Address,
    rpc: String,
    result: Result<(), String>,
}

pub async fn start_monitor(
    rpc_fleet: Arc<RpcFleet>,
    runtime_cache: Arc<RuntimeCache>,
    config: Arc<Config>,
    wallets: Vec<LocalWallet>,
    dashboard: DashboardHandle,
) -> Result<(), Box<dyn std::error::Error>> {
    info!(
        "Starting Corporate Residual Sweeper for {} wallets",
        wallets.len()
    );
    dashboard.event(
        "info",
        format!(
            "Corporate Residual Sweeper started for {} wallets",
            wallets.len()
        ),
    );

    // MIN_BALANCE agora é apenas piso de proteção (reserva mínima de segurança)
    let min_native_reserve = ethers::utils::parse_ether(config.min_balance.to_string())?;
    let mut current_interval_ms = config.min_scan_interval_ms.max(100);
    let scan_limiter = Arc::new(Semaphore::new(config.scan_concurrency.max(1)));
    let wallet_cooldown = Duration::from_secs(config.wallet_cooldown_secs);
    let queue_dedupe = Duration::from_secs(config.queue_dedupe_secs);
    let stale_threshold = Duration::from_secs(config.rpc_stale_threshold_secs);
    let mut wallet_states: HashMap<Address, WalletExecutionState> = HashMap::new();
    let mut wallet_lookup = HashMap::new();
    let all_wallet_addresses: Vec<Address> = wallets.iter().map(LocalWallet::address).collect();
    for wallet in wallets.iter().cloned() {
        wallet_states.entry(wallet.address()).or_default();
        wallet_lookup.insert(wallet.address(), wallet);
    }
    let mut sweep_queue = SweepQueue::new(queue_dedupe);
    let mut last_seen_block: Option<u64> = None;
    let mut mock_hot_rounds_executed = 0usize;
    let mut active_sweeps: JoinSet<SweepTaskResult> = JoinSet::new();

    loop {
        while let Some(result) = active_sweeps.try_join_next() {
            match result {
                Ok(task) => {
                    let state = wallet_states.entry(task.wallet).or_default();
                    state.processing = false;

                    if let Err(err) = task.result {
                        dashboard.event(
                            "warn",
                            format!("sweep worker failed for {:?} via {}", task.wallet, task.rpc),
                        );
                        dashboard.mark_sweep_failure(&format!("{:?}", task.wallet), &err);
                        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
                        let exp = state.consecutive_failures.min(4);
                        let backoff_secs = (config.wallet_failure_backoff_base_secs
                            * 2u64.pow(exp))
                        .min(config.wallet_failure_backoff_max_secs);
                        state.backoff_until =
                            Some(std::time::Instant::now() + Duration::from_secs(backoff_secs));
                        error!("Sweep failed: {}", err);
                    } else {
                        state.consecutive_failures = 0;
                        state.backoff_until = None;
                    }

                    sweep_queue.finish(task.wallet);
                }
                Err(err) => {
                    dashboard.event("error", format!("sweep task join error: {}", err));
                    error!("sweep task join error: {}", err);
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(current_interval_ms)).await;
        let scan_started = std::time::Instant::now();
        let now = std::time::Instant::now();

        let eligible_wallets: Vec<Address> = all_wallet_addresses
            .iter()
            .copied()
            .filter(|wallet_addr| {
                wallet_states.get(wallet_addr).is_some_and(|state| {
                    if state.processing {
                        return false;
                    }
                    if let Some(backoff_until) = state.backoff_until {
                        if backoff_until > now {
                            return false;
                        }
                    }
                    true
                })
            })
            .collect();

        let should_inject_mock = match config.mock_hot_wallet_mode {
            MockHotWalletMode::Continuous => config.mock_hot_wallet_count > 0,
            MockHotWalletMode::OneShot => {
                config.mock_hot_wallet_count > 0
                    && mock_hot_rounds_executed < config.mock_hot_wallet_rounds.max(1)
            }
        };
        let mut hot_wallet_snapshots = Vec::new();
        let mut queued_hot_wallets = 0usize;
        let mut queued_profit_wei = U256::zero();
        let mut injected_mock_wallets = HashSet::new();

        if should_inject_mock {
            let mock_balance_wei =
                ethers::utils::parse_ether(config.mock_hot_balance_eth.to_string())?;
            let mock_balance_eth = config.mock_hot_balance_eth;
            let mock_cost_wei = U256::from(5_000_000_000u64)
                .saturating_mul(U256::from(config.estimated_exec_gas + config.estimated_bundle_overhead_gas));
            let mock_profit_wei = mock_balance_wei.saturating_sub(mock_cost_wei);

            if mock_profit_wei > U256::zero() && mock_balance_wei >= mock_cost_wei {
                let mut injected = 0usize;
                for wallet_addr in eligible_wallets.iter().copied() {
                    if injected >= config.mock_hot_wallet_count {
                        break;
                    }
                    injected_mock_wallets.insert(wallet_addr);
                    hot_wallet_snapshots.push(WalletSnapshot {
                        address: format!("{:?}", wallet_addr),
                        balance_eth: mock_balance_eth.to_string(),
                        rpc: "mock-hot".to_string(),
                    });

                    let candidate = ResidualCandidate {
                        wallet: wallet_addr,
                        operation: OperationType::Exec,
                        requires_approve: false,
                        approval_tokens: Vec::new(),
                        native_balance: mock_balance_wei,
                        token_value_wei: U256::zero(),
                        stable_token_value_wei: U256::zero(),
                        other_token_value_wei: U256::zero(),
                        total_residual_wei: mock_balance_wei,
                        estimated_net_profit_wei: mock_profit_wei,
                        estimated_cost_wei: mock_cost_wei,
                        asset_class: AssetClass::Native.as_str().to_string(),
                        rpc: "mock-hot".to_string(),
                        timestamp: std::time::Instant::now(),
                    };

                    if queue_residual_candidate(
                        &mut wallet_states,
                        &mut sweep_queue,
                        &dashboard,
                        candidate,
                        now,
                        wallet_cooldown,
                        scan_started.elapsed().as_millis(),
                        config.hot_path_info_events,
                    ) {
                        queued_hot_wallets += 1;
                        queued_profit_wei = queued_profit_wei.saturating_add(mock_profit_wei);
                    }
                    injected += 1;
                }

                dispatch_ready_sweeps(
                    &mut active_sweeps,
                    &mut sweep_queue,
                    &mut wallet_states,
                    &wallet_lookup,
                    &runtime_cache,
                    &rpc_fleet,
                    &config,
                    U256::from(5_000_000_000u64),
                    &dashboard,
                )
                .await?;

                if injected > 0 {
                    mock_hot_rounds_executed = mock_hot_rounds_executed.saturating_add(1);
                }
            }
        }

        let control_endpoint = rpc_fleet.read_endpoint();
        let block_started = std::time::Instant::now();
        let previous_block = last_seen_block;
        let latest_block = match control_endpoint.provider.get_block_number().await {
            Ok(block) => {
                dashboard.record_latency(
                    "block_fetch",
                    block_started.elapsed().as_millis(),
                    None,
                    Some(&control_endpoint.name),
                );
                rpc_fleet.report_success(control_endpoint.id, block_started.elapsed());
                rpc_fleet.report_block(control_endpoint.id, block);
                Some(block.as_u64())
            }
            Err(err) => {
                rpc_fleet.report_provider_error(control_endpoint.id, &err.to_string());
                dashboard.event(
                    "warn",
                    format!(
                        "failed to fetch latest block from {}: {}",
                        control_endpoint.name, err
                    ),
                );
                None
            }
        };

        // Gas price e custo estimado para cálculos de lucro
        let cycle_gas_price = runtime_cache
            .gas_price(control_endpoint.provider.clone(), &config.network)
            .await
            .unwrap_or_else(|_| U256::from(15_000_000_000u64));
        let has_new_block = match latest_block {
            Some(block) => {
                if previous_block == Some(block)
                    && sweep_queue.len() == 0
                    && queued_hot_wallets == 0
                {
                    last_seen_block = Some(block);
                    false
                } else {
                    last_seen_block = Some(block);
                    true
                }
            }
            None => true,
        };

        if let Some(block) = latest_block {
            if previous_block == Some(block) && sweep_queue.len() == 0 && queued_hot_wallets == 0 {
                current_interval_ms = (current_interval_ms.saturating_mul(2))
                    .min(config.max_scan_interval_ms.max(config.min_scan_interval_ms));
                if config.hot_path_info_events {
                    dashboard.event(
                        "info",
                        format!(
                            "no new block on {}, backing off scan interval to {} ms",
                            control_endpoint.name, current_interval_ms
                        ),
                    );
                }
            } else {
                current_interval_ms = config.min_scan_interval_ms.max(100);
            }
        } else {
            current_interval_ms = (current_interval_ms.saturating_add(500))
                .min(config.max_scan_interval_ms.max(config.min_scan_interval_ms));
        }

        for endpoint in rpc_fleet.snapshot() {
            if matches!(endpoint.block_age_secs, Some(age) if age > stale_threshold.as_secs()) {
                rpc_fleet.mark_stale(endpoint.id);
            }
        }
        let should_enrich_tokens =
            has_new_block && config.enable_token_sweep && !config.monitored_tokens.is_empty();

        // Scan de saldos e tokens em paralelo
        let mut tasks = JoinSet::new();
        for wallet_chunk in eligible_wallets.chunks(config.batch_scan_size.max(1)) {
            let chunk = wallet_chunk.to_vec();
            let fleet = rpc_fleet.clone();
            let limiter = scan_limiter.clone();
            let config = config.clone();
            let min_native_reserve = min_native_reserve;
            let should_enrich_tokens = should_enrich_tokens;
            tasks.spawn(async move {
                let _permit = limiter
                    .acquire_owned()
                    .await
                    .map_err(|err| format!("scan limiter closed: {}", err))?;
                let endpoint = fleet.read_endpoint();
                let started = std::time::Instant::now();

                match endpoint.get_balances_batch(&chunk).await {
                    Ok(balances) => {
                        fleet.report_success(endpoint.id, started.elapsed());
                        let mut candidates = Vec::new();

                        for (wallet_addr, native_balance) in chunk.into_iter().zip(balances.into_iter()) {
                            let native_balance_eth = wei_to_eth_f64(native_balance);
                            let native_transferable_wei =
                                native_balance.saturating_sub(min_native_reserve);

                            if native_transferable_wei.is_zero() {
                                if config.hot_path_info_events {
                                    debug!(
                                        "wallet {} native balance {:.6} ETH below safety reserve {:.6} ETH",
                                        wallet_addr, native_balance_eth, wei_to_eth_f64(min_native_reserve)
                                    );
                                }
                                continue;
                            }

                            let operation = detect_wallet_operation(
                                endpoint.provider.clone(),
                                wallet_addr,
                                config.mock_contract_mode,
                            )
                            .await;
                            let estimated_operation_cost_wei =
                                estimated_total_cost_wei(
                                    &config,
                                    cycle_gas_price,
                                    &ResidualCandidate {
                                        wallet: wallet_addr,
                                        operation,
                                        requires_approve: false,
                                        approval_tokens: Vec::new(),
                                        native_balance,
                                        token_value_wei: U256::zero(),
                                        stable_token_value_wei: U256::zero(),
                                        other_token_value_wei: U256::zero(),
                                        total_residual_wei: native_transferable_wei,
                                        estimated_net_profit_wei: U256::zero(),
                                        estimated_cost_wei: U256::zero(),
                                        asset_class: AssetClass::Native.as_str().to_string(),
                                        rpc: endpoint.name.clone(),
                                        timestamp: std::time::Instant::now(),
                                    },
                                );
                            let estimated_operation_cost_eth =
                                wei_to_eth_f64(estimated_operation_cost_wei);
                            let requires_wallet_gas = !config.use_external_gas_sponsor;

                            if requires_wallet_gas && native_balance < estimated_operation_cost_wei {
                                continue;
                            }

                            let native_profit_wei = native_transferable_wei
                                .saturating_sub(estimated_operation_cost_wei);
                            let native_profit_eth = wei_to_eth_f64(native_profit_wei);
                            let native_policy = policy_for_asset(&config, AssetClass::Native);
                            let native_min_profit_wei = policy_min_profit_wei(native_policy);
                            let native_roi_bps =
                                compute_roi_bps(native_profit_eth, estimated_operation_cost_eth);

                            if native_policy.enabled
                                && native_profit_wei >= native_min_profit_wei
                                && native_roi_bps >= native_policy.min_roi_bps
                            {
                                info!(
                                    "Residual candidate: wallet={:?} op={} class={} native={:.6} ETH tokens={:.6} ETH total={:.6} ETH cost={:.6} ETH profit={:.6} ETH ROI={} bps",
                                    wallet_addr,
                                    operation.as_str(),
                                    AssetClass::Native.as_str(),
                                    native_balance_eth,
                                    0.0,
                                    wei_to_eth_f64(native_transferable_wei),
                                    estimated_operation_cost_eth,
                                    native_profit_eth,
                                    native_roi_bps
                                );

                                candidates.push(ResidualCandidate {
                                    wallet: wallet_addr,
                                    operation,
                                    requires_approve: false,
                                    approval_tokens: Vec::new(),
                                    native_balance,
                                    token_value_wei: U256::zero(),
                                    stable_token_value_wei: U256::zero(),
                                    other_token_value_wei: U256::zero(),
                                    total_residual_wei: native_transferable_wei,
                                    estimated_net_profit_wei: native_profit_wei,
                                    estimated_cost_wei: estimated_operation_cost_wei,
                                    asset_class: AssetClass::Native.as_str().to_string(),
                                    rpc: endpoint.name.clone(),
                                    timestamp: std::time::Instant::now(),
                                });
                                continue;
                            }

                            if !should_enrich_tokens {
                                continue;
                            }

                            let token_portfolio = estimate_token_portfolio_wei(
                                endpoint.provider.clone(),
                                wallet_addr,
                                &config.monitored_tokens,
                            )
                            .await;
                            let token_value_wei = token_portfolio.total_wei;
                            if token_value_wei.is_zero() {
                                continue;
                            }

                            let total_residual_wei = native_balance.saturating_add(token_value_wei);
                            let token_value_eth = wei_to_eth_f64(token_value_wei);
                            let estimated_net_profit_wei =
                                total_residual_wei.saturating_sub(estimated_operation_cost_wei);
                            if token_value_eth > native_balance_eth * config.max_token_value_ratio {
                                if config.hot_path_info_events {
                                    debug!(
                                        "wallet {} token/native ratio {:.4} exceeds limit {:.4}",
                                        wallet_addr,
                                        token_value_eth / native_balance_eth,
                                        config.max_token_value_ratio
                                    );
                                }
                                continue;
                            }

                            let asset_class = classify_candidate_asset(&token_portfolio);
                            let requires_approve = !token_portfolio.approval_tokens.is_empty();
                            let candidate_template = ResidualCandidate {
                                wallet: wallet_addr,
                                operation,
                                requires_approve,
                                approval_tokens: token_portfolio.approval_tokens.clone(),
                                native_balance,
                                token_value_wei,
                                stable_token_value_wei: token_portfolio.stable_wei,
                                other_token_value_wei: token_portfolio.other_wei,
                                total_residual_wei,
                                estimated_net_profit_wei: U256::zero(),
                                estimated_cost_wei: U256::zero(),
                                asset_class: asset_class.as_str().to_string(),
                                rpc: endpoint.name.clone(),
                                timestamp: std::time::Instant::now(),
                            };
                            let estimated_operation_cost_wei =
                                estimated_total_cost_wei(&config, cycle_gas_price, &candidate_template);
                            let estimated_operation_cost_eth =
                                wei_to_eth_f64(estimated_operation_cost_wei);
                            let estimated_net_profit_wei =
                                total_residual_wei.saturating_sub(estimated_operation_cost_wei);
                            let estimated_net_profit_eth = wei_to_eth_f64(estimated_net_profit_wei);
                            let policy = policy_for_asset(&config, asset_class);
                            let min_net_profit_wei = policy_min_profit_wei(policy);
                            let roi_bps =
                                compute_roi_bps(estimated_net_profit_eth, estimated_operation_cost_eth);
                            if policy.enabled
                                && estimated_net_profit_wei >= min_net_profit_wei
                                && roi_bps >= policy.min_roi_bps
                            {
                                candidates.push(ResidualCandidate {
                                    wallet: wallet_addr,
                                    operation,
                                    requires_approve,
                                    approval_tokens: token_portfolio.approval_tokens,
                                    native_balance,
                                    token_value_wei,
                                    stable_token_value_wei: token_portfolio.stable_wei,
                                    other_token_value_wei: token_portfolio.other_wei,
                                    total_residual_wei,
                                    estimated_net_profit_wei,
                                    estimated_cost_wei: estimated_operation_cost_wei,
                                    asset_class: asset_class.as_str().to_string(),
                                    rpc: endpoint.name.clone(),
                                    timestamp: std::time::Instant::now(),
                                });
                            }
                        }

                        Ok::<Vec<ResidualCandidate>, String>(candidates)
                    }
                    Err(batch_err) => {
                        let batch_err_text = batch_err.to_string();
                        fleet.report_provider_error(endpoint.id, &batch_err_text);
                        let mut candidates = Vec::new();

                        for wallet_addr in chunk {
                            let individual_started = std::time::Instant::now();
                            match endpoint.provider.get_balance(wallet_addr, None).await {
                                Ok(native_balance) => {
                                    fleet.report_success(endpoint.id, individual_started.elapsed());
                                    let native_balance_eth = wei_to_eth_f64(native_balance);
                                    let native_transferable_wei =
                                        native_balance.saturating_sub(min_native_reserve);

                                    if native_transferable_wei.is_zero() {
                                        continue;
                                    }

                                    let operation = detect_wallet_operation(
                                        endpoint.provider.clone(),
                                        wallet_addr,
                                        config.mock_contract_mode,
                                    )
                                    .await;
                                    let estimated_operation_cost_wei = estimated_total_cost_wei(
                                        &config,
                                        cycle_gas_price,
                                        &ResidualCandidate {
                                            wallet: wallet_addr,
                                            operation,
                                            requires_approve: false,
                                            approval_tokens: Vec::new(),
                                            native_balance,
                                            token_value_wei: U256::zero(),
                                            stable_token_value_wei: U256::zero(),
                                            other_token_value_wei: U256::zero(),
                                            total_residual_wei: native_transferable_wei,
                                            estimated_net_profit_wei: U256::zero(),
                                            estimated_cost_wei: U256::zero(),
                                            asset_class: AssetClass::Native.as_str().to_string(),
                                            rpc: endpoint.name.clone(),
                                            timestamp: std::time::Instant::now(),
                                        },
                                    );
                                    let estimated_operation_cost_eth =
                                        wei_to_eth_f64(estimated_operation_cost_wei);
                                    let requires_wallet_gas = !config.use_external_gas_sponsor;

                                    if requires_wallet_gas
                                        && native_balance < estimated_operation_cost_wei
                                    {
                                        continue;
                                    }

                                    let native_profit_wei = native_transferable_wei
                                        .saturating_sub(estimated_operation_cost_wei);
                                    let native_profit_eth = wei_to_eth_f64(native_profit_wei);
                                    let native_policy = policy_for_asset(&config, AssetClass::Native);
                                    let native_min_profit_wei = policy_min_profit_wei(native_policy);
                                    let native_roi_bps = compute_roi_bps(
                                        native_profit_eth,
                                        estimated_operation_cost_eth,
                                    );

                                    if native_policy.enabled
                                        && native_profit_wei >= native_min_profit_wei
                                        && native_roi_bps >= native_policy.min_roi_bps
                                    {
                                        candidates.push(ResidualCandidate {
                                            wallet: wallet_addr,
                                            operation,
                                            requires_approve: false,
                                            approval_tokens: Vec::new(),
                                            native_balance,
                                            token_value_wei: U256::zero(),
                                            stable_token_value_wei: U256::zero(),
                                            other_token_value_wei: U256::zero(),
                                            total_residual_wei: native_transferable_wei,
                                            estimated_net_profit_wei: native_profit_wei,
                                            estimated_cost_wei: estimated_operation_cost_wei,
                                            asset_class: AssetClass::Native.as_str().to_string(),
                                            rpc: endpoint.name.clone(),
                                            timestamp: std::time::Instant::now(),
                                        });
                                        continue;
                                    }

                                    if !should_enrich_tokens {
                                        continue;
                                    }

                                    let token_portfolio = estimate_token_portfolio_wei(
                                        endpoint.provider.clone(),
                                        wallet_addr,
                                        &config.monitored_tokens,
                                    )
                                    .await;
                                    let token_value_wei = token_portfolio.total_wei;
                                    if token_value_wei.is_zero() {
                                        continue;
                                    }

                                    let total_residual_wei =
                                        native_balance.saturating_add(token_value_wei);
                                    let estimated_net_profit_wei =
                                        total_residual_wei
                                            .saturating_sub(estimated_operation_cost_wei);
                                    let token_value_eth = wei_to_eth_f64(token_value_wei);

                                    if token_value_eth > native_balance_eth * config.max_token_value_ratio {
                                        continue;
                                    }

                                    let asset_class = classify_candidate_asset(&token_portfolio);
                                    let requires_approve = !token_portfolio.approval_tokens.is_empty();
                                    let candidate_template = ResidualCandidate {
                                        wallet: wallet_addr,
                                        operation,
                                        requires_approve,
                                        approval_tokens: token_portfolio.approval_tokens.clone(),
                                        native_balance,
                                        token_value_wei,
                                        stable_token_value_wei: token_portfolio.stable_wei,
                                        other_token_value_wei: token_portfolio.other_wei,
                                        total_residual_wei,
                                        estimated_net_profit_wei: U256::zero(),
                                        estimated_cost_wei: U256::zero(),
                                        asset_class: asset_class.as_str().to_string(),
                                        rpc: endpoint.name.clone(),
                                        timestamp: std::time::Instant::now(),
                                    };
                                    let estimated_operation_cost_wei = estimated_total_cost_wei(
                                        &config,
                                        cycle_gas_price,
                                        &candidate_template,
                                    );
                                    let estimated_operation_cost_eth =
                                        wei_to_eth_f64(estimated_operation_cost_wei);
                                    let estimated_net_profit_wei =
                                        total_residual_wei
                                            .saturating_sub(estimated_operation_cost_wei);
                                    let estimated_net_profit_eth =
                                        wei_to_eth_f64(estimated_net_profit_wei);
                                    let policy = policy_for_asset(&config, asset_class);
                                    let min_net_profit_wei = policy_min_profit_wei(policy);
                                    let roi_bps = compute_roi_bps(
                                        estimated_net_profit_eth,
                                        estimated_operation_cost_eth,
                                    );

                                    if policy.enabled
                                        && estimated_net_profit_wei >= min_net_profit_wei
                                        && roi_bps >= policy.min_roi_bps
                                    {
                                        candidates.push(ResidualCandidate {
                                            wallet: wallet_addr,
                                            operation,
                                            requires_approve,
                                            approval_tokens: token_portfolio.approval_tokens,
                                            native_balance,
                                            token_value_wei,
                                            stable_token_value_wei: token_portfolio.stable_wei,
                                            other_token_value_wei: token_portfolio.other_wei,
                                            total_residual_wei,
                                            estimated_net_profit_wei,
                                            estimated_cost_wei: estimated_operation_cost_wei,
                                            asset_class: asset_class.as_str().to_string(),
                                            rpc: endpoint.name.clone(),
                                            timestamp: std::time::Instant::now(),
                                        });
                                    }
                                }
                                Err(err) => {
                                    fleet.report_provider_error(endpoint.id, &err.to_string());
                                }
                            }
                        }
                        Ok(candidates)
                    }
                }
            });
        }

        // Coleta todos os candidatos das tasks
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok(candidates)) => {
                    for candidate in candidates {
                        if injected_mock_wallets.contains(&candidate.wallet) {
                            continue;
                        }
                        let candidate_profit_wei = candidate.estimated_net_profit_wei;
                        hot_wallet_snapshots.push(WalletSnapshot {
                            address: format!("{:?}", candidate.wallet),
                            balance_eth: wei_to_eth_f64(candidate.total_residual_wei).to_string(),
                            rpc: candidate.rpc.clone(),
                        });

                        if queue_residual_candidate(
                            &mut wallet_states,
                            &mut sweep_queue,
                            &dashboard,
                            candidate,
                            now,
                            wallet_cooldown,
                            scan_started.elapsed().as_millis(),
                            config.hot_path_info_events,
                        ) {
                            queued_hot_wallets += 1;
                            queued_profit_wei =
                                queued_profit_wei.saturating_add(candidate_profit_wei);
                        }
                    }
                    dispatch_ready_sweeps(
                        &mut active_sweeps,
                        &mut sweep_queue,
                        &mut wallet_states,
                        &wallet_lookup,
                        &runtime_cache,
                        &rpc_fleet,
                        &config,
                        cycle_gas_price,
                        &dashboard,
                    )
                    .await?;
                }
                Ok(Err(err)) => {
                    dashboard.event("warn", err.clone());
                    warn!("{}", err);
                }
                Err(err) => {
                    dashboard.event("warn", format!("balance scan task join error: {}", err));
                    warn!("balance scan task join error: {}", err);
                }
            }
        }

        let hot_wallet_count = hot_wallet_snapshots.len();
        dashboard.update_scan(
            scan_started.elapsed().as_millis(),
            hot_wallet_snapshots,
            rpc_fleet.snapshot(),
        );
        dashboard.record_latency("scan_cycle", scan_started.elapsed().as_millis(), None, None);

        if queued_hot_wallets == 0 && sweep_queue.len() == 0 {
            continue;
        }

        let total_profit_eth = wei_to_eth_f64(queued_profit_wei);
        info!(
            "Found {} residual candidates with total profit ~{:.6} ETH",
            queued_hot_wallets.max(hot_wallet_count),
            total_profit_eth
        );

        dispatch_ready_sweeps(
            &mut active_sweeps,
            &mut sweep_queue,
            &mut wallet_states,
            &wallet_lookup,
            &runtime_cache,
            &rpc_fleet,
            &config,
            cycle_gas_price,
            &dashboard,
        )
        .await?;
    }
}

async fn estimate_token_value_wei(
    provider: Arc<Provider<Http>>,
    wallet_addr: Address,
    monitored_tokens: &[crate::config::MonitoredTokenConfig],
) -> U256 {
    estimate_token_portfolio_wei(provider, wallet_addr, monitored_tokens)
        .await
        .total_wei
}

#[derive(Default)]
struct TokenPortfolioValue {
    total_wei: U256,
    stable_wei: U256,
    other_wei: U256,
    approval_tokens: Vec<Address>,
}

async fn estimate_token_portfolio_wei(
    provider: Arc<Provider<Http>>,
    wallet_addr: Address,
    monitored_tokens: &[crate::config::MonitoredTokenConfig],
) -> TokenPortfolioValue {
    let mut portfolio = TokenPortfolioValue::default();

    for token in monitored_tokens {
        let contract = ERC20Token::new(token.address, provider.clone());
        if let Ok(balance) = contract.balance_of(wallet_addr).call().await {
            if balance.is_zero() {
                continue;
            }

            let decimals_factor = 10f64.powi(i32::from(token.decimals));
            let normalized = balance.to_string().parse::<f64>().unwrap_or(0.0) / decimals_factor;
            let value_eth = normalized * token.price_eth;
            if let Ok(value_wei) = ethers::utils::parse_ether(value_eth.to_string()) {
                portfolio.total_wei = portfolio.total_wei.saturating_add(value_wei);
                portfolio.approval_tokens.push(token.address);
                match token.asset_class {
                    AssetClass::Stable => {
                        portfolio.stable_wei = portfolio.stable_wei.saturating_add(value_wei);
                    }
                    AssetClass::OtherToken | AssetClass::Native => {
                        portfolio.other_wei = portfolio.other_wei.saturating_add(value_wei);
                    }
                }
            }
        }
    }

    portfolio
}

fn wei_to_eth_f64(wei: U256) -> f64 {
    let wei_str = wei.to_string();
    let eth_dec = wei_str.parse::<f64>().unwrap_or(0.0) / 1e18;
    eth_dec
}

async fn detect_wallet_operation(
    provider: Arc<Provider<Http>>,
    wallet_addr: Address,
    mock_contract_mode: bool,
) -> OperationType {
    if mock_contract_mode {
        return OperationType::Exec;
    }

    match provider.get_code(wallet_addr, None).await {
        Ok(code) if code.as_ref().is_empty() => OperationType::Install,
        Ok(_) => OperationType::Exec,
        Err(_) => OperationType::Install,
    }
}

fn queue_residual_candidate(
    wallet_states: &mut HashMap<Address, WalletExecutionState>,
    sweep_queue: &mut SweepQueue,
    dashboard: &DashboardHandle,
    candidate: ResidualCandidate,
    now: std::time::Instant,
    wallet_cooldown: Duration,
    enqueue_latency_ms: u128,
    hot_path_info_events: bool,
) -> bool {
    let state = wallet_states.entry(candidate.wallet).or_default();
    if state.processing {
        return false;
    }
    if let Some(last_attempt) = state.last_attempt {
        let elapsed = now.saturating_duration_since(last_attempt);
        if elapsed < wallet_cooldown {
            let remaining = wallet_cooldown.saturating_sub(elapsed).as_secs();
            if hot_path_info_events {
                dashboard.event(
                    "info",
                    format!(
                        "wallet {:?} skipped due to cooldown, {}s remaining",
                        candidate.wallet, remaining
                    ),
                );
            }
            return false;
        }
    }
    if let Some(backoff_until) = state.backoff_until {
        if backoff_until > now {
            return false;
        }
    }

    let profit_eth = wei_to_eth_f64(candidate.estimated_net_profit_wei);
    let roi_bps = if candidate.estimated_cost_wei > U256::zero() {
        (profit_eth / wei_to_eth_f64(candidate.estimated_cost_wei) * 10000.0) as u64
    } else {
        0
    };

    info!(
        "Queueing residual candidate: wallet={:?} op={} class={} profit={:.6} ETH ROI={} bps via {}",
        candidate.wallet,
        candidate.operation.as_str(),
        candidate.asset_class,
        profit_eth,
        roi_bps,
        candidate.rpc
    );
    let rpc = candidate.rpc.clone();
    let wallet = candidate.wallet;
    let operation = candidate.operation;
    let asset_class = candidate.asset_class.clone();
    let total_residual_wei = candidate.total_residual_wei;
    let estimated_net_profit_wei = candidate.estimated_net_profit_wei;
    let queued = sweep_queue.enqueue_prioritized(candidate, rpc.clone());

    if queued {
        dashboard.record_latency(
            "enqueue_latency",
            enqueue_latency_ms,
            Some(&format!("{:?}", wallet)),
            Some(&rpc),
        );
        let is_small_positive = profit_eth <= config_small_positive_threshold();
        dashboard.record_residual_detection(
            &format!("{:?}", wallet),
            &asset_class,
            total_residual_wei,
            estimated_net_profit_wei,
            is_small_positive,
        );
        if hot_path_info_events {
            dashboard.event(
                "info",
                format!(
                    "residual candidate {:?} op={} queued (profit={:.6} ETH ROI={} bps), queue size {}",
                    wallet,
                    operation.as_str(),
                    profit_eth,
                    roi_bps,
                    sweep_queue.len()
                ),
            );
        }
        true
    } else {
        if hot_path_info_events {
            dashboard.event(
                "info",
                format!("candidate {:?} not re-queued due to dedupe window", wallet),
            );
        }
        false
    }
}

fn classify_candidate_asset(token_portfolio: &TokenPortfolioValue) -> AssetClass {
    if token_portfolio.total_wei.is_zero() {
        AssetClass::Native
    } else if token_portfolio.other_wei.is_zero() {
        AssetClass::Stable
    } else {
        AssetClass::OtherToken
    }
}

fn policy_for_asset(config: &Config, asset_class: AssetClass) -> AssetPolicy {
    match asset_class {
        AssetClass::Native => config.native_policy,
        AssetClass::Stable => config.stable_policy,
        AssetClass::OtherToken => config.other_token_policy,
    }
}

fn candidate_asset_class(asset_class: &str) -> AssetClass {
    match asset_class {
        "stable" => AssetClass::Stable,
        "other-token" => AssetClass::OtherToken,
        _ => AssetClass::Native,
    }
}

fn policy_min_profit_wei(policy: AssetPolicy) -> U256 {
    ethers::utils::parse_ether(policy.min_net_profit_eth.to_string())
        .unwrap_or_else(|_| U256::zero())
}

fn compute_roi_bps(net_profit_eth: f64, cost_eth: f64) -> u64 {
    if cost_eth > 0.0 {
        (net_profit_eth / cost_eth * 10000.0) as u64
    } else {
        0
    }
}

fn config_small_positive_threshold() -> f64 {
    0.003
}

async fn dispatch_ready_sweeps(
    active_sweeps: &mut JoinSet<SweepTaskResult>,
    sweep_queue: &mut SweepQueue,
    wallet_states: &mut HashMap<Address, WalletExecutionState>,
    wallet_lookup: &HashMap<Address, LocalWallet>,
    runtime_cache: &Arc<RuntimeCache>,
    rpc_fleet: &Arc<RpcFleet>,
    config: &Arc<Config>,
    cycle_gas_price: U256,
    dashboard: &DashboardHandle,
) -> Result<(), Box<dyn std::error::Error>> {
    let final_gas_price =
        if config.bot_mode == crate::config::BotMode::Shadow && config.mock_contract_mode {
            cycle_gas_price
        } else {
            match runtime_cache
                .gas_price(rpc_fleet.read_endpoint().provider.clone(), &config.network)
                .await
            {
                Ok(price) => price,
                Err(_) => cycle_gas_price,
            }
        };
    while active_sweeps.len() < config.queue_workers.max(1) {
        let Some(job) = sweep_queue.pop() else {
            break;
        };

        let candidate = &job.candidate;
        let wallet_addr = candidate.wallet;
        let final_cost_wei = estimated_total_cost_wei(config, final_gas_price, candidate);

        dashboard.record_latency(
            "queue_wait",
            job.enqueued_at.elapsed().as_millis(),
            Some(&format!("{:?}", wallet_addr)),
            Some(&job.rpc),
        );

        let Some(wallet) = wallet_lookup.get(&wallet_addr).cloned() else {
            dashboard.event(
                "error",
                format!("wallet {:?} missing from lookup", wallet_addr),
            );
            continue;
        };

        if candidate.total_residual_wei < final_cost_wei {
            dashboard.event(
                "warn",
                format!(
                    "candidate {:?} op={} no longer profitable (gas increased), skipping",
                    wallet_addr,
                    candidate.operation.as_str()
                ),
            );
            sweep_queue.finish(wallet_addr);
            continue;
        }

        let final_profit_wei = candidate.total_residual_wei.saturating_sub(final_cost_wei);
        let final_policy = policy_for_asset(config, candidate_asset_class(&candidate.asset_class));
        let final_min_profit_wei = policy_min_profit_wei(final_policy);
        if final_profit_wei < final_min_profit_wei {
            dashboard.event(
                "warn",
                format!(
                    "candidate {:?} below minimum net profit after recheck ({:.6} ETH < {:.6} ETH), skipping",
                    wallet_addr,
                    wei_to_eth_f64(final_profit_wei),
                    final_policy.min_net_profit_eth
                ),
            );
            sweep_queue.finish(wallet_addr);
            continue;
        }

        if final_profit_wei < candidate.estimated_net_profit_wei / U256::from(2u64) {
            dashboard.event(
                "warn",
                format!(
                    "candidate {:?} profit dropped significantly ({:.6} ETH -> {:.6} ETH), re-evaluating",
                    wallet_addr,
                    wei_to_eth_f64(candidate.estimated_net_profit_wei),
                    wei_to_eth_f64(final_profit_wei)
                ),
            );
            sweep_queue.finish(wallet_addr);
            continue;
        }

        {
            let state = wallet_states.entry(wallet_addr).or_default();
            state.processing = true;
            state.last_attempt = Some(std::time::Instant::now());
        }

        dashboard.mark_sweep_attempt(&format!("{:?}", wallet_addr), &job.rpc);
        let candidate = candidate.clone();
        let rpc_name = job.rpc.clone();
        let config = config.clone();
        let rpc_fleet = rpc_fleet.clone();
        let runtime_cache = runtime_cache.clone();
        let dashboard = dashboard.clone();

        active_sweeps.spawn(async move {
            let result = extractor::execute_job(
                wallet,
                candidate,
                config.contract,
                &config,
                rpc_fleet,
                runtime_cache,
                dashboard,
            )
            .await
            .map_err(|err| err.to_string());
            SweepTaskResult {
                wallet: wallet_addr,
                rpc: rpc_name,
                result,
            }
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_native_candidate() {
        let portfolio = TokenPortfolioValue::default();
        assert_eq!(classify_candidate_asset(&portfolio), AssetClass::Native);
    }

    #[test]
    fn classifies_stable_candidate() {
        let portfolio = TokenPortfolioValue {
            total_wei: U256::from(100u64),
            stable_wei: U256::from(100u64),
            other_wei: U256::zero(),
            approval_tokens: Vec::new(),
        };
        assert_eq!(classify_candidate_asset(&portfolio), AssetClass::Stable);
    }

    #[test]
    fn classifies_other_token_candidate() {
        let portfolio = TokenPortfolioValue {
            total_wei: U256::from(150u64),
            stable_wei: U256::from(100u64),
            other_wei: U256::from(50u64),
            approval_tokens: Vec::new(),
        };
        assert_eq!(classify_candidate_asset(&portfolio), AssetClass::OtherToken);
    }

    #[test]
    fn computes_roi_bps_from_profit_and_cost() {
        assert_eq!(compute_roi_bps(0.002, 0.001), 20_000);
        assert_eq!(compute_roi_bps(0.0, 0.001), 0);
        assert_eq!(compute_roi_bps(0.001, 0.0), 0);
    }

    #[test]
    fn small_positive_threshold_matches_micro_residue_model() {
        assert!(config_small_positive_threshold() <= 0.003);
        assert!(0.0025 <= config_small_positive_threshold());
    }
}
