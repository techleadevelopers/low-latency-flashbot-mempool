use crate::mev::inclusion_truth::BundleOutcome;
use crate::mev::state::event_store::{
    EventStore, ExecutionEventRecord, StateEvent, TxFinalized, TxReplaced, TxSigned, TxSubmitted,
};
use crate::mev::state::snapshot::ExecutionSnapshot;
use ethers::types::{Address, H256, U256};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionStage {
    Built,
    Signed,
    Submitted,
    Included,
    Dropped,
    Outbid,
    Replaced,
    Cancelled,
    Reverted,
}

#[derive(Debug, Clone)]
pub struct ManagedExecution {
    pub tx_hash: H256,
    pub opportunity_id: String,
    pub wallet: Address,
    pub nonce: U256,
    pub target_block: u64,
    pub stage: ExecutionStage,
    pub gas_price_wei: U256,
    pub tip_wei: U256,
    pub attempts: u8,
    pub created_at: Instant,
    pub updated_at: Instant,
}

#[derive(Debug)]
pub struct TxLifecycleManager {
    executions: HashMap<H256, ManagedExecution>,
    by_key: HashMap<(Address, U256), H256>,
    order: VecDeque<H256>,
    capacity: usize,
    event_store: Option<Arc<EventStore>>,
}

impl TxLifecycleManager {
    pub fn new(capacity: usize) -> Self {
        Self {
            executions: HashMap::with_capacity(capacity.min(1024)),
            by_key: HashMap::with_capacity(capacity.min(1024)),
            order: VecDeque::with_capacity(capacity),
            capacity,
            event_store: None,
        }
    }

    pub fn with_event_store(mut self, event_store: Arc<EventStore>) -> Self {
        self.event_store = Some(event_store);
        self
    }

    pub fn set_event_store(&mut self, event_store: Arc<EventStore>) {
        self.event_store = Some(event_store);
    }

    pub fn register_signed(
        &mut self,
        opportunity_id: String,
        tx_hash: H256,
        wallet: Address,
        nonce: U256,
        target_block: u64,
        gas_price_wei: U256,
        tip_wei: U256,
    ) {
        self.evict_if_needed();
        let now = Instant::now();
        let execution = ManagedExecution {
            tx_hash,
            opportunity_id: opportunity_id.clone(),
            wallet,
            nonce,
            target_block,
            stage: ExecutionStage::Signed,
            gas_price_wei,
            tip_wei,
            attempts: 0,
            created_at: now,
            updated_at: now,
        };
        self.by_key.insert((wallet, nonce), tx_hash);
        self.executions.insert(tx_hash, execution);
        self.order.push_back(tx_hash);
        self.append_event(StateEvent::ExecutionEvent(ExecutionEventRecord {
            opportunity_id: opportunity_id.clone(),
            tx_hash,
            wallet,
            nonce,
            target_block,
            gas_price_wei,
            tip_wei,
            relay: String::new(),
        }));
        self.append_event(StateEvent::TxSigned(TxSigned {
            opportunity_id,
            tx_hash,
            wallet,
            nonce,
            target_block,
            gas_price_wei,
            tip_wei,
        }));
    }

    pub fn mark_submitted(&mut self, tx_hash: H256) {
        let submitted = if let Some(execution) = self.executions.get_mut(&tx_hash) {
            if !valid_transition(execution.stage, ExecutionStage::Submitted) {
                return;
            }
            execution.stage = ExecutionStage::Submitted;
            execution.attempts = execution.attempts.saturating_add(1);
            execution.updated_at = Instant::now();
            Some(TxSubmitted {
                tx_hash,
                wallet: execution.wallet,
                nonce: execution.nonce,
                relay: String::new(),
            })
        } else {
            None
        };
        if let Some(event) = submitted {
            self.append_event(StateEvent::TxSubmitted(event));
        }
    }

    pub fn mark_replaced(&mut self, old_hash: H256, new_hash: H256, gas_price_wei: U256) {
        if let Some(mut execution) = self.executions.remove(&old_hash) {
            if !valid_transition(execution.stage, ExecutionStage::Replaced) {
                self.executions.insert(old_hash, execution);
                return;
            }
            execution.tx_hash = new_hash;
            execution.stage = ExecutionStage::Replaced;
            execution.gas_price_wei = gas_price_wei;
            execution.updated_at = Instant::now();
            let wallet = execution.wallet;
            let nonce = execution.nonce;
            self.by_key.insert((wallet, nonce), new_hash);
            self.evict_if_needed();
            self.executions.insert(new_hash, execution);
            self.order.push_back(new_hash);
            self.append_event(StateEvent::TxReplaced(TxReplaced {
                old_tx_hash: old_hash,
                new_tx_hash: new_hash,
                wallet,
                nonce,
                new_gas_price_wei: gas_price_wei,
            }));
        }
    }

    pub fn finalize(&mut self, tx_hash: H256, outcome: BundleOutcome) -> Option<ManagedExecution> {
        let mut execution = self.executions.remove(&tx_hash)?;
        let final_stage = match outcome {
            BundleOutcome::Included | BundleOutcome::LateInclusion => ExecutionStage::Included,
            BundleOutcome::Outbid => ExecutionStage::Outbid,
            BundleOutcome::Reverted => ExecutionStage::Reverted,
            BundleOutcome::NotIncluded | BundleOutcome::Pending => ExecutionStage::Dropped,
        };
        if !valid_transition(execution.stage, final_stage) {
            self.executions.insert(tx_hash, execution);
            return None;
        }
        execution.stage = final_stage;
        execution.updated_at = Instant::now();
        self.by_key.remove(&(execution.wallet, execution.nonce));
        self.append_final_event(&execution, outcome);
        Some(execution)
    }

    pub fn active_for_nonce(&self, wallet: Address, nonce: U256) -> Option<&ManagedExecution> {
        self.by_key
            .get(&(wallet, nonce))
            .and_then(|hash| self.executions.get(hash))
    }

    pub fn restore_snapshot(&mut self, snapshots: &[ExecutionSnapshot]) {
        self.executions.clear();
        self.by_key.clear();
        self.order.clear();
        for snapshot in snapshots {
            if let Some(stage) = stage_from_str(&snapshot.stage) {
                self.insert_restored(ManagedExecution {
                    tx_hash: snapshot.tx_hash,
                    opportunity_id: snapshot.opportunity_id.clone(),
                    wallet: snapshot.wallet,
                    nonce: snapshot.nonce,
                    target_block: snapshot.target_block,
                    stage,
                    gas_price_wei: snapshot.gas_price_wei,
                    tip_wei: snapshot.tip_wei,
                    attempts: snapshot.attempts,
                    created_at: Instant::now(),
                    updated_at: Instant::now(),
                });
            }
        }
    }

    pub fn snapshot(&self) -> Vec<ExecutionSnapshot> {
        let mut out = self
            .executions
            .values()
            .map(|execution| ExecutionSnapshot {
                tx_hash: execution.tx_hash,
                opportunity_id: execution.opportunity_id.clone(),
                wallet: execution.wallet,
                nonce: execution.nonce,
                target_block: execution.target_block,
                stage: stage_as_str(execution.stage).to_string(),
                gas_price_wei: execution.gas_price_wei,
                tip_wei: execution.tip_wei,
                attempts: execution.attempts,
            })
            .collect::<Vec<_>>();
        out.sort_by_key(|item| (item.wallet, item.nonce, item.tx_hash));
        out
    }

    pub fn apply_signed(
        &mut self,
        opportunity_id: String,
        tx_hash: H256,
        wallet: Address,
        nonce: U256,
        target_block: u64,
        gas_price_wei: U256,
        tip_wei: U256,
    ) {
        if self.executions.contains_key(&tx_hash) {
            return;
        }
        self.insert_restored(ManagedExecution {
            tx_hash,
            opportunity_id,
            wallet,
            nonce,
            target_block,
            stage: ExecutionStage::Signed,
            gas_price_wei,
            tip_wei,
            attempts: 0,
            created_at: Instant::now(),
            updated_at: Instant::now(),
        });
    }

    pub fn apply_submitted(&mut self, tx_hash: H256) {
        if let Some(execution) = self.executions.get_mut(&tx_hash) {
            if valid_transition(execution.stage, ExecutionStage::Submitted) {
                execution.stage = ExecutionStage::Submitted;
                execution.attempts = execution.attempts.saturating_add(1);
                execution.updated_at = Instant::now();
            }
        }
    }

    pub fn apply_replaced(&mut self, old_hash: H256, new_hash: H256, gas_price_wei: U256) {
        if let Some(mut execution) = self.executions.remove(&old_hash) {
            if !valid_transition(execution.stage, ExecutionStage::Replaced) {
                self.executions.insert(old_hash, execution);
                return;
            }
            execution.tx_hash = new_hash;
            execution.stage = ExecutionStage::Replaced;
            execution.gas_price_wei = gas_price_wei;
            execution.updated_at = Instant::now();
            self.by_key
                .insert((execution.wallet, execution.nonce), new_hash);
            self.insert_restored(execution);
        }
    }

    pub fn apply_final(&mut self, tx_hash: H256, stage: ExecutionStage) {
        if let Some(mut execution) = self.executions.remove(&tx_hash) {
            if !valid_transition(execution.stage, stage) {
                self.executions.insert(tx_hash, execution);
                return;
            }
            execution.stage = stage;
            execution.updated_at = Instant::now();
            self.by_key.remove(&(execution.wallet, execution.nonce));
        }
    }

    fn insert_restored(&mut self, execution: ManagedExecution) {
        self.evict_if_needed();
        self.by_key
            .insert((execution.wallet, execution.nonce), execution.tx_hash);
        self.order.push_back(execution.tx_hash);
        self.executions.insert(execution.tx_hash, execution);
    }

    fn evict_if_needed(&mut self) {
        while self.order.len() >= self.capacity {
            let Some(hash) = self.order.pop_front() else {
                break;
            };
            if let Some(execution) = self.executions.remove(&hash) {
                self.by_key.remove(&(execution.wallet, execution.nonce));
            }
        }
    }

    fn append_final_event(&self, execution: &ManagedExecution, outcome: BundleOutcome) {
        let event = TxFinalized {
            tx_hash: execution.tx_hash,
            wallet: execution.wallet,
            nonce: execution.nonce,
            block_number: Some(execution.target_block),
            reason: Some(format!("{outcome:?}")),
        };
        let state_event = match outcome {
            BundleOutcome::Included | BundleOutcome::LateInclusion => StateEvent::TxIncluded(event),
            BundleOutcome::Outbid | BundleOutcome::NotIncluded | BundleOutcome::Pending => {
                StateEvent::TxDropped(event)
            }
            BundleOutcome::Reverted => StateEvent::TxDropped(event),
        };
        self.append_event(state_event);
    }

    fn append_event(&self, event: StateEvent) {
        if let Some(store) = &self.event_store {
            let _ = store.append(event);
        }
    }
}

fn valid_transition(from: ExecutionStage, to: ExecutionStage) -> bool {
    matches!(
        (from, to),
        (ExecutionStage::Built, ExecutionStage::Signed)
            | (ExecutionStage::Signed, ExecutionStage::Submitted)
            | (ExecutionStage::Signed, ExecutionStage::Cancelled)
            | (ExecutionStage::Submitted, ExecutionStage::Included)
            | (ExecutionStage::Submitted, ExecutionStage::Dropped)
            | (ExecutionStage::Submitted, ExecutionStage::Outbid)
            | (ExecutionStage::Submitted, ExecutionStage::Reverted)
            | (ExecutionStage::Submitted, ExecutionStage::Replaced)
            | (ExecutionStage::Replaced, ExecutionStage::Submitted)
            | (ExecutionStage::Replaced, ExecutionStage::Dropped)
            | (ExecutionStage::Replaced, ExecutionStage::Included)
            | (ExecutionStage::Replaced, ExecutionStage::Reverted)
    )
}

pub fn stage_as_str(stage: ExecutionStage) -> &'static str {
    match stage {
        ExecutionStage::Built => "built",
        ExecutionStage::Signed => "signed",
        ExecutionStage::Submitted => "submitted",
        ExecutionStage::Included => "included",
        ExecutionStage::Dropped => "dropped",
        ExecutionStage::Outbid => "outbid",
        ExecutionStage::Replaced => "replaced",
        ExecutionStage::Cancelled => "cancelled",
        ExecutionStage::Reverted => "reverted",
    }
}

pub fn stage_from_str(stage: &str) -> Option<ExecutionStage> {
    match stage {
        "built" => Some(ExecutionStage::Built),
        "signed" => Some(ExecutionStage::Signed),
        "submitted" => Some(ExecutionStage::Submitted),
        "included" => Some(ExecutionStage::Included),
        "dropped" => Some(ExecutionStage::Dropped),
        "outbid" => Some(ExecutionStage::Outbid),
        "replaced" => Some(ExecutionStage::Replaced),
        "cancelled" => Some(ExecutionStage::Cancelled),
        "reverted" => Some(ExecutionStage::Reverted),
        _ => None,
    }
}
