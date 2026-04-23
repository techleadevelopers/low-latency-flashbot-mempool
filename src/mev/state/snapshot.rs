use ethers::types::{Address, H256, U256};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[path = "drift_checker.rs"]
pub mod drift_checker;
#[path = "snapshot_daemon.rs"]
pub mod snapshot_daemon;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StateSnapshot {
    pub version: u32,
    pub created_at_ms: u64,
    pub last_event_sequence: u64,
    pub last_processed_block: u64,
    pub nonce_state: Vec<WalletNonceSnapshot>,
    pub active_positions: Vec<ActivePositionSnapshot>,
    pub pending_executions: Vec<ExecutionSnapshot>,
    pub lifecycle_state: Vec<ExecutionSnapshot>,
    pub risk_state: RiskStateSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletNonceSnapshot {
    pub wallet: Address,
    pub next_nonce: U256,
    pub generation: u64,
    pub pending: Vec<PendingNonceSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingNonceSnapshot {
    pub wallet: Address,
    pub nonce: U256,
    pub tx_hash: H256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionSnapshot {
    pub tx_hash: H256,
    pub opportunity_id: String,
    pub wallet: Address,
    pub nonce: U256,
    pub target_block: u64,
    pub stage: String,
    pub gas_price_wei: U256,
    pub tip_wei: U256,
    pub attempts: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivePositionSnapshot {
    pub opportunity_id: String,
    pub tx_hash: H256,
    pub wallet: Address,
    pub nonce: U256,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RiskStateSummary {
    pub survival_mode: bool,
    pub degradation_ewma: f64,
    pub bad_streak: u32,
    pub min_profit_multiplier: f64,
    pub tip_scaling_factor: f64,
}

#[derive(Debug, Clone)]
pub struct SnapshotStore {
    dir: PathBuf,
}

impl SnapshotStore {
    pub fn new(dir: impl AsRef<Path>) -> std::io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    pub fn save(&self, snapshot: &StateSnapshot) -> std::io::Result<PathBuf> {
        let mut snapshot = snapshot.clone();
        normalize_snapshot(&mut snapshot);
        let path = self.dir.join(format!(
            "snapshot-{:020}.json",
            snapshot.last_event_sequence
        ));
        let tmp = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(&snapshot).map_err(json_error)?;
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, &path)?;
        Ok(path)
    }

    pub fn load_latest(&self) -> std::io::Result<Option<StateSnapshot>> {
        let Some(path) = latest_snapshot_path(&self.dir)? else {
            return Ok(None);
        };
        let bytes = fs::read(path)?;
        let snapshot = serde_json::from_slice(&bytes).map_err(json_error)?;
        Ok(Some(snapshot))
    }
}

fn latest_snapshot_path(dir: &Path) -> std::io::Result<Option<PathBuf>> {
    let mut best: Option<(u64, PathBuf)> = None;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(number) = name
            .strip_prefix("snapshot-")
            .and_then(|value| value.strip_suffix(".json"))
        else {
            continue;
        };
        let Ok(sequence) = number.parse::<u64>() else {
            continue;
        };
        if best
            .as_ref()
            .map(|(seq, _)| sequence > *seq)
            .unwrap_or(true)
        {
            best = Some((sequence, path));
        }
    }
    Ok(best.map(|(_, path)| path))
}

fn normalize_snapshot(snapshot: &mut StateSnapshot) {
    snapshot
        .nonce_state
        .sort_by_key(|item| (item.wallet, item.next_nonce));
    for nonce in &mut snapshot.nonce_state {
        nonce
            .pending
            .sort_by_key(|item| (item.wallet, item.nonce, item.tx_hash));
    }
    snapshot
        .active_positions
        .sort_by_key(|item| (item.wallet, item.nonce, item.tx_hash));
    snapshot
        .pending_executions
        .sort_by_key(|item| (item.wallet, item.nonce, item.tx_hash));
    snapshot
        .lifecycle_state
        .sort_by_key(|item| (item.wallet, item.nonce, item.tx_hash));
}

fn json_error(error: serde_json::Error) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error)
}
