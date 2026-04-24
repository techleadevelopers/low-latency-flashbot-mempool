#![allow(dead_code)]

use crate::config::ExecutionMode;
use ethers::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug)]
pub struct ProfitOutcome {
    pub net_profit_wei: Option<U256>,
    pub roi_bps: Option<u64>,
}

#[derive(Default, Serialize, Deserialize)]
struct CooldownState {
    wallet_last_execution_unix: HashMap<String, u64>,
}

pub struct CooldownGuard {
    path: PathBuf,
    state: CooldownState,
}

impl CooldownGuard {
    pub fn load(path: Option<&PathBuf>) -> Result<Option<Self>, Box<dyn Error>> {
        let Some(path) = path else {
            return Ok(None);
        };

        let state = if path.exists() {
            serde_json::from_str::<CooldownState>(&fs::read_to_string(path)?)?
        } else {
            CooldownState::default()
        };

        Ok(Some(Self {
            path: path.clone(),
            state,
        }))
    }

    pub fn ensure_ready(
        &self,
        wallet: Address,
        cooldown_seconds: Option<u64>,
    ) -> Result<(), Box<dyn Error>> {
        let Some(cooldown_seconds) = cooldown_seconds else {
            return Ok(());
        };
        let now = unix_now()?;
        let key = format!("{wallet:#x}");
        if let Some(last) = self.state.wallet_last_execution_unix.get(&key) {
            let next_allowed = last.saturating_add(cooldown_seconds);
            if now < next_allowed {
                return Err(format!(
                    "wallet cooldown active until unix timestamp {}",
                    next_allowed
                )
                .into());
            }
        }
        Ok(())
    }

    pub fn record_execution(&mut self, wallet: Address) -> Result<(), Box<dyn Error>> {
        self.state
            .wallet_last_execution_unix
            .insert(format!("{wallet:#x}"), unix_now()?);
        self.flush()
    }

    fn flush(&self) -> Result<(), Box<dyn Error>> {
        if let Some(parent) = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        fs::write(&self.path, serde_json::to_string_pretty(&self.state)?)?;
        Ok(())
    }
}

pub fn ensure_value_cap(value_wei: U256, cap_wei: U256) -> Result<(), Box<dyn Error>> {
    if value_wei > cap_wei {
        Err(format!(
            "max value per tx exceeded: value {} > cap {}",
            value_wei, cap_wei
        )
        .into())
    } else {
        Ok(())
    }
}

pub fn ensure_max_executions(executed: usize, cap: usize) -> Result<(), Box<dyn Error>> {
    if executed >= cap {
        Err(format!("max executions per run reached: {}", cap).into())
    } else {
        Ok(())
    }
}

pub fn evaluate_profit(
    value_wei: U256,
    gas_cost_wei: U256,
    min_net_profit_wei: U256,
    min_roi_bps: u64,
) -> Result<ProfitOutcome, Box<dyn Error>> {
    if value_wei <= gas_cost_wei {
        return Err(format!(
            "profit guard failed: value {} <= gas cost {}",
            value_wei, gas_cost_wei
        )
        .into());
    }

    let net_profit_wei = value_wei.saturating_sub(gas_cost_wei);
    if net_profit_wei < min_net_profit_wei {
        return Err(format!(
            "profit guard failed: net profit {} < threshold {}",
            net_profit_wei, min_net_profit_wei
        )
        .into());
    }

    let roi = if gas_cost_wei.is_zero() {
        u64::MAX
    } else {
        let raw = net_profit_wei
            .saturating_mul(U256::from(10_000u64))
            .checked_div(gas_cost_wei)
            .unwrap_or_else(U256::zero);
        raw.min(U256::from(u64::MAX)).as_u64()
    };

    if roi < min_roi_bps {
        return Err(format!(
            "profit guard failed: roi_bps {} < threshold {}",
            roi, min_roi_bps
        )
        .into());
    }

    Ok(ProfitOutcome {
        net_profit_wei: Some(net_profit_wei),
        roi_bps: Some(roi),
    })
}

pub fn not_applicable_profit() -> ProfitOutcome {
    ProfitOutcome {
        net_profit_wei: None,
        roi_bps: None,
    }
}

pub fn mode_allows_execution(mode: ExecutionMode) -> bool {
    matches!(mode, ExecutionMode::Execute)
}

fn unix_now() -> Result<u64, Box<dyn Error>> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}
