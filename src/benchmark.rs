use crate::config::Config;
use crate::rpc::RpcFleet;
use ethers::middleware::SignerMiddleware;
use ethers::prelude::*;
use ethers::types::transaction::eip2718::TypedTransaction;
use ethers_flashbots::{BundleRequest, FlashbotsMiddleware};
use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{info, warn};
use url::Url;

pub async fn maybe_run_network_benchmark(
    config: Arc<Config>,
    rpc_fleet: Arc<RpcFleet>,
    wallets: &[LocalWallet],
) -> Result<bool, Box<dyn std::error::Error>> {
    if !env_flag("RUN_NETWORK_BENCHMARK") {
        return Ok(false);
    }

    let samples = env_u64("NETWORK_BENCHMARK_SAMPLES", 12) as usize;
    let wallet_sample_size = env_usize("NETWORK_BENCHMARK_WALLETS", 25)
        .max(1)
        .min(wallets.len().max(1));
    let bench_bundle = env_flag("NETWORK_BENCHMARK_BUNDLE");

    info!(
        "Running network benchmark: samples={} wallet_sample_size={} bundle={}",
        samples, wallet_sample_size, bench_bundle
    );

    let sample_addresses: Vec<Address> = wallets
        .iter()
        .take(wallet_sample_size.min(wallets.len()))
        .map(LocalWallet::address)
        .collect();

    if sample_addresses.is_empty() {
        warn!("network benchmark skipped batch/nonce probes because no wallets were loaded");
    }

    println!("=== Network Benchmark ===");
    println!("Network: {}", config.network);
    println!("RPC endpoints: {}", rpc_fleet.endpoint_count());
    println!("Samples per endpoint: {}", samples);
    println!("Wallet sample size: {}", sample_addresses.len());
    println!("Bundle probe enabled: {}", bench_bundle);
    println!();

    for handle in rpc_fleet.snapshot() {
        println!("[{}] {} ({})", handle.kind, handle.name, handle.url);

        let endpoint = rpc_fleet
            .all_handles()
            .into_iter()
            .find(|candidate| candidate.id == handle.id)
            .ok_or("benchmark endpoint lookup failed")?;

        let block_metrics = measure_async(samples, || {
            let provider = endpoint.provider.clone();
            async move {
                provider
                    .get_block_number()
                    .await
                    .map(|_| ())
                    .map_err(|err| err.to_string())
            }
        })
        .await;
        print_metrics("get_block_number", &block_metrics);

        let gas_metrics = measure_async(samples, || {
            let provider = endpoint.provider.clone();
            async move {
                provider
                    .get_gas_price()
                    .await
                    .map(|_| ())
                    .map_err(|err| err.to_string())
            }
        })
        .await;
        print_metrics("get_gas_price", &gas_metrics);

        if !sample_addresses.is_empty() {
            let batch_metrics = measure_async(samples, || {
                let rpc = endpoint.clone();
                let addresses = sample_addresses.clone();
                async move { rpc.get_balances_batch(&addresses).await.map(|_| ()) }
            })
            .await;
            print_metrics("get_balances_batch", &batch_metrics);

            let nonce_metrics = measure_async(samples, || {
                let provider = endpoint.provider.clone();
                let address = sample_addresses[0];
                async move {
                    provider
                        .get_transaction_count(address, None)
                        .await
                        .map(|_| ())
                        .map_err(|err| err.to_string())
                }
            })
            .await;
            print_metrics("get_transaction_count", &nonce_metrics);
        }

        if bench_bundle {
            let bundle_metrics = benchmark_bundle_submission(&config, &endpoint).await;
            print_metrics("send_bundle", &bundle_metrics);
        } else {
            println!("  send_bundle: skipped (set NETWORK_BENCHMARK_BUNDLE=true to enable)");
        }

        println!();
    }

    Ok(true)
}

async fn benchmark_bundle_submission(
    config: &Config,
    endpoint: &crate::rpc::RpcHandle,
) -> ProbeMetrics {
    let samples = env_u64("NETWORK_BENCHMARK_BUNDLE_SAMPLES", 3) as usize;
    let sponsor_wallet = match config
        .sender_private_key
        .parse::<LocalWallet>()
        .map(|wallet| wallet.with_chain_id(config.chain_id))
    {
        Ok(wallet) => wallet,
        Err(err) => {
            return ProbeMetrics::single_error(format!("failed to load sponsor wallet: {}", err));
        }
    };

    let relay_url = match Url::parse(&config.flashbots_relay) {
        Ok(url) => url,
        Err(err) => return ProbeMetrics::single_error(format!("invalid relay url: {}", err)),
    };

    measure_async(samples, || {
        let provider = endpoint.provider.clone();
        let relay_signer = sponsor_wallet.clone();
        let bundle_signer = sponsor_wallet.clone();
        let relay_url = relay_url.clone();
        let middleware_signer = sponsor_wallet.clone();
        async move {
            let latest_block = provider
                .get_block_number()
                .await
                .map_err(|err| format!("block fetch failed before bundle: {}", err))?;

            let tx: TypedTransaction = TransactionRequest::new()
                .to(bundle_signer.address())
                .value(U256::zero())
                .gas(21_000u64)
                .gas_price(U256::from(1_000_000_000u64))
                .nonce(
                    provider
                        .get_transaction_count(bundle_signer.address(), None)
                        .await
                        .map_err(|err| format!("nonce fetch failed before bundle: {}", err))?,
                )
                .from(bundle_signer.address())
                .into();
            let signature = bundle_signer
                .sign_transaction(&tx)
                .await
                .map_err(|err| format!("bundle sign failed: {}", err))?;
            let signed_tx = tx.rlp_signed(&signature);

            let flashbots_client = SignerMiddleware::new(provider.clone(), middleware_signer);
            let flashbots =
                FlashbotsMiddleware::new(flashbots_client, relay_url.clone(), relay_signer.clone());
            let bundle = BundleRequest::new()
                .set_block(latest_block + 1)
                .push_transaction(signed_tx);

            flashbots
                .send_bundle(&bundle)
                .await
                .map(|_| ())
                .map_err(|err| err.to_string())
        }
    })
    .await
}

#[derive(Default)]
struct ProbeMetrics {
    latencies: Vec<Duration>,
    errors: Vec<String>,
}

impl ProbeMetrics {
    fn single_error(message: String) -> Self {
        Self {
            latencies: Vec::new(),
            errors: vec![message],
        }
    }
}

async fn measure_async<F, Fut>(samples: usize, mut f: F) -> ProbeMetrics
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let mut metrics = ProbeMetrics::default();

    for _ in 0..samples {
        let started = Instant::now();
        match f().await {
            Ok(()) => metrics.latencies.push(started.elapsed()),
            Err(err) => metrics.errors.push(err),
        }
    }

    metrics
}

fn print_metrics(label: &str, metrics: &ProbeMetrics) {
    if metrics.latencies.is_empty() {
        println!("  {}: no successful samples", label);
    } else {
        let mut millis: Vec<u128> = metrics
            .latencies
            .iter()
            .map(|latency| latency.as_millis())
            .collect();
        millis.sort_unstable();
        let p50 = percentile(&millis, 50);
        let p95 = percentile(&millis, 95);
        let avg = millis.iter().sum::<u128>() / millis.len() as u128;
        println!(
            "  {}: ok={} err={} avg={}ms p50={}ms p95={}ms",
            label,
            millis.len(),
            metrics.errors.len(),
            avg,
            p50,
            p95
        );
    }

    if !metrics.errors.is_empty() {
        for error in metrics.errors.iter().take(3) {
            println!("    error: {}", error);
        }
        if metrics.errors.len() > 3 {
            println!("    error: ... and {} more", metrics.errors.len() - 3);
        }
    }
}

fn percentile(values: &[u128], percentile: usize) -> u128 {
    if values.is_empty() {
        return 0;
    }
    let rank = ((values.len() - 1) * percentile) / 100;
    values[rank]
}

fn env_flag(name: &str) -> bool {
    env::var(name)
        .unwrap_or_else(|_| "false".to_string())
        .trim()
        .eq_ignore_ascii_case("true")
}

fn env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(default)
}
