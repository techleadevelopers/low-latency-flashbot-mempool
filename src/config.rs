use clap::Parser;
use ethers::types::Address;
use serde::Deserialize;
use std::collections::HashSet;
use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "mev-sweeper")]
#[command(author = "GhostInject")]
#[command(version = "1.0")]
#[command(about = "Corporate Residual Sweeper - High-performance MEV sweeper with Flashbots")]
struct Cli {
    /// Path to keys.txt or wallets.json
    #[arg(short, long, default_value = "keys.txt")]
    wallets: PathBuf,

    /// Network (arbitrum, bsc, ethereum)
    #[arg(long, default_value = "ethereum")]
    network: String,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub wallets: PathBuf,
    pub network: String,
    pub chain_id: u64,
    pub bot_mode: BotMode,
    pub allow_send: bool,
    pub test_wallet_allowlist: HashSet<Address>,
    pub max_sweep_value_eth: Option<f64>,
    pub mock_contract_mode: bool,
    pub mock_hot_wallet_count: usize,
    pub mock_hot_balance_eth: f64,
    pub mock_hot_wallet_mode: MockHotWalletMode,
    pub mock_hot_wallet_rounds: usize,
    pub alchemy_key: String,
    pub infura_ids: Vec<String>,
    pub flashbots_relay: String,
    pub sender_private_key: String,
    pub contract: Address,
    pub forwarder: Address,
    pub control_address: Address,
    pub monitored_tokens: Vec<MonitoredTokenConfig>,
    pub min_balance: f64, // AGORA: reserva mínima de segurança (não mais gatilho)
    pub min_net_profit_eth: f64, // AGORA: lucro mínimo para executar sweep
    pub estimated_sweep_gas: u64,
    pub estimated_install_gas: u64,
    pub estimated_approve_gas: u64,
    pub estimated_exec_gas: u64,
    pub estimated_bundle_overhead_gas: u64,
    pub profit_margin_bps: u64, // DEPRECATED: será ignorado na nova lógica
    pub interval: u64,
    pub min_scan_interval_ms: u64,
    pub max_scan_interval_ms: u64,
    pub scan_concurrency: usize,
    pub batch_scan_size: usize,
    pub queue_workers: usize,
    pub hot_path_info_events: bool,
    pub wallet_cooldown_secs: u64,
    pub queue_dedupe_secs: u64,
    pub wallet_failure_backoff_base_secs: u64,
    pub wallet_failure_backoff_max_secs: u64,
    pub rpc_stale_threshold_secs: u64,
    pub rpc_rate_limit_cooldown_secs: u64,
    pub max_infura_endpoints: usize,
    pub rpc_read_preference: RpcPreference,
    pub rpc_send_preference: RpcPreference,
    pub contract_cache_ttl_secs: u64,
    pub gas_price_cache_ttl_secs: u64,
    pub storage_path: PathBuf,
    pub dashboard_addr: SocketAddr,
    // ========== NOVOS CAMPOS PARA CORPORATE RESIDUAL SWEEPER ==========
    pub disable_public_fallback: bool, // Endurecimento: desabilita fallback público em produção
    pub min_roi_bps: u64,              // ROI mínimo em basis points (ex: 500 = 5%)
    pub enable_token_sweep: bool,      // Habilita varredura de tokens
    pub max_token_value_ratio: f64,    // Máximo valor de token relativo ao nativo (ex: 10.0 = 10x)
    pub native_immediate_sweep: bool,
    pub use_external_gas_sponsor: bool,
    pub native_policy: AssetPolicy,
    pub stable_policy: AssetPolicy,
    pub other_token_policy: AssetPolicy,
    pub enable_mempool_monitor: bool,
    pub mempool_ws_url: Option<String>,
    pub frontrun_slippage_bps: u64,
    pub frontrun_gas_bump_bps: u64,
}

#[derive(Debug, Clone)]
pub struct MonitoredTokenConfig {
    pub symbol: String,
    pub address: Address,
    pub decimals: u8,
    pub price_eth: f64,
    pub asset_class: AssetClass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetClass {
    Native,
    Stable,
    OtherToken,
}

impl AssetClass {
    pub fn as_str(self) -> &'static str {
        match self {
            AssetClass::Native => "native",
            AssetClass::Stable => "stable",
            AssetClass::OtherToken => "other-token",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AssetPolicy {
    pub min_net_profit_eth: f64,
    pub min_roi_bps: u64,
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotMode {
    Shadow,
    Paper,
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockHotWalletMode {
    OneShot,
    Continuous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcPreference {
    Auto,
    Alchemy,
    Infura,
}

impl MockHotWalletMode {
    pub fn as_str(self) -> &'static str {
        match self {
            MockHotWalletMode::OneShot => "oneshot",
            MockHotWalletMode::Continuous => "continuous",
        }
    }
}

impl RpcPreference {
    pub fn as_str(self) -> &'static str {
        match self {
            RpcPreference::Auto => "auto",
            RpcPreference::Alchemy => "alchemy",
            RpcPreference::Infura => "infura",
        }
    }
}

impl BotMode {
    pub fn as_str(self) -> &'static str {
        match self {
            BotMode::Shadow => "shadow",
            BotMode::Paper => "paper",
            BotMode::Live => "live",
        }
    }
}

impl Config {
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        dotenvy::dotenv().ok();

        let cli = Cli::parse();
        let network = env::var("NETWORK")
            .ok()
            .map(|value| value.trim().to_lowercase())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| cli.network.to_lowercase());
        let bot_mode = parse_bot_mode(
            env::var("BOT_MODE")
                .unwrap_or_else(|_| "shadow".to_string())
                .trim(),
        )?;
        let allow_send = env::var("ALLOW_SEND")
            .unwrap_or_else(|_| "false".to_string())
            .trim()
            .eq_ignore_ascii_case("true");
        let test_wallet_allowlist =
            parse_address_set(&env::var("TEST_WALLET_ALLOWLIST").unwrap_or_default())?;
        let max_sweep_value_eth = env::var("MAX_SWEEP_VALUE_ETH")
            .ok()
            .map(|value| value.trim().parse::<f64>())
            .transpose()?;
        let mock_contract_mode = env::var("MOCK_CONTRACT_MODE")
            .unwrap_or_else(|_| "false".to_string())
            .trim()
            .eq_ignore_ascii_case("true");
        let mock_hot_wallet_count = env::var("MOCK_HOT_WALLET_COUNT")
            .unwrap_or_else(|_| "0".to_string())
            .parse::<usize>()?;
        let mock_hot_balance_eth = env::var("MOCK_HOT_BALANCE_ETH")
            .unwrap_or_else(|_| "0.011".to_string())
            .parse::<f64>()?;
        let mock_hot_wallet_mode = parse_mock_hot_wallet_mode(
            env::var("MOCK_HOT_WALLET_MODE")
                .unwrap_or_else(|_| "oneshot".to_string())
                .trim(),
        )?;
        let mock_hot_wallet_rounds = env::var("MOCK_HOT_WALLET_ROUNDS")
            .unwrap_or_else(|_| "1".to_string())
            .parse::<usize>()?;
        let alchemy_key = required_env("ALCHEMY_KEY")?;
        let flashbots_relay = required_env("FLASHBOTS_RELAY")?;
        let chain_id = env::var("CHAIN_ID")
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or_else(|| default_chain_id(&network));
        let sender_private_key = env::var("SENDER_PRIVATE_KEY")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !is_placeholder(value))
            .or_else(|| {
                env::var("CONTROL_PRIVATE_KEY")
                    .ok()
                    .map(|value| value.trim().to_string())
                    .filter(|value| !is_placeholder(value))
            })
            .ok_or(
                "environment variable SENDER_PRIVATE_KEY or CONTROL_PRIVATE_KEY is not configured",
            )?;
        let contract = resolve_contract_address(&network)?.parse::<Address>()?;
        let control_address = resolve_control_address()?.parse::<Address>()?;
        let forwarder = resolve_forwarder_address(&network, control_address)?.parse::<Address>()?;
        let monitored_tokens = parse_monitored_tokens(&network)?;

        // ========== CAMPOS EXISTENTES ==========
        let min_balance = env::var("MIN_BALANCE")
            .unwrap_or_else(|_| "0.01".to_string())
            .parse::<f64>()?;
        let min_net_profit_eth = env::var("MIN_NET_PROFIT_ETH")
            .unwrap_or_else(|_| "0.001".to_string())
            .parse::<f64>()?;
        let estimated_sweep_gas = env::var("ESTIMATED_SWEEP_GAS")
            .unwrap_or_else(|_| "250000".to_string())
            .parse::<u64>()?;
        let estimated_install_gas = env::var("ESTIMATED_INSTALL_GAS")
            .unwrap_or_else(|_| "180000".to_string())
            .parse::<u64>()?;
        let estimated_approve_gas = env::var("ESTIMATED_APPROVE_GAS")
            .unwrap_or_else(|_| "65000".to_string())
            .parse::<u64>()?;
        let estimated_exec_gas = env::var("ESTIMATED_EXEC_GAS")
            .ok()
            .map(|value| value.trim().parse::<u64>())
            .transpose()?
            .unwrap_or(estimated_sweep_gas);
        let estimated_bundle_overhead_gas = env::var("ESTIMATED_BUNDLE_OVERHEAD_GAS")
            .unwrap_or_else(|_| "25000".to_string())
            .parse::<u64>()?;
        let profit_margin_bps = env::var("PROFIT_MARGIN_BPS")
            .unwrap_or_else(|_| "12000".to_string())
            .parse::<u64>()?;
        let interval = env::var("SCAN_INTERVAL_MS")
            .unwrap_or_else(|_| "500".to_string())
            .parse::<u64>()?;
        let min_scan_interval_ms = env::var("MIN_SCAN_INTERVAL_MS")
            .unwrap_or_else(|_| interval.to_string())
            .parse::<u64>()?;
        let max_scan_interval_ms = env::var("MAX_SCAN_INTERVAL_MS")
            .unwrap_or_else(|_| "3000".to_string())
            .parse::<u64>()?;
        let scan_concurrency = env::var("SCAN_CONCURRENCY")
            .unwrap_or_else(|_| "16".to_string())
            .parse::<usize>()?;
        let batch_scan_size = env::var("BATCH_SCAN_SIZE")
            .unwrap_or_else(|_| "20".to_string())
            .parse::<usize>()?;
        let queue_workers = env::var("QUEUE_WORKERS")
            .unwrap_or_else(|_| "2".to_string())
            .parse::<usize>()?;
        let hot_path_info_events = env::var("HOT_PATH_INFO_EVENTS")
            .unwrap_or_else(|_| "false".to_string())
            .trim()
            .eq_ignore_ascii_case("true");
        let wallet_cooldown_secs = env::var("WALLET_COOLDOWN_SECS")
            .unwrap_or_else(|_| "20".to_string())
            .parse::<u64>()?;
        let queue_dedupe_secs = env::var("QUEUE_DEDUPE_SECS")
            .unwrap_or_else(|_| "10".to_string())
            .parse::<u64>()?;
        let wallet_failure_backoff_base_secs = env::var("WALLET_FAILURE_BACKOFF_BASE_SECS")
            .unwrap_or_else(|_| "15".to_string())
            .parse::<u64>()?;
        let wallet_failure_backoff_max_secs = env::var("WALLET_FAILURE_BACKOFF_MAX_SECS")
            .unwrap_or_else(|_| "300".to_string())
            .parse::<u64>()?;
        let rpc_stale_threshold_secs = env::var("RPC_STALE_THRESHOLD_SECS")
            .unwrap_or_else(|_| "30".to_string())
            .parse::<u64>()?;
        let rpc_rate_limit_cooldown_secs = env::var("RPC_RATE_LIMIT_COOLDOWN_SECS")
            .unwrap_or_else(|_| "120".to_string())
            .parse::<u64>()?;
        let max_infura_endpoints = env::var("MAX_INFURA_ENDPOINTS")
            .unwrap_or_else(|_| "2".to_string())
            .parse::<usize>()?;
        let rpc_read_preference = parse_rpc_preference(
            env::var("RPC_READ_PREFERENCE")
                .unwrap_or_else(|_| "auto".to_string())
                .trim(),
        )?;
        let rpc_send_preference = parse_rpc_preference(
            env::var("RPC_SEND_PREFERENCE")
                .unwrap_or_else(|_| "auto".to_string())
                .trim(),
        )?;
        let contract_cache_ttl_secs = env::var("CONTRACT_CACHE_TTL_SECS")
            .unwrap_or_else(|_| "15".to_string())
            .parse::<u64>()?;
        let gas_price_cache_ttl_secs = env::var("GAS_PRICE_CACHE_TTL_SECS")
            .unwrap_or_else(|_| "8".to_string())
            .parse::<u64>()?;
        let storage_path = env::var("STORAGE_PATH")
            .unwrap_or_else(|_| "bot_state.sqlite".to_string())
            .into();
        let dashboard_addr = env::var("DASHBOARD_ADDR")
            .unwrap_or_else(|_| "127.0.0.1:8787".to_string())
            .parse::<SocketAddr>()?;

        // ========== NOVOS CAMPOS PARA CORPORATE RESIDUAL SWEEPER ==========
        let disable_public_fallback = env::var("DISABLE_PUBLIC_FALLBACK")
            .unwrap_or_else(|_| "true".to_string()) // Padrão: desabilitado em produção
            .trim()
            .eq_ignore_ascii_case("true");

        let min_roi_bps = env::var("MIN_ROI_BPS")
            .unwrap_or_else(|_| "500".to_string()) // 5% ROI mínimo por padrão
            .parse::<u64>()?;

        let enable_token_sweep = env::var("ENABLE_TOKEN_SWEEP")
            .unwrap_or_else(|_| "true".to_string())
            .trim()
            .eq_ignore_ascii_case("true");

        let max_token_value_ratio = env::var("MAX_TOKEN_VALUE_RATIO")
            .unwrap_or_else(|_| "10.0".to_string()) // Token não pode valer mais que 10x o nativo
            .parse::<f64>()?;
        let native_immediate_sweep = env::var("NATIVE_IMMEDIATE_SWEEP")
            .unwrap_or_else(|_| "true".to_string())
            .trim()
            .eq_ignore_ascii_case("true");
        let use_external_gas_sponsor = env::var("USE_EXTERNAL_GAS_SPONSOR")
            .unwrap_or_else(|_| "true".to_string())
            .trim()
            .eq_ignore_ascii_case("true");
        let native_policy = AssetPolicy {
            min_net_profit_eth: if native_immediate_sweep {
                0.0
            } else {
                env::var("NATIVE_MIN_NET_PROFIT_ETH")
                    .ok()
                    .map(|value| value.trim().parse::<f64>())
                    .transpose()?
                    .unwrap_or(min_net_profit_eth)
            },
            min_roi_bps: if native_immediate_sweep {
                0
            } else {
                env::var("NATIVE_MIN_ROI_BPS")
                    .ok()
                    .map(|value| value.trim().parse::<u64>())
                    .transpose()?
                    .unwrap_or(min_roi_bps)
            },
            enabled: env::var("ENABLE_NATIVE_SWEEP")
                .unwrap_or_else(|_| "true".to_string())
                .trim()
                .eq_ignore_ascii_case("true"),
        };
        let stable_policy = AssetPolicy {
            min_net_profit_eth: env::var("STABLE_MIN_NET_PROFIT_ETH")
                .ok()
                .map(|value| value.trim().parse::<f64>())
                .transpose()?
                .unwrap_or(min_net_profit_eth),
            min_roi_bps: env::var("STABLE_MIN_ROI_BPS")
                .ok()
                .map(|value| value.trim().parse::<u64>())
                .transpose()?
                .unwrap_or(min_roi_bps),
            enabled: env::var("ENABLE_STABLE_SWEEP")
                .unwrap_or_else(|_| enable_token_sweep.to_string())
                .trim()
                .eq_ignore_ascii_case("true"),
        };
        let other_token_policy = AssetPolicy {
            min_net_profit_eth: env::var("OTHER_TOKEN_MIN_NET_PROFIT_ETH")
                .ok()
                .map(|value| value.trim().parse::<f64>())
                .transpose()?
                .unwrap_or(min_net_profit_eth),
            min_roi_bps: env::var("OTHER_TOKEN_MIN_ROI_BPS")
                .ok()
                .map(|value| value.trim().parse::<u64>())
                .transpose()?
                .unwrap_or(min_roi_bps),
            enabled: env::var("ENABLE_OTHER_TOKEN_SWEEP")
                .unwrap_or_else(|_| enable_token_sweep.to_string())
                .trim()
                .eq_ignore_ascii_case("true"),
        };
        let enable_mempool_monitor = env::var("ENABLE_MEMPOOL_MONITOR")
            .unwrap_or_else(|_| "false".to_string())
            .trim()
            .eq_ignore_ascii_case("true");
        let mempool_ws_url = env::var("MEMPOOL_WS_URL")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let frontrun_slippage_bps = env::var("FRONTRUN_SLIPPAGE_BPS")
            .unwrap_or_else(|_| "100".to_string())
            .parse::<u64>()?;
        let frontrun_gas_bump_bps = env::var("FRONTRUN_GAS_BUMP_BPS")
            .unwrap_or_else(|_| "11000".to_string())
            .parse::<u64>()?;

        let mut infura_ids = Vec::new();
        for idx in 1..=10 {
            if let Ok(value) = env::var(format!("INFURA_ID_{idx}")) {
                let trimmed = value.trim();
                if !trimmed.is_empty() {
                    infura_ids.push(trimmed.to_string());
                }
            }
        }

        Ok(Self {
            wallets: cli.wallets,
            network,
            chain_id,
            bot_mode,
            allow_send,
            test_wallet_allowlist,
            max_sweep_value_eth,
            mock_contract_mode,
            mock_hot_wallet_count,
            mock_hot_balance_eth,
            mock_hot_wallet_mode,
            mock_hot_wallet_rounds,
            alchemy_key,
            infura_ids,
            flashbots_relay,
            sender_private_key,
            contract,
            forwarder,
            control_address,
            monitored_tokens,
            min_balance,
            min_net_profit_eth,
            estimated_sweep_gas,
            estimated_install_gas,
            estimated_approve_gas,
            estimated_exec_gas,
            estimated_bundle_overhead_gas,
            profit_margin_bps,
            interval,
            min_scan_interval_ms,
            max_scan_interval_ms,
            scan_concurrency,
            batch_scan_size,
            queue_workers,
            hot_path_info_events,
            wallet_cooldown_secs,
            queue_dedupe_secs,
            wallet_failure_backoff_base_secs,
            wallet_failure_backoff_max_secs,
            rpc_stale_threshold_secs,
            rpc_rate_limit_cooldown_secs,
            max_infura_endpoints,
            rpc_read_preference,
            rpc_send_preference,
            contract_cache_ttl_secs,
            gas_price_cache_ttl_secs,
            storage_path,
            dashboard_addr,
            // NOVOS CAMPOS
            disable_public_fallback,
            min_roi_bps,
            enable_token_sweep,
            max_token_value_ratio,
            native_immediate_sweep,
            use_external_gas_sponsor,
            native_policy,
            stable_policy,
            other_token_policy,
            enable_mempool_monitor,
            mempool_ws_url,
            frontrun_slippage_bps,
            frontrun_gas_bump_bps,
        })
    }

    pub fn rpc_urls(&self) -> Vec<(String, String)> {
        let mut urls = Vec::with_capacity(self.infura_ids.len() + 1);
        if let Some(alchemy_url) = alchemy_url_for_network(&self.network, &self.alchemy_key) {
            urls.push(("alchemy-primary".to_string(), alchemy_url));
        }

        for (idx, infura_id) in self
            .infura_ids
            .iter()
            .take(self.max_infura_endpoints)
            .enumerate()
        {
            if let Some(infura_url) = infura_url_for_network(&self.network, infura_id) {
                urls.push((format!("infura-{}", idx + 1), infura_url));
            }
        }

        urls
    }

    // ========== MÉTODO AUXILIAR PARA LOG ==========
    pub fn log_config_status(&self) {
        println!("=== Corporate Residual Sweeper Configuration ===");
        println!("Network: {}", self.network);
        println!("Chain ID: {}", self.chain_id);
        println!("Bot Mode: {}", self.bot_mode.as_str());
        println!("Allow Send: {}", self.allow_send);
        println!("Disable Public Fallback: {}", self.disable_public_fallback);
        println!("Min Balance (safety reserve): {} ETH", self.min_balance);
        println!("Min Net Profit: {} ETH", self.min_net_profit_eth);
        println!(
            "Estimated Gas: install={} approve={} exec={} bundle_overhead={} legacy_sweep={}",
            self.estimated_install_gas,
            self.estimated_approve_gas,
            self.estimated_exec_gas,
            self.estimated_bundle_overhead_gas,
            self.estimated_sweep_gas
        );
        println!(
            "Min ROI: {} bps ({}%)",
            self.min_roi_bps,
            self.min_roi_bps as f64 / 100.0
        );
        println!("Enable Token Sweep: {}", self.enable_token_sweep);
        println!("Max Token/Value Ratio: {}", self.max_token_value_ratio);
        println!(
            "RPC preference read/send: {}/{}",
            self.rpc_read_preference.as_str(),
            self.rpc_send_preference.as_str()
        );
        println!(
            "Policies: native(min_profit={} roi={} enabled={}) stable(min_profit={} roi={} enabled={}) other(min_profit={} roi={} enabled={})",
            self.native_policy.min_net_profit_eth,
            self.native_policy.min_roi_bps,
            self.native_policy.enabled,
            self.stable_policy.min_net_profit_eth,
            self.stable_policy.min_roi_bps,
            self.stable_policy.enabled,
            self.other_token_policy.min_net_profit_eth,
            self.other_token_policy.min_roi_bps,
            self.other_token_policy.enabled
        );
        println!("Monitored Tokens: {}", self.monitored_tokens.len());
        println!("Mempool Monitor: {}", self.enable_mempool_monitor);
        println!("Frontrun slippage bps: {}", self.frontrun_slippage_bps);
        println!("Frontrun gas bump bps: {}", self.frontrun_gas_bump_bps);
        println!("===============================================");
    }

    pub fn mempool_ws_url(&self) -> Option<String> {
        self.mempool_ws_url
            .clone()
            .or_else(|| alchemy_ws_url_for_network(&self.network, &self.alchemy_key))
    }
}

#[derive(Debug, Deserialize)]
pub struct WalletEntry {
    pub private_key: String,
}

fn required_env(name: &str) -> Result<String, Box<dyn std::error::Error>> {
    let value = env::var(name)?;
    let trimmed = value.trim();
    if is_placeholder(trimmed) {
        return Err(format!("environment variable {name} is not configured").into());
    }
    Ok(trimmed.to_string())
}

fn resolve_contract_address(network: &str) -> Result<String, Box<dyn std::error::Error>> {
    let network_key = match network {
        "arbitrum" => "DELEGATE_CONTRACT_ARBITRUM",
        "bsc" => "DELEGATE_CONTRACT_BSC",
        "polygon" => "DELEGATE_CONTRACT_POLYGON",
        "ethereum" => "CONTRACT_ADDRESS",
        other => {
            return Err(format!("unsupported network in NETWORK: {other}").into());
        }
    };

    if let Ok(value) = env::var(network_key) {
        let trimmed = value.trim();
        if !is_placeholder(trimmed) {
            return Ok(trimmed.to_string());
        }
    }

    required_env("CONTRACT_ADDRESS")
}

fn resolve_control_address() -> Result<String, Box<dyn std::error::Error>> {
    required_env("CONTROL_ADDRESS")
}

fn resolve_forwarder_address(
    network: &str,
    control_address: Address,
) -> Result<String, Box<dyn std::error::Error>> {
    let network_key = match network {
        "arbitrum" => "FORWARDER_ADDRESS_ARBITRUM",
        "bsc" => "FORWARDER_ADDRESS_BSC",
        "polygon" => "FORWARDER_ADDRESS_POLYGON",
        "ethereum" => "FORWARDER_ADDRESS",
        other => {
            return Err(format!("unsupported network in NETWORK for forwarder: {other}").into());
        }
    };

    if let Ok(value) = env::var(network_key) {
        let trimmed = value.trim();
        if !is_placeholder(trimmed) {
            return Ok(trimmed.to_string());
        }
    }

    Ok(format!("{control_address:?}"))
}

fn is_placeholder(value: &str) -> bool {
    value.trim().is_empty()
        || value.trim() == "SUA_CHAVE_HEX_AQUI"
        || value.trim() == "0xSeuContratoAlvo"
}

fn default_chain_id(network: &str) -> u64 {
    match network {
        "ethereum" => 1,
        "bsc" => 56,
        "polygon" => 137,
        "arbitrum" => 42161,
        _ => 1,
    }
}

fn alchemy_url_for_network(network: &str, key: &str) -> Option<String> {
    match network {
        "ethereum" => Some(format!("https://eth-mainnet.g.alchemy.com/v2/{key}")),
        "arbitrum" => Some(format!("https://arb-mainnet.g.alchemy.com/v2/{key}")),
        "polygon" => Some(format!("https://polygon-mainnet.g.alchemy.com/v2/{key}")),
        _ => None,
    }
}

fn alchemy_ws_url_for_network(network: &str, key: &str) -> Option<String> {
    match network {
        "ethereum" => Some(format!("wss://eth-mainnet.g.alchemy.com/v2/{key}")),
        "arbitrum" => Some(format!("wss://arb-mainnet.g.alchemy.com/v2/{key}")),
        "polygon" => Some(format!("wss://polygon-mainnet.g.alchemy.com/v2/{key}")),
        _ => None,
    }
}

fn infura_url_for_network(network: &str, key: &str) -> Option<String> {
    match network {
        "ethereum" => Some(format!("https://mainnet.infura.io/v3/{key}")),
        "arbitrum" => Some(format!("https://arbitrum-mainnet.infura.io/v3/{key}")),
        "polygon" => Some(format!("https://polygon-mainnet.infura.io/v3/{key}")),
        _ => None,
    }
}

fn parse_bot_mode(value: &str) -> Result<BotMode, Box<dyn std::error::Error>> {
    match value.to_lowercase().as_str() {
        "shadow" => Ok(BotMode::Shadow),
        "paper" => Ok(BotMode::Paper),
        "live" => Ok(BotMode::Live),
        other => Err(format!("unsupported BOT_MODE: {other}").into()),
    }
}

fn parse_mock_hot_wallet_mode(
    value: &str,
) -> Result<MockHotWalletMode, Box<dyn std::error::Error>> {
    match value.to_lowercase().as_str() {
        "oneshot" => Ok(MockHotWalletMode::OneShot),
        "continuous" => Ok(MockHotWalletMode::Continuous),
        other => Err(format!("unsupported MOCK_HOT_WALLET_MODE: {other}").into()),
    }
}

fn parse_rpc_preference(value: &str) -> Result<RpcPreference, Box<dyn std::error::Error>> {
    match value.trim().to_lowercase().as_str() {
        "auto" => Ok(RpcPreference::Auto),
        "alchemy" => Ok(RpcPreference::Alchemy),
        "infura" => Ok(RpcPreference::Infura),
        other => Err(format!("unsupported RPC preference: {other}").into()),
    }
}

fn parse_address_set(value: &str) -> Result<HashSet<Address>, Box<dyn std::error::Error>> {
    let mut addresses = HashSet::new();
    for raw in value.split(',') {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        addresses.insert(trimmed.parse::<Address>()?);
    }
    Ok(addresses)
}

fn parse_monitored_tokens(
    network: &str,
) -> Result<Vec<MonitoredTokenConfig>, Box<dyn std::error::Error>> {
    let env_name = match network {
        "arbitrum" => "MONITORED_TOKENS_ARBITRUM",
        "bsc" => "MONITORED_TOKENS_BSC",
        "polygon" => "MONITORED_TOKENS_POLYGON",
        "ethereum" => "MONITORED_TOKENS_ETHEREUM",
        _ => return Ok(Vec::new()),
    };

    let raw = env::var(env_name).unwrap_or_default();
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }

    let mut tokens = Vec::new();
    for entry in raw.split(',') {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }

        let parts: Vec<&str> = trimmed.split(':').map(str::trim).collect();
        let token = match parts.as_slice() {
            [address, decimals, price_eth] => MonitoredTokenConfig {
                symbol: short_token_label(address),
                address: (*address).parse::<Address>()?,
                decimals: (*decimals).parse::<u8>()?,
                price_eth: (*price_eth).parse::<f64>()?,
                asset_class: infer_asset_class(&short_token_label(address)),
            },
            [symbol, address, decimals, price_eth] => MonitoredTokenConfig {
                symbol: (*symbol).to_string(),
                address: (*address).parse::<Address>()?,
                decimals: (*decimals).parse::<u8>()?,
                price_eth: (*price_eth).parse::<f64>()?,
                asset_class: infer_asset_class(symbol),
            },
            [symbol, class, address, decimals, price_eth] => MonitoredTokenConfig {
                symbol: (*symbol).to_string(),
                address: (*address).parse::<Address>()?,
                decimals: (*decimals).parse::<u8>()?,
                price_eth: (*price_eth).parse::<f64>()?,
                asset_class: parse_asset_class(class)?,
            },
            _ => {
                return Err(format!(
                    "invalid token entry in {env_name}: {trimmed}. expected address:decimals:price_eth, symbol:address:decimals:price_eth or symbol:class:address:decimals:price_eth"
                )
                .into())
            }
        };
        tokens.push(token);
    }

    Ok(tokens)
}

fn short_token_label(address: &str) -> String {
    address.chars().take(8).collect()
}

fn parse_asset_class(value: &str) -> Result<AssetClass, Box<dyn std::error::Error>> {
    match value.trim().to_lowercase().as_str() {
        "native" => Ok(AssetClass::Native),
        "stable" => Ok(AssetClass::Stable),
        "other" | "other-token" | "token" => Ok(AssetClass::OtherToken),
        other => Err(format!("unsupported token asset class: {other}").into()),
    }
}

fn infer_asset_class(symbol: &str) -> AssetClass {
    match symbol.trim().to_uppercase().as_str() {
        "USDT" | "USDC" | "DAI" | "USDE" | "FDUSD" | "TUSD" => AssetClass::Stable,
        _ => AssetClass::OtherToken,
    }
}
