use crate::mev::execution::nonce_manager::NonceManager;
use crate::mev::execution::tx_lifecycle::TxLifecycleManager;
use crate::mev::state::event_store::EventStore;
use crate::mev::state::snapshot::{RiskStateSummary, SnapshotStore, StateSnapshot};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio::time::{Duration, MissedTickBehavior};

#[derive(Debug, Clone)]
pub struct SnapshotDaemonConfig {
    pub snapshot_interval_ms: u64,
    pub event_flush_threshold: usize,
    pub max_segment_size: usize,
}

impl SnapshotDaemonConfig {
    pub fn from_env() -> Self {
        Self {
            snapshot_interval_ms: read_env_u64("MEV_SNAPSHOT_INTERVAL_MS", 30_000),
            event_flush_threshold: read_env_usize("MEV_EVENT_FLUSH_THRESHOLD", 256),
            max_segment_size: read_env_usize("MEV_EVENT_MAX_SEGMENT_SIZE", 8 * 1024 * 1024),
        }
    }
}

#[derive(Clone)]
pub struct SnapshotDaemonHandle {
    force_tx: mpsc::Sender<SnapshotCommand>,
}

impl SnapshotDaemonHandle {
    pub async fn force_snapshot(&self) {
        let _ = self.force_tx.send(SnapshotCommand::ForceSnapshot).await;
    }
}

#[derive(Debug)]
enum SnapshotCommand {
    ForceSnapshot,
}

pub fn spawn_snapshot_daemon(
    state_dir: PathBuf,
    config: SnapshotDaemonConfig,
    event_store: Arc<EventStore>,
    nonce: Arc<Mutex<NonceManager>>,
    lifecycle: Arc<Mutex<TxLifecycleManager>>,
) -> SnapshotDaemonHandle {
    let (force_tx, mut force_rx) = mpsc::channel(8);
    let handle = SnapshotDaemonHandle {
        force_tx: force_tx.clone(),
    };
    tokio::spawn(async move {
        let snapshot_dir = state_dir.join("snapshots");
        let Ok(snapshot_store) = SnapshotStore::new(snapshot_dir) else {
            return;
        };
        let mut last_snapshot_sequence = 0u64;
        let mut interval = tokio::time::interval(Duration::from_millis(
            config.snapshot_interval_ms.max(5_000),
        ));
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    run_durability_pass(
                        &snapshot_store,
                        &event_store,
                        &nonce,
                        &lifecycle,
                        &config,
                        &mut last_snapshot_sequence,
                    ).await;
                }
                command = force_rx.recv() => {
                    match command {
                        Some(SnapshotCommand::ForceSnapshot) => {
                            run_forced_snapshot(
                                &snapshot_store,
                                &event_store,
                                &nonce,
                                &lifecycle,
                                &mut last_snapshot_sequence,
                            ).await;
                        }
                        None => break,
                    }
                }
            }
        }
    });
    handle
}

pub fn build_live_snapshot(
    nonce: &NonceManager,
    lifecycle: &TxLifecycleManager,
    last_processed_block: u64,
    last_event_sequence: u64,
    risk_state: RiskStateSummary,
) -> StateSnapshot {
    let lifecycle_state = lifecycle.snapshot();
    let pending_executions = lifecycle_state
        .iter()
        .filter(|execution| {
            matches!(
                execution.stage.as_str(),
                "signed" | "submitted" | "replaced"
            )
        })
        .cloned()
        .collect();
    StateSnapshot {
        version: 1,
        created_at_ms: unix_ms(),
        last_event_sequence,
        last_processed_block,
        nonce_state: nonce.snapshot(),
        active_positions: Vec::new(),
        pending_executions,
        lifecycle_state,
        risk_state,
    }
}

async fn run_durability_pass(
    snapshot_store: &SnapshotStore,
    event_store: &Arc<EventStore>,
    nonce: &Arc<Mutex<NonceManager>>,
    lifecycle: &Arc<Mutex<TxLifecycleManager>>,
    config: &SnapshotDaemonConfig,
    last_snapshot_sequence: &mut u64,
) {
    let _ = event_store.flush_if_needed(config.event_flush_threshold);
    let _ = event_store.rotate_if_needed(config.max_segment_size as u64);
    let current_sequence = event_store.current_sequence();
    if current_sequence <= *last_snapshot_sequence {
        return;
    }
    run_forced_snapshot(
        snapshot_store,
        event_store,
        nonce,
        lifecycle,
        last_snapshot_sequence,
    )
    .await;
}

async fn run_forced_snapshot(
    snapshot_store: &SnapshotStore,
    event_store: &Arc<EventStore>,
    nonce: &Arc<Mutex<NonceManager>>,
    lifecycle: &Arc<Mutex<TxLifecycleManager>>,
    last_snapshot_sequence: &mut u64,
) {
    let snapshot = {
        let Ok(nonce) = nonce.lock() else {
            return;
        };
        let Ok(lifecycle) = lifecycle.lock() else {
            return;
        };
        build_live_snapshot(
            &nonce,
            &lifecycle,
            0,
            event_store.current_sequence(),
            RiskStateSummary::default(),
        )
    };
    let snapshot_store = snapshot_store.clone();
    let sequence = snapshot.last_event_sequence;
    let saved = tokio::task::spawn_blocking(move || snapshot_store.save(&snapshot))
        .await
        .ok()
        .and_then(Result::ok)
        .is_some();
    if saved {
        *last_snapshot_sequence = sequence;
        let _ = event_store.flush();
    }
}

fn read_env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

fn read_env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}

pub fn state_dir(root: impl AsRef<Path>) -> PathBuf {
    root.as_ref().join("mev_state")
}
