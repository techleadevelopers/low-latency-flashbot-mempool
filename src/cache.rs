use crate::config::Config;
use crate::contract::Simple7702Delegate;
use ethers::prelude::*;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct RuntimeCache {
    inner: Arc<RwLock<RuntimeCacheState>>,
    contract_ttl: Duration,
    gas_ttl: Duration,
}

#[derive(Default)]
struct RuntimeCacheState {
    contract: Option<CachedContractState>,
    gas_price: Option<CachedGasPrice>,
}

#[derive(Clone)]
pub struct ContractStateSnapshot {
    pub frozen: bool,
    pub destination: Option<Address>,
    pub tokens: Vec<Address>,
}

struct CachedContractState {
    fetched_at: Instant,
    network: String,
    contract: Address,
    frozen: bool,
    destination: Option<Address>,
    tokens: Vec<Address>,
}

struct CachedGasPrice {
    fetched_at: Instant,
    network: String,
    value: U256,
}

impl RuntimeCache {
    pub fn new(config: &Config) -> Self {
        Self {
            inner: Arc::new(RwLock::new(RuntimeCacheState::default())),
            contract_ttl: Duration::from_secs(config.contract_cache_ttl_secs),
            gas_ttl: Duration::from_secs(config.gas_price_cache_ttl_secs),
        }
    }

    pub async fn contract_state(
        &self,
        provider: Arc<Provider<Http>>,
        contract_addr: Address,
        network: &str,
    ) -> Result<ContractStateSnapshot, Box<dyn std::error::Error>> {
        {
            let state = self.inner.read().await;
            if let Some(cached) = &state.contract {
                if cached.contract == contract_addr
                    && cached.network == network
                    && cached.fetched_at.elapsed() <= self.contract_ttl
                {
                    return Ok(ContractStateSnapshot {
                        frozen: cached.frozen,
                        destination: cached.destination,
                        tokens: cached.tokens.clone(),
                    });
                }
            }
        }

        let contract = Simple7702Delegate::new(contract_addr, provider);
        let frozen = contract.frozen().call().await?;
        let destination = contract.destination().call().await.ok();
        let tokens = match network {
            "arbitrum" => contract.get_arbitrum_tokens().call().await?,
            "bsc" => contract.get_bsc_tokens().call().await?,
            _ => Vec::new(),
        };

        let snapshot = ContractStateSnapshot {
            frozen,
            destination,
            tokens: tokens.clone(),
        };

        let mut state = self.inner.write().await;
        state.contract = Some(CachedContractState {
            fetched_at: Instant::now(),
            network: network.to_string(),
            contract: contract_addr,
            frozen,
            destination,
            tokens,
        });

        Ok(snapshot)
    }

    pub async fn refresh_contract_state(
        &self,
        provider: Arc<Provider<Http>>,
        contract_addr: Address,
        network: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let contract = Simple7702Delegate::new(contract_addr, provider);
        let frozen = contract.frozen().call().await?;
        let destination = contract.destination().call().await.ok();
        let tokens = match network {
            "arbitrum" => contract.get_arbitrum_tokens().call().await?,
            "bsc" => contract.get_bsc_tokens().call().await?,
            _ => Vec::new(),
        };

        let mut state = self.inner.write().await;
        state.contract = Some(CachedContractState {
            fetched_at: Instant::now(),
            network: network.to_string(),
            contract: contract_addr,
            frozen,
            destination,
            tokens,
        });

        Ok(())
    }

    pub async fn gas_price(
        &self,
        provider: Arc<Provider<Http>>,
        network: &str,
    ) -> Result<U256, Box<dyn std::error::Error>> {
        {
            let state = self.inner.read().await;
            if let Some(cached) = &state.gas_price {
                if cached.network == network && cached.fetched_at.elapsed() <= self.gas_ttl {
                    return Ok(cached.value);
                }
            }
        }

        let gas_price = provider.get_gas_price().await?;
        let mut state = self.inner.write().await;
        state.gas_price = Some(CachedGasPrice {
            fetched_at: Instant::now(),
            network: network.to_string(),
            value: gas_price,
        });

        Ok(gas_price)
    }

    pub async fn refresh_gas_price(
        &self,
        provider: Arc<Provider<Http>>,
        network: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let gas_price = provider.get_gas_price().await?;
        let mut state = self.inner.write().await;
        state.gas_price = Some(CachedGasPrice {
            fetched_at: Instant::now(),
            network: network.to_string(),
            value: gas_price,
        });
        Ok(())
    }
}
