use crate::config::Config;
use ethers::types::U256;
use std::collections::VecDeque;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct RelayEndpoint {
    pub url: String,
    pub stats: RelayStats,
}

#[derive(Debug, Clone, Copy)]
pub struct RelayStats {
    pub attempts: u64,
    pub successes: u64,
    pub failures: u64,
    pub outbids: u64,
    pub reverts: u64,
    pub avg_latency_ms: f64,
    pub avg_required_tip_usd: f64,
}

impl Default for RelayStats {
    fn default() -> Self {
        Self {
            attempts: 0,
            successes: 0,
            failures: 0,
            outbids: 0,
            reverts: 0,
            avg_latency_ms: 120.0,
            avg_required_tip_usd: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct InclusionContext {
    pub expected_profit_usd: f64,
    pub gas_cost_usd: f64,
    pub competition_score: f64,
    pub confidence_score: f64,
    pub urgency: f64,
    pub recent_failures: u32,
    pub tip_scaling_factor: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct InclusionPlan {
    pub execute: bool,
    pub tip_wei: U256,
    pub tip_usd: f64,
    pub inclusion_probability: f64,
    pub adjusted_ev_usd: f64,
    pub attempts: u8,
}

#[derive(Debug, Clone, Copy)]
pub enum InclusionFailureReason {
    NotIncluded,
    Outbid,
    Revert,
    RelayError,
}

#[derive(Debug, Clone, Copy)]
pub struct InclusionFeedback {
    pub reason: InclusionFailureReason,
    pub tip_usd: f64,
    pub competition_score: f64,
}

pub struct InclusionEngine {
    relays: Vec<RelayEndpoint>,
    feedback: VecDeque<InclusionFeedback>,
    min_ev_usd: f64,
    max_attempts: u8,
    base_tip_bps: u64,
    max_tip_bps: u64,
}

impl InclusionEngine {
    pub fn from_config(config: &Config) -> Self {
        Self {
            relays: config
                .mev
                .builder_relays
                .iter()
                .map(|url| RelayEndpoint {
                    url: url.clone(),
                    stats: RelayStats::default(),
                })
                .collect(),
            feedback: VecDeque::with_capacity(128),
            min_ev_usd: config.mev.inclusion_min_ev_usd,
            max_attempts: config.mev.inclusion_max_attempts.max(1),
            base_tip_bps: config.mev.inclusion_base_tip_bps,
            max_tip_bps: config
                .mev
                .inclusion_max_tip_bps
                .max(config.mev.inclusion_base_tip_bps),
        }
    }

    #[inline(always)]
    pub fn plan(&self, context: InclusionContext, eth_usd_price: f64) -> InclusionPlan {
        let relay_perf = self.best_relay_performance();
        let recent_failure_penalty = (context.recent_failures as f64 * 0.06).min(0.35);
        let tip_bps = ((self.dynamic_tip_bps(context, relay_perf) as f64)
            * context.tip_scaling_factor.clamp(0.50, 2.50)) as u64;
        let tip_bps = tip_bps.min(self.max_tip_bps).max(self.base_tip_bps / 2);
        let tip_usd = context.expected_profit_usd * (tip_bps as f64 / 10_000.0);
        let inclusion_probability = inclusion_probability(
            tip_bps as f64 / 10_000.0,
            context.competition_score,
            relay_perf,
            recent_failure_penalty,
        );
        let adjusted_ev_usd =
            (context.expected_profit_usd - context.gas_cost_usd - tip_usd) * inclusion_probability;
        let execute = adjusted_ev_usd >= self.min_ev_usd && inclusion_probability > 0.20;
        let tip_eth = if eth_usd_price > 0.0 {
            tip_usd / eth_usd_price
        } else {
            0.0
        };
        let tip_wei =
            ethers::utils::parse_ether(format!("{tip_eth:.18}")).unwrap_or_else(|_| U256::zero());

        InclusionPlan {
            execute,
            tip_wei,
            tip_usd,
            inclusion_probability,
            adjusted_ev_usd,
            attempts: self.max_attempts,
        }
    }

    pub fn relay_order(&self, out: &mut [Option<usize>]) {
        for slot in out.iter_mut() {
            *slot = None;
        }
        for slot_idx in 0..out.len() {
            let mut best_idx = None;
            let mut best_score = f64::MIN;
            for idx in 0..self.relays.len() {
                if out[..slot_idx]
                    .iter()
                    .flatten()
                    .any(|selected| *selected == idx)
                {
                    continue;
                }
                let score = self.relay_score(idx);
                if score > best_score {
                    best_score = score;
                    best_idx = Some(idx);
                }
            }
            out[slot_idx] = best_idx;
        }
    }

    pub fn relay_url(&self, idx: usize) -> Option<&str> {
        self.relays.get(idx).map(|relay| relay.url.as_str())
    }

    pub fn record_success(&mut self, idx: usize, latency: Duration) {
        if let Some(relay) = self.relays.get_mut(idx) {
            relay.stats.attempts = relay.stats.attempts.saturating_add(1);
            relay.stats.successes = relay.stats.successes.saturating_add(1);
            relay.stats.avg_latency_ms =
                ewma(relay.stats.avg_latency_ms, latency.as_secs_f64() * 1_000.0);
        }
    }

    pub fn record_success_with_tip(&mut self, idx: usize, latency: Duration, tip_usd: f64) {
        if let Some(relay) = self.relays.get_mut(idx) {
            relay.stats.attempts = relay.stats.attempts.saturating_add(1);
            relay.stats.successes = relay.stats.successes.saturating_add(1);
            relay.stats.avg_latency_ms =
                ewma(relay.stats.avg_latency_ms, latency.as_secs_f64() * 1_000.0);
            relay.stats.avg_required_tip_usd = if relay.stats.avg_required_tip_usd == 0.0 {
                tip_usd
            } else {
                ewma_with_alpha(relay.stats.avg_required_tip_usd, tip_usd, 0.20)
            };
        }
    }

    pub fn record_failure(
        &mut self,
        idx: usize,
        reason: InclusionFailureReason,
        feedback: InclusionFeedback,
    ) {
        if let Some(relay) = self.relays.get_mut(idx) {
            relay.stats.attempts = relay.stats.attempts.saturating_add(1);
            relay.stats.failures = relay.stats.failures.saturating_add(1);
            match reason {
                InclusionFailureReason::Outbid => {
                    relay.stats.outbids = relay.stats.outbids.saturating_add(1)
                }
                InclusionFailureReason::Revert => {
                    relay.stats.reverts = relay.stats.reverts.saturating_add(1)
                }
                InclusionFailureReason::NotIncluded | InclusionFailureReason::RelayError => {}
            }
        }
        if self.feedback.len() == 128 {
            self.feedback.pop_front();
        }
        self.feedback.push_back(feedback);
    }

    #[inline(always)]
    fn dynamic_tip_bps(&self, context: InclusionContext, relay_perf: f64) -> u64 {
        let competition = context.competition_score.clamp(0.0, 1.0);
        let confidence = context.confidence_score.clamp(0.0, 1.0);
        let urgency = context.urgency.clamp(0.0, 1.0);
        let weak_relay = 1.0 - relay_perf.clamp(0.0, 1.0);
        let multiplier =
            1.0 + competition * 1.4 + confidence * 0.8 + urgency * 0.7 + weak_relay * 0.4;
        let raw = (self.base_tip_bps as f64 * multiplier) as u64;
        let low_ev_cap = if context.expected_profit_usd <= context.gas_cost_usd * 4.0 {
            self.max_tip_bps / 2
        } else {
            self.max_tip_bps
        };
        raw.min(low_ev_cap).max(self.base_tip_bps)
    }

    #[inline(always)]
    fn best_relay_performance(&self) -> f64 {
        let mut best: f64 = 0.50;
        for relay in &self.relays {
            best = f64::max(best, relay_performance(relay.stats));
        }
        best
    }

    #[inline(always)]
    fn relay_score(&self, idx: usize) -> f64 {
        self.relays
            .get(idx)
            .map(|relay| relay_performance(relay.stats) - relay.stats.avg_latency_ms / 10_000.0)
            .unwrap_or(0.0)
    }
}

#[inline(always)]
fn inclusion_probability(
    tip_ratio: f64,
    competition_score: f64,
    relay_performance: f64,
    recent_failure_penalty: f64,
) -> f64 {
    (0.18 + tip_ratio * 2.2 + relay_performance * 0.55
        - competition_score * 0.45
        - recent_failure_penalty)
        .clamp(0.0, 1.0)
}

#[inline(always)]
fn relay_performance(stats: RelayStats) -> f64 {
    if stats.attempts == 0 {
        return 0.55;
    }
    let success_rate = stats.successes as f64 / stats.attempts as f64;
    let revert_penalty = stats.reverts as f64 / stats.attempts as f64 * 0.25;
    let outbid_penalty = stats.outbids as f64 / stats.attempts as f64 * 0.15;
    (success_rate - revert_penalty - outbid_penalty).clamp(0.05, 0.98)
}

#[inline(always)]
fn ewma(previous: f64, current: f64) -> f64 {
    previous * 0.75 + current * 0.25
}

fn ewma_with_alpha(previous: f64, current: f64, alpha: f64) -> f64 {
    previous * (1.0 - alpha) + current * alpha
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn higher_competition_increases_tip() {
        let engine = InclusionEngine {
            relays: vec![RelayEndpoint {
                url: "relay".to_string(),
                stats: RelayStats::default(),
            }],
            feedback: VecDeque::new(),
            min_ev_usd: 1.0,
            max_attempts: 2,
            base_tip_bps: 500,
            max_tip_bps: 3000,
        };
        let low = engine.plan(
            InclusionContext {
                expected_profit_usd: 50.0,
                gas_cost_usd: 5.0,
                competition_score: 0.1,
                confidence_score: 0.8,
                urgency: 0.5,
                recent_failures: 0,
                tip_scaling_factor: 1.0,
            },
            3200.0,
        );
        let high = engine.plan(
            InclusionContext {
                competition_score: 0.9,
                ..InclusionContext {
                    expected_profit_usd: 50.0,
                    gas_cost_usd: 5.0,
                    competition_score: 0.1,
                    confidence_score: 0.8,
                    urgency: 0.5,
                    recent_failures: 0,
                    tip_scaling_factor: 1.0,
                }
            },
            3200.0,
        );
        assert!(high.tip_usd > low.tip_usd);
    }
}
