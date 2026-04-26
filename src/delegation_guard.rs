use crate::config::{Config, DelegationGuardConfig};
use crate::dashboard::DashboardHandle;
use crate::rpc::RpcFleet;
use crate::runtime_mode::{RuntimeMode, RuntimeModeController};
use ethers::middleware::SignerMiddleware;
use ethers::prelude::*;
use ethers::types::transaction::eip2718::TypedTransaction;
use ethers::types::transaction::eip1559::Eip1559TransactionRequest;
use ethers_flashbots::FlashbotsMiddleware;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};
use url::Url;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

abigen!(
    GuardErc20,
    r#"[
        function allowance(address owner, address spender) external view returns (uint256)
        function approve(address spender, uint256 amount) external returns (bool)
    ]"#,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DelegationClassification {
    Trusted,
    Unknown,
    Compromised,
}

impl DelegationClassification {
    fn as_str(self) -> &'static str {
        match self {
            DelegationClassification::Trusted => "TRUSTED",
            DelegationClassification::Unknown => "UNKNOWN",
            DelegationClassification::Compromised => "COMPROMISED",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DelegationSource {
    Mempool,
    Confirmed,
}

impl DelegationSource {
    fn as_str(self) -> &'static str {
        match self {
            DelegationSource::Mempool => "mempool",
            DelegationSource::Confirmed => "confirmed",
        }
    }
}

#[derive(Debug, Clone)]
struct DelegationEvent {
    authority: Address,
    delegate: Address,
    authorization_nonce: U256,
    tx_hash: H256,
    source: DelegationSource,
    block_number: Option<u64>,
    calldata_selector: Option<[u8; 4]>,
}

#[derive(Debug)]
struct WalletIncidentState {
    recent_reclaims: VecDeque<Instant>,
    last_reclaim_at: Option<Instant>,
    last_observed_tx: Option<H256>,
    cooldown_until: Option<Instant>,
}

impl WalletIncidentState {
    fn new() -> Self {
        Self {
            recent_reclaims: VecDeque::new(),
            last_reclaim_at: None,
            last_observed_tx: None,
            cooldown_until: None,
        }
    }

    fn can_reclaim(&mut self, config: &DelegationGuardConfig, now: Instant) -> bool {
        while let Some(oldest) = self.recent_reclaims.front().copied() {
            if now.duration_since(oldest).as_secs() > config.reclaim_window_secs {
                self.recent_reclaims.pop_front();
            } else {
                break;
            }
        }

        if let Some(cooldown_until) = self.cooldown_until {
            if cooldown_until > now {
                return false;
            }
        }

        self.recent_reclaims.len() < config.max_reclaims_per_window
    }

    fn record_reclaim(&mut self, config: &DelegationGuardConfig, now: Instant) {
        self.recent_reclaims.push_back(now);
        self.last_reclaim_at = Some(now);
        self.cooldown_until = Some(now + Duration::from_millis(config.reclaim_cooldown_ms));
    }
}

#[derive(Debug)]
struct SponsorNonceManager {
    next_nonce: Option<U256>,
}

impl SponsorNonceManager {
    fn new() -> Self {
        Self { next_nonce: None }
    }

    async fn reserve(
        &mut self,
        provider: Arc<Provider<Http>>,
        sponsor: Address,
    ) -> Result<U256, BoxError> {
        let chain_pending = provider.get_transaction_count(sponsor, Some(BlockNumber::Pending.into())).await?;
        let reserved = match self.next_nonce {
            Some(next) if next > chain_pending => next,
            _ => chain_pending,
        };
        self.next_nonce = Some(reserved + U256::one());
        Ok(reserved)
    }
}

#[derive(Clone)]
pub struct DelegationGuardService {
    config: Arc<Config>,
    rpc_fleet: Arc<RpcFleet>,
    dashboard: DashboardHandle,
    mode: RuntimeModeController,
    wallets: Arc<HashMap<Address, LocalWallet>>,
    trusted_code_hashes: Arc<HashMap<Address, H256>>,
    sponsor_wallet: LocalWallet,
    sponsor_nonce: Arc<tokio::sync::Mutex<SponsorNonceManager>>,
    incidents: Arc<tokio::sync::Mutex<HashMap<Address, WalletIncidentState>>>,
}

#[derive(Debug, Deserialize, Serialize)]
struct RpcBlock {
    #[serde(default, rename = "number")]
    number_hex: Option<String>,
    #[serde(default)]
    transactions: Vec<RpcTransaction>,
}

#[derive(Debug, Deserialize, Serialize)]
struct RpcTransaction {
    hash: H256,
    input: Bytes,
    #[serde(default, rename = "authorizationList")]
    authorization_list: Vec<RpcAuthorization>,
}

#[derive(Debug, Deserialize, Serialize)]
struct RpcAuthorization {
    #[serde(rename = "chainId")]
    chain_id: U256,
    address: Address,
    nonce: U256,
    #[serde(rename = "yParity")]
    y_parity: U64,
    r: U256,
    s: U256,
}

#[derive(Clone)]
struct ReclaimSubmission {
    tx_hash: H256,
    relay_used: Option<String>,
}

pub async fn start(
    rpc_fleet: Arc<RpcFleet>,
    config: Arc<Config>,
    wallets: Vec<LocalWallet>,
    dashboard: DashboardHandle,
    mode: RuntimeModeController,
) -> Result<(), BoxError> {
    if !config.delegation_guard.enabled || wallets.is_empty() {
        return Ok(());
    }

    let sponsor_wallet = config
        .sender_private_key
        .parse::<LocalWallet>()?
        .with_chain_id(config.chain_id);
    let mut trusted_code_hashes = HashMap::new();
    for delegate in &config.delegation_guard.trusted_delegates {
        let provider = rpc_fleet.read_endpoint().provider.clone();
        let code = provider.get_code(*delegate, None).await?;
        if !code.as_ref().is_empty() {
            trusted_code_hashes.insert(*delegate, H256::from(ethers::utils::keccak256(code.as_ref())));
        }
    }

    let wallet_map = wallets
        .into_iter()
        .map(|wallet| (wallet.address(), wallet))
        .collect::<HashMap<_, _>>();

    let service = DelegationGuardService {
        config,
        rpc_fleet,
        dashboard,
        mode,
        wallets: Arc::new(wallet_map),
        trusted_code_hashes: Arc::new(trusted_code_hashes),
        sponsor_wallet,
        sponsor_nonce: Arc::new(tokio::sync::Mutex::new(SponsorNonceManager::new())),
        incidents: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
    };

    service.spawn_monitors().await;
    Ok(())
}

impl DelegationGuardService {
    async fn spawn_monitors(self) {
        info!(
            "{}",
            json!({
                "event": "delegation_guard_started",
                "wallets": self.wallets.len(),
                "mode": self.mode.mode().as_str(),
                "trusted_delegate_count": self.config.delegation_guard.trusted_delegates.len(),
            })
        );

        let pending_service = self.clone();
        tokio::spawn(async move {
            pending_service.monitor_pending_block().await;
        });

        let confirmed_service = self.clone();
        tokio::spawn(async move {
            confirmed_service.monitor_confirmed_blocks().await;
        });
    }

    async fn monitor_pending_block(self) {
        let mut seen = HashSet::new();
        loop {
            if let Ok(events) = self.fetch_block_events("pending", DelegationSource::Mempool).await {
                for event in events {
                    if seen.insert(event.tx_hash) {
                        self.handle_event(event).await;
                    }
                }
                if seen.len() > 2048 {
                    seen.clear();
                }
            }
            tokio::time::sleep(Duration::from_millis(self.config.delegation_guard.poll_interval_ms)).await;
        }
    }

    async fn monitor_confirmed_blocks(self) {
        let mut last_block = None;
        loop {
            let endpoint = self.rpc_fleet.read_endpoint();
            let block_number = match endpoint.provider.get_block_number().await {
                Ok(number) => number.as_u64(),
                Err(err) => {
                    self.rpc_fleet
                        .report_provider_error(endpoint.id, &err.to_string());
                    tokio::time::sleep(Duration::from_millis(self.config.delegation_guard.poll_interval_ms)).await;
                    continue;
                }
            };
            self.rpc_fleet.report_success(endpoint.id, Duration::from_millis(1));
            self.rpc_fleet.report_block(endpoint.id, block_number.into());

            let start = last_block.map(|value| value + 1).unwrap_or(block_number);
            for number in start..=block_number {
                let tag = format!("0x{number:x}");
                if let Ok(events) = self.fetch_block_events(&tag, DelegationSource::Confirmed).await {
                    for event in events {
                        self.handle_event(event).await;
                    }
                }
            }
            last_block = Some(block_number);
            tokio::time::sleep(Duration::from_millis(self.config.delegation_guard.poll_interval_ms)).await;
        }
    }

    async fn fetch_block_events(
        &self,
        block_tag: &str,
        source: DelegationSource,
    ) -> Result<Vec<DelegationEvent>, BoxError> {
        let mut events = Vec::new();
        for handle in self.rpc_fleet.all_handles() {
            let response: RpcBlock = handle
                .provider
                .request::<_, RpcBlock>("eth_getBlockByNumber", json!([block_tag, true]))
                .await?;

            let block_number = response
                .number_hex
                .as_deref()
                .and_then(|value| u64::from_str_radix(value.trim_start_matches("0x"), 16).ok());

            for tx in response.transactions {
                let selector = tx
                    .input
                    .as_ref()
                    .get(0..4)
                    .map(|value| [value[0], value[1], value[2], value[3]]);
                for authorization in tx.authorization_list {
                    let authority = recover_authority(&authorization)?;
                    if !self.wallets.contains_key(&authority) {
                        continue;
                    }
                    events.push(DelegationEvent {
                        authority,
                        delegate: authorization.address,
                        authorization_nonce: authorization.nonce,
                        tx_hash: tx.hash,
                        source,
                        block_number,
                        calldata_selector: selector,
                    });
                }
            }
            if !events.is_empty() {
                break;
            }
        }
        Ok(events)
    }

    async fn handle_event(&self, event: DelegationEvent) {
        let classification = match self.classify_event(&event).await {
            Ok(value) => value,
            Err(err) => {
                warn!("delegation classification failed for {:?}: {}", event.authority, err);
                DelegationClassification::Unknown
            }
        };

        info!(
            "{}",
            json!({
                "event": "delegation_detected",
                "wallet": format!("{:?}", event.authority),
                "delegate": format!("{:?}", event.delegate),
                "classification": classification.as_str(),
                "source": event.source.as_str(),
                "block_number": event.block_number,
                "tx_hash": format!("{:?}", event.tx_hash),
                "authorization_nonce": event.authorization_nonce.to_string(),
            })
        );

        match classification {
            DelegationClassification::Trusted => {}
            DelegationClassification::Unknown => {
                self.mode.escalate(RuntimeMode::Alert);
                self.dashboard.event(
                    "warn",
                    format!("delegation alert for {:?} via {:?}", event.authority, event.tx_hash),
                );
                self.trigger_incident(event, classification).await;
            }
            DelegationClassification::Compromised => {
                self.mode.escalate(RuntimeMode::Lockdown);
                self.dashboard.event(
                    "error",
                    format!("delegation lockdown for {:?} via {:?}", event.authority, event.tx_hash),
                );
                self.trigger_incident(event, classification).await;
            }
        }
    }

    async fn classify_event(
        &self,
        event: &DelegationEvent,
    ) -> Result<DelegationClassification, BoxError> {
        if !self
            .config
            .delegation_guard
            .trusted_delegates
            .contains(&event.delegate)
        {
            return Ok(match event.source {
                DelegationSource::Mempool => DelegationClassification::Unknown,
                DelegationSource::Confirmed => DelegationClassification::Compromised,
            });
        }

        if !self.config.delegation_guard.allowed_calldata_selectors.is_empty() {
            let selector_ok = event
                .calldata_selector
                .map(|selector| {
                    self.config
                        .delegation_guard
                        .allowed_calldata_selectors
                        .contains(&selector)
                })
                .unwrap_or(false);
            if !selector_ok {
                return Ok(match event.source {
                    DelegationSource::Mempool => DelegationClassification::Unknown,
                    DelegationSource::Confirmed => DelegationClassification::Compromised,
                });
            }
        }

        if let Some(expected_hash) = self.trusted_code_hashes.get(&event.delegate).copied() {
            let provider = self.rpc_fleet.read_endpoint().provider.clone();
            let code = provider.get_code(event.delegate, None).await?;
            let observed_hash = H256::from(ethers::utils::keccak256(code.as_ref()));
            if observed_hash != expected_hash {
                return Ok(DelegationClassification::Compromised);
            }
        }

        Ok(DelegationClassification::Trusted)
    }

    async fn trigger_incident(
        &self,
        event: DelegationEvent,
        classification: DelegationClassification,
    ) {
        info!(
            "{}",
            json!({
                "event": "incident_triggered",
                "wallet": format!("{:?}", event.authority),
                "delegate": format!("{:?}", event.delegate),
                "classification": classification.as_str(),
                "mode": self.mode.mode().as_str(),
                "source": event.source.as_str(),
                "tx_hash": format!("{:?}", event.tx_hash),
            })
        );

        let now = Instant::now();
        {
            let mut incidents = self.incidents.lock().await;
            let state = incidents
                .entry(event.authority)
                .or_insert_with(WalletIncidentState::new);
            if state.last_observed_tx == Some(event.tx_hash) {
                return;
            }
            state.last_observed_tx = Some(event.tx_hash);
            if !state.can_reclaim(&self.config.delegation_guard, now) {
                warn!("reclaim rate-limited for {:?}", event.authority);
                return;
            }
            state.record_reclaim(&self.config.delegation_guard, now);
        }

        match self.reclaim_wallet(&event).await {
            Ok(submission) => {
                info!(
                    "{}",
                    json!({
                        "event": "reclaim_sent",
                        "wallet": format!("{:?}", event.authority),
                        "trusted_delegate": format!("{:?}", self.config.contract),
                        "tx_hash": format!("{:?}", submission.tx_hash),
                        "relay": submission.relay_used,
                    })
                );
                if let Err(err) = self.confirm_reclaim(&event, submission.tx_hash).await {
                    error!(
                        "{}",
                        json!({
                            "event": "reclaim_failed",
                            "wallet": format!("{:?}", event.authority),
                            "tx_hash": format!("{:?}", submission.tx_hash),
                            "error": err.to_string(),
                        })
                    );
                } else {
                    info!(
                        "{}",
                        json!({
                            "event": "reclaim_confirmed",
                            "wallet": format!("{:?}", event.authority),
                            "tx_hash": format!("{:?}", submission.tx_hash),
                        })
                    );
                    if let Err(err) = self.run_mitigations(event.authority).await {
                        warn!("mitigation failed for {:?}: {}", event.authority, err);
                    }
                }
            }
            Err(err) => error!(
                "{}",
                json!({
                    "event": "reclaim_failed",
                    "wallet": format!("{:?}", event.authority),
                    "error": err.to_string(),
                })
            ),
        }
    }

    async fn reclaim_wallet(
        &self,
        event: &DelegationEvent,
    ) -> Result<ReclaimSubmission, BoxError> {
        let wallet = self
            .wallets
            .get(&event.authority)
            .ok_or("missing managed wallet")?;
        let send_handle = self.rpc_fleet.send_endpoint();
        let provider = send_handle.provider.clone();
        let latest_nonce = provider
            .get_transaction_count(event.authority, Some(BlockNumber::Pending.into()))
            .await?;
        let authority_nonce = match event.source {
            DelegationSource::Mempool if event.authorization_nonce >= latest_nonce => {
                event.authorization_nonce
            }
            _ => latest_nonce,
        };

        let base_fee = provider.get_gas_price().await?;
        let aggressive_priority = ethers::utils::parse_units(
            self.config
                .delegation_guard
                .aggressive_priority_fee_gwei
                .to_string(),
            "gwei",
        )?
        .into();
        let aggressive_max_fee = base_fee
            .saturating_mul(U256::from(self.config.delegation_guard.aggressive_fee_multiplier_bps))
            / U256::from(10_000u64)
            + aggressive_priority;

        let auth = build_eip7702_authorization(
            wallet,
            self.config.chain_id,
            self.config.contract,
            authority_nonce,
        )?;

        let sponsor_nonce = {
            let mut nonce_manager = self.sponsor_nonce.lock().await;
            nonce_manager
                .reserve(provider.clone(), self.sponsor_wallet.address())
                .await?
        };
        let tx = build_eip7702_reclaim_tx(
            &self.sponsor_wallet,
            sponsor_nonce,
            aggressive_priority,
            aggressive_max_fee,
            self.config.estimated_install_gas.max(220_000),
            self.sponsor_wallet.address(),
            &[auth],
        )?;

        let tx_hash = if self.config.delegation_guard.private_relay_enabled {
            match self.submit_private_reclaim(provider.clone(), tx.clone()).await {
                Ok(hash) => {
                    return Ok(ReclaimSubmission {
                        tx_hash: hash,
                        relay_used: Some(self.config.flashbots_relay.clone()),
                    })
                }
                Err(err) => {
                    warn!("private relay reclaim failed, falling back to public send: {}", err);
                    provider.send_raw_transaction(tx.clone()).await?.tx_hash()
                }
            }
        } else {
            provider.send_raw_transaction(tx.clone()).await?.tx_hash()
        };

        Ok(ReclaimSubmission {
            tx_hash,
            relay_used: None,
        })
    }

    async fn submit_private_reclaim(
        &self,
        provider: Arc<Provider<Http>>,
        raw_tx: Bytes,
    ) -> Result<H256, BoxError> {
        let relay_url = Url::parse(&self.config.flashbots_relay)?;
        let relay_signer = self.sponsor_wallet.clone();
        let client = SignerMiddleware::new(provider, self.sponsor_wallet.clone());
        let flashbots = FlashbotsMiddleware::new(client, relay_url, relay_signer);
        let block = flashbots.inner().get_block_number().await?;
        let bundle = ethers_flashbots::BundleRequest::new()
            .set_block(block + 1)
            .push_transaction(raw_tx);
        let pending = flashbots.send_bundle(&bundle).await?;
        pending
            .bundle_hash
            .ok_or_else(|| "private relay returned no bundle hash".into())
    }

    async fn confirm_reclaim(
        &self,
        event: &DelegationEvent,
        tx_hash: H256,
    ) -> Result<(), BoxError> {
        let provider = self.rpc_fleet.read_endpoint().provider.clone();
        for _ in 0..20 {
            if let Some(receipt) = provider.get_transaction_receipt(tx_hash).await? {
                if receipt.status != Some(U64::from(1u64)) {
                    return Err(format!("reclaim tx {:?} reverted", tx_hash).into());
                }
                let code = provider.get_code(event.authority, None).await?;
                let delegate = extract_eip7702_delegate(&code);
                if delegate == Some(self.config.contract) {
                    return Ok(());
                }
                return Err(format!(
                    "reclaim tx {:?} mined but delegate is {:?}",
                    tx_hash, delegate
                )
                .into());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        Err(format!("timeout waiting reclaim tx {:?}", tx_hash).into())
    }

    async fn run_mitigations(
        &self,
        wallet_address: Address,
    ) -> Result<(), BoxError> {
        self.revoke_approvals(wallet_address).await?;
        if self.config.delegation_guard.secure_sweep_enabled {
            self.secure_sweep(wallet_address).await?;
        }
        Ok(())
    }

    async fn revoke_approvals(
        &self,
        wallet_address: Address,
    ) -> Result<(), BoxError> {
        if self.config.delegation_guard.approval_spenders.is_empty()
            || self.config.monitored_tokens.is_empty()
        {
            return Ok(());
        }

        let wallet = self.wallets.get(&wallet_address).ok_or("missing wallet")?.clone();
        let provider = self.rpc_fleet.send_endpoint().provider.clone();
        let signer = Arc::new(SignerMiddleware::new(provider.clone(), wallet.clone()));
        let mut next_nonce = provider
            .get_transaction_count(wallet_address, Some(BlockNumber::Pending.into()))
            .await?;

        for token in &self.config.monitored_tokens {
            let contract = GuardErc20::new(token.address, signer.clone());
            for spender in &self.config.delegation_guard.approval_spenders {
                let allowance = contract.allowance(wallet_address, *spender).call().await?;
                if allowance.is_zero() {
                    continue;
                }
                let approve_call = contract
                    .approve(*spender, U256::zero())
                    .nonce(next_nonce)
                    .gas(65_000u64);
                let pending = approve_call.send().await?;
                info!(
                    "{}",
                    json!({
                        "event": "approval_revocation_sent",
                        "wallet": format!("{:?}", wallet_address),
                        "token": format!("{:?}", token.address),
                        "spender": format!("{:?}", spender),
                        "tx_hash": format!("{:?}", pending.tx_hash()),
                    })
                );
                next_nonce += U256::one();
            }
        }

        Ok(())
    }

    async fn secure_sweep(
        &self,
        wallet_address: Address,
    ) -> Result<(), BoxError> {
        let wallet = self.wallets.get(&wallet_address).ok_or("missing wallet")?.clone();
        let provider = self.rpc_fleet.send_endpoint().provider.clone();
        let balance = provider.get_balance(wallet_address, None).await?;
        if balance.is_zero() {
            return Ok(());
        }

        let gas_price = provider.get_gas_price().await?;
        let gas_limit = U256::from(21_000u64);
        let gas_cost = gas_price.saturating_mul(gas_limit);
        if balance <= gas_cost {
            return Ok(());
        }

        let nonce = provider
            .get_transaction_count(wallet_address, Some(BlockNumber::Pending.into()))
            .await?;
        let tx: TypedTransaction = Eip1559TransactionRequest::new()
            .to(self.config.control_address)
            .value(balance.saturating_sub(gas_cost))
            .nonce(nonce)
            .gas(gas_limit)
            .max_priority_fee_per_gas(gas_price)
            .max_fee_per_gas(gas_price)
            .into();
        let signature = wallet.sign_transaction(&tx).await?;
        let pending = provider.send_raw_transaction(tx.rlp_signed(&signature)).await?;
        info!(
            "{}",
            json!({
                "event": "secure_sweep_sent",
                "wallet": format!("{:?}", wallet_address),
                "destination": format!("{:?}", self.config.control_address),
                "tx_hash": format!("{:?}", pending.tx_hash()),
            })
        );
        Ok(())
    }
}

fn recover_authority(auth: &RpcAuthorization) -> Result<Address, BoxError> {
    let mut payload = ethers::utils::rlp::RlpStream::new_list(3);
    payload.append(&auth.chain_id);
    payload.append(&auth.address);
    payload.append(&auth.nonce);

    let mut preimage = vec![0x05];
    preimage.extend_from_slice(payload.out().as_ref());
    let hash = H256::from(ethers::utils::keccak256(preimage));
    let signature = Signature {
        r: auth.r,
        s: auth.s,
        v: match auth.y_parity.as_u64() {
            0 | 27 => 27,
            1 | 28 => 28,
            other => other,
        },
    };
    Ok(signature.recover(hash)?)
}

fn extract_eip7702_delegate(code: &Bytes) -> Option<Address> {
    let raw = code.as_ref();
    if raw.len() >= 23 && raw.starts_with(&[0xef, 0x01, 0x00]) {
        Some(Address::from_slice(&raw[3..23]))
    } else {
        None
    }
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
) -> Result<Eip7702Authorization, BoxError> {
    use ethers::utils::rlp::RlpStream;

    let chain_id = U256::from(chain_id);
    let mut payload = RlpStream::new_list(3);
    payload.append(&chain_id);
    payload.append(&delegate_address);
    payload.append(&nonce);
    let mut preimage = vec![0x05];
    preimage.extend_from_slice(payload.out().as_ref());
    let signature = wallet.sign_hash(H256::from(ethers::utils::keccak256(preimage)))?;
    Ok(Eip7702Authorization {
        chain_id,
        delegate_address,
        nonce,
        signature,
    })
}

fn build_eip7702_reclaim_tx(
    sponsor_wallet: &LocalWallet,
    sponsor_nonce: U256,
    max_priority_fee_per_gas: U256,
    max_fee_per_gas: U256,
    gas_limit: u64,
    destination: Address,
    authorizations: &[Eip7702Authorization],
) -> Result<Bytes, BoxError> {
    use ethers::types::transaction::eip2930::AccessList;
    use ethers::utils::rlp::RlpStream;

    let chain_id = U256::from(sponsor_wallet.chain_id());
    let gas_limit = U256::from(gas_limit);
    let access_list = AccessList::default();
    let data: &[u8] = &[];

    let mut unsigned = RlpStream::new_list(10);
    unsigned.append(&chain_id);
    unsigned.append(&sponsor_nonce);
    unsigned.append(&max_priority_fee_per_gas);
    unsigned.append(&max_fee_per_gas);
    unsigned.append(&gas_limit);
    unsigned.append(&destination);
    unsigned.append(&U256::zero());
    unsigned.append(&data);
    unsigned.append(&access_list);
    append_authorization_list(&mut unsigned, authorizations)?;

    let mut sighash_preimage = vec![0x04];
    sighash_preimage.extend_from_slice(unsigned.out().as_ref());
    let outer_sig =
        sponsor_wallet.sign_hash(H256::from(ethers::utils::keccak256(sighash_preimage)))?;
    let outer_y_parity = signature_y_parity(&outer_sig)?;

    let mut signed = RlpStream::new_list(13);
    signed.append(&chain_id);
    signed.append(&sponsor_nonce);
    signed.append(&max_priority_fee_per_gas);
    signed.append(&max_fee_per_gas);
    signed.append(&gas_limit);
    signed.append(&destination);
    signed.append(&U256::zero());
    signed.append(&data);
    signed.append(&access_list);
    append_authorization_list(&mut signed, authorizations)?;
    signed.append(&outer_y_parity);
    signed.append(&outer_sig.r);
    signed.append(&outer_sig.s);

    let mut encoded = vec![0x04];
    encoded.extend_from_slice(signed.out().as_ref());
    Ok(Bytes::from(encoded))
}

fn append_authorization_list(
    rlp: &mut ethers::utils::rlp::RlpStream,
    authorizations: &[Eip7702Authorization],
) -> Result<(), BoxError> {
    rlp.begin_list(authorizations.len());
    for auth in authorizations {
        let y_parity = signature_y_parity(&auth.signature)?;
        rlp.begin_list(6);
        rlp.append(&auth.chain_id);
        rlp.append(&auth.delegate_address);
        rlp.append(&auth.nonce);
        rlp.append(&y_parity);
        rlp.append(&auth.signature.r);
        rlp.append(&auth.signature.s);
    }
    Ok(())
}

fn signature_y_parity(signature: &Signature) -> Result<u8, BoxError> {
    match signature.v {
        27 | 28 => Ok((signature.v - 27) as u8),
        0 | 1 => Ok(signature.v as u8),
        other => Err(format!("unsupported signature v value: {}", other).into()),
    }
}
