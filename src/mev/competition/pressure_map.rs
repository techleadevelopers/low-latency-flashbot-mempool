use crate::mev::competition::signal_extractor::CompetingSwapSignal;
use crate::mev::competition::CompetitionForecast;
use ethers::types::{Address, U256};
use std::collections::{HashMap, VecDeque};

#[derive(Debug, Clone, Copy)]
pub struct CompetitionPressure {
    pub pool: Address,
    pub heat: f64,
    pub mempool_congestion: f64,
    pub marginal_tip_wei: U256,
    pub forward_pressure: f64,
    pub execution_frequency_factor: f64,
    pub selectivity_multiplier: f64,
}

#[derive(Debug)]
pub struct PressureMap {
    pools: HashMap<Address, PoolPressure>,
    recent: VecDeque<Address>,
    capacity: usize,
}

#[derive(Debug, Clone, Copy)]
struct PoolPressure {
    heat: f64,
    forward_heat: f64,
    marginal_tip_wei: U256,
    updates: u64,
}

impl PressureMap {
    pub fn new(capacity: usize) -> Self {
        Self {
            pools: HashMap::with_capacity(capacity),
            recent: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    pub fn record_block(&mut self, signals: &[CompetingSwapSignal]) {
        for signal in signals.iter().take(self.capacity) {
            self.record_signal(*signal);
        }
        self.decay();
    }

    pub fn record_forecast(&mut self, pool: Address, forecast: CompetitionForecast) {
        self.ensure_capacity();
        self.recent.push_back(pool);
        let entry = self.pools.entry(pool).or_insert(PoolPressure {
            heat: 0.0,
            forward_heat: 0.0,
            marginal_tip_wei: U256::zero(),
            updates: 0,
        });
        entry.forward_heat =
            (entry.forward_heat * 0.70 + forecast.pressure_probability * 0.30).clamp(0.0, 1.0);
        if forecast.max_pending_tip_wei > entry.marginal_tip_wei {
            entry.marginal_tip_wei = forecast.max_pending_tip_wei;
        }
        entry.updates = entry.updates.saturating_add(1);
    }

    pub fn pressure(&self, pool: Address) -> CompetitionPressure {
        let pressure = self.pools.get(&pool).copied().unwrap_or(PoolPressure {
            heat: 0.0,
            forward_heat: 0.0,
            marginal_tip_wei: U256::zero(),
            updates: 0,
        });
        let congestion = (self.recent.len() as f64 / self.capacity.max(1) as f64).clamp(0.0, 1.0);
        let combined_heat = pressure.heat.max(pressure.forward_heat * 0.85);
        CompetitionPressure {
            pool,
            heat: combined_heat.clamp(0.0, 1.0),
            mempool_congestion: congestion,
            marginal_tip_wei: pressure.marginal_tip_wei,
            forward_pressure: pressure.forward_heat.clamp(0.0, 1.0),
            execution_frequency_factor: (1.0 - combined_heat * 0.55 - congestion * 0.20)
                .clamp(0.15, 1.0),
            selectivity_multiplier: (1.0 + combined_heat * 0.75 + congestion * 0.35)
                .clamp(1.0, 2.25),
        }
    }

    fn record_signal(&mut self, signal: CompetingSwapSignal) {
        self.ensure_capacity();
        self.recent.push_back(signal.pool);
        let entry = self.pools.entry(signal.pool).or_insert(PoolPressure {
            heat: 0.0,
            forward_heat: 0.0,
            marginal_tip_wei: U256::zero(),
            updates: 0,
        });
        entry.heat = (entry.heat * 0.80 + signal.aggressiveness * 0.20 + 0.05).clamp(0.0, 1.0);
        entry.marginal_tip_wei = if entry.marginal_tip_wei.is_zero() {
            signal.effective_tip_wei
        } else {
            (entry.marginal_tip_wei.saturating_mul(U256::from(4u64)) + signal.effective_tip_wei)
                / U256::from(5u64)
        };
        entry.updates = entry.updates.saturating_add(1);
    }

    fn ensure_capacity(&mut self) {
        if self.recent.len() == self.capacity {
            if let Some(old) = self.recent.pop_front() {
                if let Some(pool) = self.pools.get_mut(&old) {
                    pool.heat *= 0.96;
                    pool.forward_heat *= 0.98;
                }
            }
        }
    }

    fn decay(&mut self) {
        for pressure in self.pools.values_mut() {
            pressure.heat *= 0.985;
            pressure.forward_heat *= 0.92;
        }
    }
}
