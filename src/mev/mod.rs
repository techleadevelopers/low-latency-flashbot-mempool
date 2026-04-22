pub mod amm;
pub mod analytics;
pub mod backrun;
pub mod capital;
pub mod execution;
pub mod opportunity;
pub mod pnl;
pub mod simulation;

use crate::config::{Config, MevStrategy};
use crate::dashboard::DashboardHandle;
use crate::rpc::RpcFleet;
use std::sync::Arc;

pub async fn run(
    config: Arc<Config>,
    rpc_fleet: Arc<RpcFleet>,
    dashboard: DashboardHandle,
) -> Result<(), Box<dyn std::error::Error>> {
    dashboard.event(
        "info",
        format!(
            "MEV engine started strategy={} mode={} capital={:.6} ETH",
            config.mev.strategy.as_str(),
            config.bot_mode.as_str(),
            config.mev.capital_eth
        ),
    );

    match config.mev.strategy {
        MevStrategy::Backrun => backrun::run(config, rpc_fleet, dashboard).await,
    }
}
