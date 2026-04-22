use crate::mev::inclusion_truth::{BundleOutcome, InclusionTruth};
use ethers::types::{Address, H256};
use std::collections::VecDeque;

#[derive(Debug, Clone, Copy)]
pub enum PostBlockFailure {
    NotIncluded,
    Outbid,
    Reverted,
    Late,
}

#[derive(Debug, Clone)]
pub struct MissedOpportunityAnalysis {
    pub tx_hash: H256,
    pub pool: Option<Address>,
    pub failure: PostBlockFailure,
    pub estimated_lost_profit_usd: f64,
    pub latency_impact_ms: u128,
}

#[derive(Debug)]
pub struct PostBlockAnalyzer {
    recent: VecDeque<MissedOpportunityAnalysis>,
    capacity: usize,
}

impl PostBlockAnalyzer {
    pub fn new(capacity: usize) -> Self {
        Self {
            recent: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn analyze_truth(&mut self, truth: &InclusionTruth) -> Option<MissedOpportunityAnalysis> {
        let failure = match truth.outcome {
            BundleOutcome::Included => return None,
            BundleOutcome::Pending => return None,
            BundleOutcome::NotIncluded => PostBlockFailure::NotIncluded,
            BundleOutcome::Outbid => PostBlockFailure::Outbid,
            BundleOutcome::Reverted => PostBlockFailure::Reverted,
            BundleOutcome::LateInclusion => PostBlockFailure::Late,
        };
        let lost_multiplier = match failure {
            PostBlockFailure::Outbid => 0.85,
            PostBlockFailure::Late => 0.55,
            PostBlockFailure::NotIncluded => 0.70,
            PostBlockFailure::Reverted => 0.0,
        };
        let analysis = MissedOpportunityAnalysis {
            tx_hash: truth.tx_hash,
            pool: None,
            failure,
            estimated_lost_profit_usd: truth.expected_profit_usd * lost_multiplier,
            latency_impact_ms: truth.latency_ms,
        };
        if self.recent.len() == self.capacity {
            self.recent.pop_front();
        }
        self.recent.push_back(analysis.clone());
        Some(analysis)
    }

    pub fn recent(&self) -> impl Iterator<Item = &MissedOpportunityAnalysis> {
        self.recent.iter()
    }
}
