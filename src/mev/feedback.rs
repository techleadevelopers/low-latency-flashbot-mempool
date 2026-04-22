use std::collections::VecDeque;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailureReason {
    NotIncluded,
    Outbid,
    Reverted,
    StaleState,
    LowProfit,
    RelayError,
    SimulationFailed,
    CapitalLimit,
}

impl FailureReason {
    pub fn as_str(self) -> &'static str {
        match self {
            FailureReason::NotIncluded => "not_included",
            FailureReason::Outbid => "outbid",
            FailureReason::Reverted => "reverted",
            FailureReason::StaleState => "stale_state",
            FailureReason::LowProfit => "low_profit",
            FailureReason::RelayError => "relay_error",
            FailureReason::SimulationFailed => "simulation_failed",
            FailureReason::CapitalLimit => "capital_limit",
        }
    }

    fn index(self) -> usize {
        match self {
            FailureReason::NotIncluded => 0,
            FailureReason::Outbid => 1,
            FailureReason::Reverted => 2,
            FailureReason::StaleState => 3,
            FailureReason::LowProfit => 4,
            FailureReason::RelayError => 5,
            FailureReason::SimulationFailed => 6,
            FailureReason::CapitalLimit => 7,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExecutionFeedback {
    pub success: bool,
    pub included: bool,
    pub profit_expected: f64,
    pub profit_realized: f64,
    pub gas_used: f64,
    pub tip_used: f64,
    pub competition_score: f64,
    pub confidence_score: f64,
    pub relay_used: String,
    pub block_delay: u64,
    pub failure_reason: Option<FailureReason>,
}

#[derive(Debug, Clone, Copy)]
pub struct RollingMetrics {
    pub inclusion_rate: f64,
    pub success_rate: f64,
    pub avg_realized_profit: f64,
    pub expected_vs_real_ratio: f64,
    pub tip_efficiency: f64,
    pub bad_streak: u32,
    pub samples: u64,
}

impl Default for RollingMetrics {
    fn default() -> Self {
        Self {
            inclusion_rate: 0.5,
            success_rate: 0.5,
            avg_realized_profit: 0.0,
            expected_vs_real_ratio: 1.0,
            tip_efficiency: 1.0,
            bad_streak: 0,
            samples: 0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AdaptiveThresholds {
    pub min_profit_multiplier: f64,
    pub max_competition_offset: f64,
    pub min_confidence_offset: f64,
    pub tip_scaling_factor: f64,
    pub execution_frequency_factor: f64,
}

impl Default for AdaptiveThresholds {
    fn default() -> Self {
        Self {
            min_profit_multiplier: 1.0,
            max_competition_offset: 0.0,
            min_confidence_offset: 0.0,
            tip_scaling_factor: 1.0,
            execution_frequency_factor: 1.0,
        }
    }
}

#[derive(Debug)]
pub struct FeedbackEngine {
    recent: VecDeque<ExecutionFeedback>,
    capacity: usize,
    metrics: RollingMetrics,
    thresholds: AdaptiveThresholds,
    failure_distribution: [u64; 8],
}

impl FeedbackEngine {
    pub fn new(capacity: usize) -> Self {
        Self {
            recent: VecDeque::with_capacity(capacity),
            capacity,
            metrics: RollingMetrics::default(),
            thresholds: AdaptiveThresholds::default(),
            failure_distribution: [0; 8],
        }
    }

    pub fn record(&mut self, feedback: ExecutionFeedback) {
        if self.recent.len() == self.capacity {
            if let Some(old) = self.recent.pop_front() {
                if let Some(reason) = old.failure_reason {
                    self.failure_distribution[reason.index()] =
                        self.failure_distribution[reason.index()].saturating_sub(1);
                }
            }
        }

        if let Some(reason) = feedback.failure_reason {
            self.failure_distribution[reason.index()] =
                self.failure_distribution[reason.index()].saturating_add(1);
        }

        self.update_metrics(&feedback);
        self.recent.push_back(feedback);
        self.update_thresholds();
    }

    pub fn metrics(&self) -> RollingMetrics {
        self.metrics
    }

    pub fn thresholds(&self) -> AdaptiveThresholds {
        self.thresholds
    }

    pub fn failure_count(&self, reason: FailureReason) -> u64 {
        self.failure_distribution[reason.index()]
    }

    fn update_metrics(&mut self, feedback: &ExecutionFeedback) {
        let alpha = 0.12;
        self.metrics.samples = self.metrics.samples.saturating_add(1);
        self.metrics.inclusion_rate = ewma(
            self.metrics.inclusion_rate,
            if feedback.included { 1.0 } else { 0.0 },
            alpha,
        );
        self.metrics.success_rate = ewma(
            self.metrics.success_rate,
            if feedback.success { 1.0 } else { 0.0 },
            alpha,
        );
        self.metrics.avg_realized_profit = ewma(
            self.metrics.avg_realized_profit,
            feedback.profit_realized,
            alpha,
        );

        let ratio = if feedback.profit_expected.abs() > f64::EPSILON {
            feedback.profit_realized / feedback.profit_expected
        } else {
            0.0
        };
        self.metrics.expected_vs_real_ratio = ewma(
            self.metrics.expected_vs_real_ratio,
            ratio.clamp(-2.0, 2.0),
            alpha,
        );

        let tip_efficiency = if feedback.tip_used > f64::EPSILON {
            feedback.profit_realized / feedback.tip_used
        } else {
            feedback.profit_realized.max(0.0)
        };
        self.metrics.tip_efficiency = ewma(
            self.metrics.tip_efficiency,
            tip_efficiency.clamp(-5.0, 20.0),
            alpha,
        );

        if feedback.success && feedback.profit_realized > 0.0 {
            self.metrics.bad_streak = 0;
        } else {
            self.metrics.bad_streak = self.metrics.bad_streak.saturating_add(1);
        }
    }

    fn update_thresholds(&mut self) {
        let bad = self.metrics.bad_streak as f64;
        let failure_pressure = (1.0 - self.metrics.success_rate).clamp(0.0, 1.0);
        let inclusion_pressure = (1.0 - self.metrics.inclusion_rate).clamp(0.0, 1.0);
        let poor_realization = (1.0 - self.metrics.expected_vs_real_ratio).clamp(0.0, 1.0);

        self.thresholds.min_profit_multiplier =
            (1.0 + failure_pressure * 0.7 + poor_realization * 0.5 + bad.min(8.0) * 0.05)
                .clamp(0.85, 2.5);
        self.thresholds.max_competition_offset =
            -(failure_pressure * 0.18 + bad.min(5.0) * 0.015).clamp(0.0, 0.30);
        self.thresholds.min_confidence_offset =
            (failure_pressure * 0.16 + poor_realization * 0.12).clamp(0.0, 0.30);
        self.thresholds.tip_scaling_factor = (1.0 + inclusion_pressure * 0.8
            - (self.metrics.tip_efficiency - 1.0).max(0.0) * 0.03)
            .clamp(0.65, 2.2);
        self.thresholds.execution_frequency_factor =
            (1.0 - failure_pressure * 0.5 - bad.min(10.0) * 0.035).clamp(0.20, 1.15);
    }
}

#[inline(always)]
fn ewma(previous: f64, current: f64, alpha: f64) -> f64 {
    previous * (1.0 - alpha) + current * alpha
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_streak_raises_thresholds() {
        let mut engine = FeedbackEngine::new(32);
        for _ in 0..6 {
            engine.record(ExecutionFeedback {
                success: false,
                included: false,
                profit_expected: 10.0,
                profit_realized: 0.0,
                gas_used: 4.0,
                tip_used: 1.0,
                competition_score: 0.8,
                confidence_score: 0.7,
                relay_used: "relay".to_string(),
                block_delay: 1,
                failure_reason: Some(FailureReason::NotIncluded),
            });
        }
        assert!(engine.thresholds().min_profit_multiplier > 1.0);
        assert!(engine.thresholds().execution_frequency_factor < 1.0);
    }
}
