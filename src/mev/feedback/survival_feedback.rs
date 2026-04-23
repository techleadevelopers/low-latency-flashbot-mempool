use crate::mev::market_truth::execution_outcome_real::ExecutionOutcomeReal;
use crate::mev::market_truth::truth_pipeline::MarketTruthReport;
use crate::mev::survival::survival_gate::{SurvivalAdaptiveParams, SurvivalGateConfig};
use ethers::types::H256;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct SurvivalFeedbackMetrics {
    pub false_positives: u64,
    pub false_negatives: u64,
    pub correct_decisions: u64,
    pub low_confidence_ignored: u64,
    pub accepted_samples: u64,
    pub false_positive_ewma: f64,
    pub false_negative_ewma: f64,
    pub good_execution_ewma: f64,
    pub adaptation_drift: f64,
}

impl Default for SurvivalFeedbackMetrics {
    fn default() -> Self {
        Self {
            false_positives: 0,
            false_negatives: 0,
            correct_decisions: 0,
            low_confidence_ignored: 0,
            accepted_samples: 0,
            false_positive_ewma: 0.0,
            false_negative_ewma: 0.0,
            good_execution_ewma: 0.0,
            adaptation_drift: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SurvivalSampleClass {
    TooPermissive,
    TooStrict,
    Correct,
    IgnoredLowConfidence,
}

impl SurvivalSampleClass {
    pub fn as_str(self) -> &'static str {
        match self {
            SurvivalSampleClass::TooPermissive => "too_permissive",
            SurvivalSampleClass::TooStrict => "too_strict",
            SurvivalSampleClass::Correct => "correct",
            SurvivalSampleClass::IgnoredLowConfidence => "ignored_low_confidence",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurvivalFeedbackUpdate {
    pub tx_hash: H256,
    pub outcome: String,
    pub sample_class: String,
    pub accepted_sample: bool,
    pub false_positive_ewma: f64,
    pub false_negative_ewma: f64,
    pub good_execution_ewma: f64,
    pub false_positives: u64,
    pub false_negatives: u64,
    pub correct_decisions: u64,
    pub low_confidence_ignored: u64,
    pub accepted_samples: u64,
    pub adaptation_drift: f64,
    pub survival_probability_threshold: f64,
    pub max_competitor_capture: f64,
    pub max_latency_risk: f64,
    pub max_staleness: f64,
    pub min_pool_freshness: f64,
}

#[derive(Debug)]
pub struct SurvivalFeedbackEngine {
    base: SurvivalAdaptiveParams,
    params: SurvivalAdaptiveParams,
    metrics: SurvivalFeedbackMetrics,
    alpha: f64,
    min_samples: u64,
}

impl SurvivalFeedbackEngine {
    pub fn new(base: SurvivalAdaptiveParams) -> Self {
        let params = SurvivalGateConfig::update_adaptive(base);
        Self {
            base: params,
            params,
            metrics: SurvivalFeedbackMetrics::default(),
            alpha: 0.12,
            min_samples: 8,
        }
    }

    pub fn params(&self) -> SurvivalAdaptiveParams {
        self.params
    }

    pub fn metrics(&self) -> SurvivalFeedbackMetrics {
        self.metrics
    }

    pub fn ingest_report(&mut self, report: &MarketTruthReport) -> SurvivalFeedbackUpdate {
        let sample_class = classify_sample(report);
        let accepted_sample = !matches!(sample_class, SurvivalSampleClass::IgnoredLowConfidence);

        match sample_class {
            SurvivalSampleClass::TooPermissive => {
                self.metrics.false_positives = self.metrics.false_positives.saturating_add(1);
                self.metrics.false_positive_ewma =
                    ewma(self.metrics.false_positive_ewma, 1.0, self.alpha);
                self.metrics.false_negative_ewma =
                    ewma(self.metrics.false_negative_ewma, 0.0, self.alpha);
                self.metrics.good_execution_ewma =
                    ewma(self.metrics.good_execution_ewma, 0.0, self.alpha);
            }
            SurvivalSampleClass::TooStrict => {
                self.metrics.false_negatives = self.metrics.false_negatives.saturating_add(1);
                self.metrics.false_positive_ewma =
                    ewma(self.metrics.false_positive_ewma, 0.0, self.alpha);
                self.metrics.false_negative_ewma =
                    ewma(self.metrics.false_negative_ewma, 1.0, self.alpha);
                self.metrics.good_execution_ewma =
                    ewma(self.metrics.good_execution_ewma, 0.0, self.alpha);
            }
            SurvivalSampleClass::Correct => {
                self.metrics.correct_decisions = self.metrics.correct_decisions.saturating_add(1);
                self.metrics.false_positive_ewma =
                    ewma(self.metrics.false_positive_ewma, 0.0, self.alpha);
                self.metrics.false_negative_ewma =
                    ewma(self.metrics.false_negative_ewma, 0.0, self.alpha);
                self.metrics.good_execution_ewma =
                    ewma(self.metrics.good_execution_ewma, 1.0, self.alpha);
            }
            SurvivalSampleClass::IgnoredLowConfidence => {
                self.metrics.low_confidence_ignored =
                    self.metrics.low_confidence_ignored.saturating_add(1);
            }
        }

        if accepted_sample {
            self.metrics.accepted_samples = self.metrics.accepted_samples.saturating_add(1);
            if self.metrics.accepted_samples >= self.min_samples {
                self.params = recompute_params(self.base, self.metrics);
                self.params = SurvivalGateConfig::update_adaptive(self.params);
                self.metrics.adaptation_drift = adaptation_drift(self.base, self.params);
            }
        }

        SurvivalFeedbackUpdate {
            tx_hash: report.tx_hash,
            outcome: format!("{:?}", report.outcome),
            sample_class: sample_class.as_str().to_string(),
            accepted_sample,
            false_positive_ewma: self.metrics.false_positive_ewma,
            false_negative_ewma: self.metrics.false_negative_ewma,
            good_execution_ewma: self.metrics.good_execution_ewma,
            false_positives: self.metrics.false_positives,
            false_negatives: self.metrics.false_negatives,
            correct_decisions: self.metrics.correct_decisions,
            low_confidence_ignored: self.metrics.low_confidence_ignored,
            accepted_samples: self.metrics.accepted_samples,
            adaptation_drift: self.metrics.adaptation_drift,
            survival_probability_threshold: self.params.survival_probability_threshold,
            max_competitor_capture: self.params.max_competitor_capture,
            max_latency_risk: self.params.max_latency_risk,
            max_staleness: self.params.max_staleness,
            min_pool_freshness: self.params.min_pool_freshness,
        }
    }
}

fn classify_sample(report: &MarketTruthReport) -> SurvivalSampleClass {
    if !has_confident_market_truth(report) {
        return SurvivalSampleClass::IgnoredLowConfidence;
    }

    match report.outcome {
        ExecutionOutcomeReal::IncludedLoss
        | ExecutionOutcomeReal::IncludedToxicFill
        | ExecutionOutcomeReal::IncludedAdverseSelection
        | ExecutionOutcomeReal::IncludedLateFill => SurvivalSampleClass::TooPermissive,
        ExecutionOutcomeReal::MissedOpportunity
            if report.replay.missed_opportunity > 0.0 && report.markout.edge_real_value > 0.0 =>
        {
            SurvivalSampleClass::TooStrict
        }
        ExecutionOutcomeReal::IncludedProfit => SurvivalSampleClass::Correct,
        ExecutionOutcomeReal::IncludedPartialCapture if report.markout.edge_real_value > 0.0 => {
            SurvivalSampleClass::Correct
        }
        _ => SurvivalSampleClass::IgnoredLowConfidence,
    }
}

fn has_confident_market_truth(report: &MarketTruthReport) -> bool {
    let markout_present = report.markout.markout_100ms != 0.0
        || report.markout.markout_500ms != 0.0
        || report.markout.markout_1s != 0.0
        || report.markout.markout_5s != 0.0;
    let economics_present = report.realized_pnl.is_finite()
        && report.slippage_bps.is_finite()
        && report.fill_ratio.is_finite()
        && report.fill_ratio > 0.0;
    markout_present && economics_present
}

fn recompute_params(
    base: SurvivalAdaptiveParams,
    metrics: SurvivalFeedbackMetrics,
) -> SurvivalAdaptiveParams {
    let strictness_bias =
        (metrics.false_positive_ewma - metrics.false_negative_ewma).clamp(-1.0, 1.0);
    let stability_bonus = metrics.good_execution_ewma.clamp(0.0, 1.0);
    let adaptation_scale = (0.08 + (1.0 - stability_bonus) * 0.04).clamp(0.04, 0.12);

    SurvivalAdaptiveParams {
        survival_probability_threshold: (base.survival_probability_threshold
            + strictness_bias * adaptation_scale)
            .clamp(0.45, 0.95),
        max_competitor_capture: (base.max_competitor_capture
            - strictness_bias * adaptation_scale * 0.90)
            .clamp(0.05, 0.95),
        max_latency_risk: (base.max_latency_risk - strictness_bias * adaptation_scale * 0.70)
            .clamp(0.05, 0.95),
        max_staleness: (base.max_staleness - strictness_bias * adaptation_scale * 0.65)
            .clamp(0.05, 0.95),
        min_pool_freshness: (base.min_pool_freshness + strictness_bias * adaptation_scale * 0.75)
            .clamp(0.05, 0.95),
    }
}

fn adaptation_drift(base: SurvivalAdaptiveParams, current: SurvivalAdaptiveParams) -> f64 {
    ((base.survival_probability_threshold - current.survival_probability_threshold).abs()
        + (base.max_competitor_capture - current.max_competitor_capture).abs()
        + (base.max_latency_risk - current.max_latency_risk).abs()
        + (base.max_staleness - current.max_staleness).abs()
        + (base.min_pool_freshness - current.min_pool_freshness).abs())
        / 5.0
}

fn ewma(current: f64, sample: f64, alpha: f64) -> f64 {
    current * (1.0 - alpha) + sample * alpha
}
