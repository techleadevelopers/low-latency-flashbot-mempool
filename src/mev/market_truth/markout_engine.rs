use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MarketSnapshot {
    pub timestamp_ms: u64,
    pub price: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MarkoutResult {
    pub markout_100ms: f64,
    pub markout_500ms: f64,
    pub markout_1s: f64,
    pub markout_5s: f64,
    pub edge_real_value: f64,
    pub adverse_selection_score: f64,
    pub fill_quality_score: f64,
    pub execution_toxicity_index: f64,
}

pub type MarkoutMetrics = MarkoutResult;

impl Default for MarkoutResult {
    fn default() -> Self {
        Self {
            markout_100ms: 0.0,
            markout_500ms: 0.0,
            markout_1s: 0.0,
            markout_5s: 0.0,
            edge_real_value: 0.0,
            adverse_selection_score: 0.0,
            fill_quality_score: 1.0,
            execution_toxicity_index: 0.0,
        }
    }
}

pub struct MarkoutEngine;

impl MarkoutEngine {
    pub fn compute(
        entry_timestamp_ms: u64,
        entry_price: f64,
        execution_price: f64,
        snapshots: &[MarketSnapshot],
    ) -> MarkoutResult {
        if !entry_price.is_finite() || entry_price <= 0.0 || snapshots.is_empty() {
            return MarkoutMetrics::default();
        }
        let markout_100ms = markout(entry_timestamp_ms, entry_price, snapshots, 100);
        let markout_500ms = markout(entry_timestamp_ms, entry_price, snapshots, 500);
        let markout_1s = markout(entry_timestamp_ms, entry_price, snapshots, 1_000);
        let markout_5s = markout(entry_timestamp_ms, entry_price, snapshots, 5_000);
        let fill_quality_score = fill_quality(entry_price, execution_price);
        let adverse_selection_score =
            adverse_selection([markout_100ms, markout_500ms, markout_1s, markout_5s]);
        let execution_toxicity_index =
            (adverse_selection_score * 0.65 + (1.0 - fill_quality_score) * 0.35).clamp(0.0, 1.0);

        MarkoutResult {
            markout_100ms,
            markout_500ms,
            markout_1s,
            markout_5s,
            edge_real_value: markout_5s,
            adverse_selection_score,
            fill_quality_score,
            execution_toxicity_index,
        }
    }
}

fn markout(
    entry_timestamp_ms: u64,
    entry_price: f64,
    snapshots: &[MarketSnapshot],
    delta_ms: u64,
) -> f64 {
    let target = entry_timestamp_ms.saturating_add(delta_ms);
    snapshots
        .iter()
        .filter(|snapshot| snapshot.timestamp_ms >= target)
        .min_by_key(|snapshot| snapshot.timestamp_ms)
        .map(|snapshot| snapshot.price - entry_price)
        .unwrap_or(0.0)
}

fn fill_quality(entry_price: f64, execution_price: f64) -> f64 {
    if !execution_price.is_finite() || execution_price <= 0.0 || entry_price <= 0.0 {
        return 1.0;
    }
    let relative_error = ((execution_price - entry_price) / entry_price).abs();
    (1.0 - relative_error * 50.0).clamp(0.0, 1.0)
}

fn adverse_selection(markouts: [f64; 4]) -> f64 {
    let negative_pressure = markouts
        .iter()
        .filter(|value| **value < 0.0)
        .map(|value| value.abs())
        .sum::<f64>();
    (negative_pressure / 4.0).clamp(0.0, 1.0)
}
