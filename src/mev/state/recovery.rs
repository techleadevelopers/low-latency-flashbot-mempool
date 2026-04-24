use crate::mev::execution::nonce_manager::NonceManager;
use crate::mev::execution::tx_lifecycle::{ExecutionStage, TxLifecycleManager};
use crate::mev::state::event_store::{EventEnvelope, EventStore, StateEvent};
use crate::mev::state::snapshot::{RiskStateSummary, SnapshotStore, StateSnapshot};
use ethers::providers::{Http, Middleware, Provider};
use ethers::types::Address;
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct RecoveryReport {
    pub snapshot_loaded: bool,
    pub snapshot_sequence: u64,
    pub events_replayed: usize,
    pub last_sequence: u64,
}

#[derive(Debug)]
pub struct RecoveryEngine {
    snapshot_store: SnapshotStore,
    event_store: Arc<EventStore>,
}

impl RecoveryEngine {
    pub fn open(state_dir: impl AsRef<Path>) -> std::io::Result<Self> {
        let state_dir = state_dir.as_ref();
        let snapshot_store = SnapshotStore::new(state_dir.join("snapshots"))?;
        let event_store = Arc::new(EventStore::open(state_dir.join("events"))?);
        Ok(Self {
            snapshot_store,
            event_store,
        })
    }

    pub fn event_store(&self) -> Arc<EventStore> {
        self.event_store.clone()
    }

    pub fn recover_managers(
        &self,
        nonce: &mut NonceManager,
        lifecycle: &mut TxLifecycleManager,
    ) -> std::io::Result<RecoveryReport> {
        let snapshot = self.snapshot_store.load_latest()?;
        let snapshot_sequence = snapshot
            .as_ref()
            .map(|snapshot| snapshot.last_event_sequence)
            .unwrap_or(0);

        if let Some(snapshot) = snapshot {
            apply_snapshot(nonce, lifecycle, &snapshot);
        }

        let events = self.event_store.replay_after(snapshot_sequence)?;
        let last_sequence = events
            .last()
            .map(|event| event.sequence)
            .unwrap_or(snapshot_sequence);
        replay_events(nonce, lifecycle, &events);

        Ok(RecoveryReport {
            snapshot_loaded: snapshot_sequence > 0,
            snapshot_sequence,
            events_replayed: events.len(),
            last_sequence,
        })
    }

    pub fn save_snapshot(
        &self,
        nonce: &NonceManager,
        lifecycle: &TxLifecycleManager,
        last_processed_block: u64,
        risk_state: RiskStateSummary,
    ) -> std::io::Result<()> {
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
        let snapshot = StateSnapshot {
            version: 1,
            created_at_ms: unix_ms(),
            last_event_sequence: self.event_store.current_sequence(),
            last_processed_block,
            nonce_state: nonce.snapshot(),
            active_positions: Vec::new(),
            pending_executions,
            lifecycle_state,
            risk_state,
        };
        self.snapshot_store.save(&snapshot).map(|_| ())
    }

    pub async fn reconcile_wallet_nonces(
        &self,
        provider: Arc<Provider<Http>>,
        nonce: &mut NonceManager,
        wallets: &[Address],
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut unique = BTreeSet::new();
        unique.extend(wallets.iter().copied());
        for wallet in unique {
            let chain_pending = provider
                .get_transaction_count(wallet, Some(ethers::types::BlockNumber::Pending.into()))
                .await?;
            nonce.reconcile_chain_pending_nonce(wallet, chain_pending);
        }
        Ok(())
    }
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}

pub fn apply_snapshot(
    nonce: &mut NonceManager,
    lifecycle: &mut TxLifecycleManager,
    snapshot: &StateSnapshot,
) {
    nonce.restore_snapshot(&snapshot.nonce_state);
    lifecycle.restore_snapshot(&snapshot.lifecycle_state);
}

pub fn replay_events(
    nonce: &mut NonceManager,
    lifecycle: &mut TxLifecycleManager,
    events: &[EventEnvelope],
) {
    for envelope in events {
        match &envelope.event {
            StateEvent::NonceReserved(event) => nonce.apply_nonce_reserved(
                event.wallet,
                event.nonce,
                event.generation,
                event.chain_pending_nonce,
            ),
            StateEvent::ExecutionEvent(event) => lifecycle.apply_signed(
                event.opportunity_id.clone(),
                event.tx_hash,
                event.wallet,
                event.nonce,
                event.target_block,
                event.gas_price_wei,
                event.tip_wei,
            ),
            StateEvent::TxSigned(event) => lifecycle.apply_signed(
                event.opportunity_id.clone(),
                event.tx_hash,
                event.wallet,
                event.nonce,
                event.target_block,
                event.gas_price_wei,
                event.tip_wei,
            ),
            StateEvent::TxSubmitted(event) => {
                nonce.insert_pending(event.wallet, event.nonce, event.tx_hash);
                lifecycle.apply_submitted(event.tx_hash);
            }
            StateEvent::TxIncluded(event) => {
                nonce.finalize(event.wallet, event.nonce);
                lifecycle.apply_final(event.tx_hash, ExecutionStage::Included);
            }
            StateEvent::TxDropped(event) => {
                nonce.finalize(event.wallet, event.nonce);
                lifecycle.apply_final(event.tx_hash, ExecutionStage::Dropped);
            }
            StateEvent::TxCancelled(event) => {
                nonce.finalize(event.wallet, event.nonce);
                lifecycle.apply_final(event.tx_hash, ExecutionStage::Cancelled);
            }
            StateEvent::TxReplaced(event) => {
                nonce.insert_pending(event.wallet, event.nonce, event.new_tx_hash);
                lifecycle.apply_replaced(
                    event.old_tx_hash,
                    event.new_tx_hash,
                    event.new_gas_price_wei,
                );
            }
            StateEvent::RiskDecision(_)
            | StateEvent::InclusionTruthUpdate(_)
            | StateEvent::MarketTruthUpdate(_)
            | StateEvent::SurvivalFeedbackUpdate(_) => {}
        }
    }
}
