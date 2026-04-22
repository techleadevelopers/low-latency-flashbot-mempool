use crate::config::MevConfig;
use crate::mev::opportunity::{wei_to_eth_f64, MevOpportunity};
use ethers::types::U256;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct CapitalManager {
    current_capital_wei: U256,
    max_daily_loss_wei: U256,
    max_gas_window_wei: U256,
    gas_window: Duration,
    max_allocation_bps: u64,
    daily_pnl_wei: i128,
    peak_capital_wei: U256,
    gas_spend_events: VecDeque<(Instant, U256)>,
    last_execution_at: Option<Instant>,
    cooldown: Duration,
}

#[derive(Debug, Clone)]
pub struct CapitalDecision {
    pub accepted: bool,
    pub reason: String,
    pub allocation_wei: U256,
    pub drawdown_bps: u64,
}

impl CapitalManager {
    pub fn from_config(config: &MevConfig) -> Result<Self, Box<dyn std::error::Error>> {
        let capital = ethers::utils::parse_ether(config.capital_eth.to_string())?;
        Ok(Self {
            current_capital_wei: capital,
            max_daily_loss_wei: ethers::utils::parse_ether(config.max_daily_loss_eth.to_string())?,
            max_gas_window_wei: ethers::utils::parse_ether(
                config.max_gas_spend_window_eth.to_string(),
            )?,
            gas_window: Duration::from_secs(config.gas_spend_window_secs),
            max_allocation_bps: config.max_allocation_bps,
            daily_pnl_wei: 0,
            peak_capital_wei: capital,
            gas_spend_events: VecDeque::new(),
            last_execution_at: None,
            cooldown: Duration::from_millis(config.execution_cooldown_ms),
        })
    }

    pub fn evaluate(&mut self, opportunity: &MevOpportunity) -> CapitalDecision {
        self.expire_gas_window();

        if self.daily_pnl_wei < 0
            && U256::from((-self.daily_pnl_wei) as u128) >= self.max_daily_loss_wei
        {
            return self.reject("daily stop-loss active");
        }

        if let Some(last) = self.last_execution_at {
            if last.elapsed() < self.cooldown {
                return self.reject("execution cooldown active");
            }
        }

        let window_spend = self.window_gas_spend();
        if window_spend.saturating_add(opportunity.score.execution_cost_wei)
            > self.max_gas_window_wei
        {
            return self.reject("gas spend window cap exceeded");
        }

        let allocation_cap = self
            .current_capital_wei
            .saturating_mul(U256::from(self.max_allocation_bps))
            / U256::from(10_000u64);
        let allocation = opportunity.allocation_wei(allocation_cap);

        if allocation.is_zero() {
            return self.reject("zero allocation after cap");
        }

        if opportunity.score.execution_cost_wei >= allocation {
            return self.reject("execution cost exceeds allocation");
        }

        CapitalDecision {
            accepted: true,
            reason: "accepted".to_string(),
            allocation_wei: allocation,
            drawdown_bps: self.drawdown_bps(),
        }
    }

    pub fn reserve_execution(&mut self, gas_cost_wei: U256) {
        self.last_execution_at = Some(Instant::now());
        self.gas_spend_events
            .push_back((Instant::now(), gas_cost_wei));
    }

    pub fn record_pnl(&mut self, pnl_wei: i128) {
        self.daily_pnl_wei = self.daily_pnl_wei.saturating_add(pnl_wei);
        if pnl_wei >= 0 {
            self.current_capital_wei = self
                .current_capital_wei
                .saturating_add(U256::from(pnl_wei as u128));
        } else {
            self.current_capital_wei = self
                .current_capital_wei
                .saturating_sub(U256::from((-pnl_wei) as u128));
        }
        self.peak_capital_wei = self.peak_capital_wei.max(self.current_capital_wei);
    }

    pub fn current_capital_eth(&self) -> f64 {
        wei_to_eth_f64(self.current_capital_wei)
    }

    pub fn daily_pnl_eth(&self) -> f64 {
        self.daily_pnl_wei as f64 / 1e18
    }

    fn reject(&self, reason: &str) -> CapitalDecision {
        CapitalDecision {
            accepted: false,
            reason: reason.to_string(),
            allocation_wei: U256::zero(),
            drawdown_bps: self.drawdown_bps(),
        }
    }

    fn expire_gas_window(&mut self) {
        while let Some((at, _)) = self.gas_spend_events.front() {
            if at.elapsed() <= self.gas_window {
                break;
            }
            self.gas_spend_events.pop_front();
        }
    }

    fn window_gas_spend(&self) -> U256 {
        self.gas_spend_events
            .iter()
            .fold(U256::zero(), |acc, (_, value)| acc.saturating_add(*value))
    }

    fn drawdown_bps(&self) -> u64 {
        if self.peak_capital_wei.is_zero() || self.current_capital_wei >= self.peak_capital_wei {
            return 0;
        }
        let drawdown = self
            .peak_capital_wei
            .saturating_sub(self.current_capital_wei);
        (drawdown.saturating_mul(U256::from(10_000u64)) / self.peak_capital_wei).as_u64()
    }
}
