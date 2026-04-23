use crate::mev::state::snapshot::{ExecutionSnapshot, StateSnapshot, WalletNonceSnapshot};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriftSeverity {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone)]
pub struct StateDriftReport {
    pub drift_detected: bool,
    pub severity: DriftSeverity,
    pub mismatches: Vec<String>,
}

pub fn compare(snapshot_state: &StateSnapshot, live_state: &StateSnapshot) -> StateDriftReport {
    let mut mismatches = Vec::new();
    compare_nonce_state(
        &snapshot_state.nonce_state,
        &live_state.nonce_state,
        &mut mismatches,
    );
    compare_lifecycle_state(
        &snapshot_state.lifecycle_state,
        &live_state.lifecycle_state,
        &mut mismatches,
    );
    compare_pending_executions(
        &snapshot_state.pending_executions,
        &live_state.pending_executions,
        &mut mismatches,
    );
    compare_risk_summary(snapshot_state, live_state, &mut mismatches);

    let severity = if mismatches.iter().any(|item| {
        item.contains("nonce")
            || item.contains("missing live pending")
            || item.contains("lifecycle stage")
    }) {
        DriftSeverity::High
    } else if mismatches.len() > 2 {
        DriftSeverity::Medium
    } else {
        DriftSeverity::Low
    };

    StateDriftReport {
        drift_detected: !mismatches.is_empty(),
        severity,
        mismatches,
    }
}

fn compare_nonce_state(
    snapshot: &[WalletNonceSnapshot],
    live: &[WalletNonceSnapshot],
    mismatches: &mut Vec<String>,
) {
    let snapshot_map = snapshot
        .iter()
        .map(|item| (item.wallet, item))
        .collect::<HashMap<_, _>>();
    for live_nonce in live {
        match snapshot_map.get(&live_nonce.wallet) {
            Some(snapshot_nonce) => {
                if snapshot_nonce.next_nonce != live_nonce.next_nonce {
                    mismatches.push(format!(
                        "nonce mismatch wallet={:?} snapshot={} live={}",
                        live_nonce.wallet, snapshot_nonce.next_nonce, live_nonce.next_nonce
                    ));
                }
                if snapshot_nonce.pending.len() != live_nonce.pending.len() {
                    mismatches.push(format!(
                        "pending nonce mismatch wallet={:?} snapshot={} live={}",
                        live_nonce.wallet,
                        snapshot_nonce.pending.len(),
                        live_nonce.pending.len()
                    ));
                }
            }
            None => mismatches.push(format!(
                "missing snapshot nonce wallet={:?} live_next={}",
                live_nonce.wallet, live_nonce.next_nonce
            )),
        }
    }
}

fn compare_lifecycle_state(
    snapshot: &[ExecutionSnapshot],
    live: &[ExecutionSnapshot],
    mismatches: &mut Vec<String>,
) {
    let snapshot_map = snapshot
        .iter()
        .map(|item| (item.tx_hash, item))
        .collect::<HashMap<_, _>>();
    for live_execution in live {
        match snapshot_map.get(&live_execution.tx_hash) {
            Some(snapshot_execution) => {
                if snapshot_execution.stage != live_execution.stage {
                    mismatches.push(format!(
                        "lifecycle stage mismatch tx={:?} snapshot={} live={}",
                        live_execution.tx_hash, snapshot_execution.stage, live_execution.stage
                    ));
                }
            }
            None => mismatches.push(format!(
                "missing snapshot lifecycle tx={:?} stage={}",
                live_execution.tx_hash, live_execution.stage
            )),
        }
    }
}

fn compare_pending_executions(
    snapshot: &[ExecutionSnapshot],
    live: &[ExecutionSnapshot],
    mismatches: &mut Vec<String>,
) {
    let snapshot_map = snapshot
        .iter()
        .map(|item| (item.tx_hash, item))
        .collect::<HashMap<_, _>>();
    for live_execution in live {
        if !snapshot_map.contains_key(&live_execution.tx_hash) {
            mismatches.push(format!(
                "missing live pending in snapshot tx={:?}",
                live_execution.tx_hash
            ));
        }
    }
}

fn compare_risk_summary(
    snapshot_state: &StateSnapshot,
    live_state: &StateSnapshot,
    mismatches: &mut Vec<String>,
) {
    let snapshot = &snapshot_state.risk_state;
    let live = &live_state.risk_state;
    if snapshot.survival_mode != live.survival_mode {
        mismatches.push(format!(
            "risk survival mismatch snapshot={} live={}",
            snapshot.survival_mode, live.survival_mode
        ));
    }
    if (snapshot.degradation_ewma - live.degradation_ewma).abs() > 0.20 {
        mismatches.push(format!(
            "risk ewma deviation snapshot={:.4} live={:.4}",
            snapshot.degradation_ewma, live.degradation_ewma
        ));
    }
}
