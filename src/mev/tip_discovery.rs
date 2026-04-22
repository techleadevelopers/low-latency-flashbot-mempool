use std::collections::{HashMap, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpportunityClass {
    BackrunV2,
    BackrunV3,
    Liquidation,
    Unknown,
}

#[derive(Debug, Clone, Copy)]
pub struct TipOutcome {
    pub class: OpportunityClass,
    pub tip_bps: u64,
    pub included: bool,
    pub profit_usd: f64,
    pub competition_score: f64,
}

#[derive(Debug, Clone, Copy)]
struct TipBand {
    attempts: u64,
    included: u64,
    avg_profit_usd: f64,
}

impl Default for TipBand {
    fn default() -> Self {
        Self {
            attempts: 0,
            included: 0,
            avg_profit_usd: 0.0,
        }
    }
}

#[derive(Debug)]
pub struct TipDiscoveryEngine {
    bands: HashMap<OpportunityClass, [TipBand; 8]>,
    recent: VecDeque<TipOutcome>,
    capacity: usize,
}

impl TipDiscoveryEngine {
    pub fn new(capacity: usize) -> Self {
        Self {
            bands: HashMap::new(),
            recent: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn record(&mut self, outcome: TipOutcome) {
        let idx = band_index(outcome.tip_bps);
        let bands = self
            .bands
            .entry(outcome.class)
            .or_insert([TipBand::default(); 8]);
        let band = &mut bands[idx];
        band.attempts = band.attempts.saturating_add(1);
        if outcome.included {
            band.included = band.included.saturating_add(1);
        }
        band.avg_profit_usd = band.avg_profit_usd * 0.88 + outcome.profit_usd * 0.12;

        if self.recent.len() == self.capacity {
            self.recent.pop_front();
        }
        self.recent.push_back(outcome);
    }

    pub fn recommended_tip_bps(
        &self,
        class: OpportunityClass,
        base_tip_bps: u64,
        competition_score: f64,
    ) -> u64 {
        let Some(bands) = self.bands.get(&class) else {
            return scale_for_competition(base_tip_bps, competition_score);
        };
        let mut best = base_tip_bps;
        let mut best_score = f64::MIN;
        for (idx, band) in bands.iter().enumerate() {
            if band.attempts < 3 {
                continue;
            }
            let inclusion = band.included as f64 / band.attempts as f64;
            let cost_penalty = idx as f64 * 0.08;
            let score = inclusion * 0.7 + band.avg_profit_usd.max(0.0).ln_1p() * 0.1 - cost_penalty;
            if score > best_score {
                best_score = score;
                best = band_midpoint_bps(idx);
            }
        }
        scale_for_competition(best, competition_score)
    }
}

fn band_index(tip_bps: u64) -> usize {
    match tip_bps {
        0..=499 => 0,
        500..=999 => 1,
        1000..=1499 => 2,
        1500..=1999 => 3,
        2000..=2999 => 4,
        3000..=4499 => 5,
        4500..=6999 => 6,
        _ => 7,
    }
}

fn band_midpoint_bps(idx: usize) -> u64 {
    [250, 750, 1250, 1750, 2500, 3750, 5750, 8000][idx.min(7)]
}

fn scale_for_competition(base: u64, competition: f64) -> u64 {
    (base as f64 * (1.0 + competition.clamp(0.0, 1.0) * 0.55)) as u64
}
