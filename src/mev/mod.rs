pub mod amm;
pub mod analytics;
pub mod backrun;
pub mod block_loop;
pub mod capital;
pub mod competition;
pub mod execution;
pub mod feedback;
pub mod inclusion;
pub mod inclusion_truth;
pub mod market_truth;
pub mod meta_decision;
pub mod opportunity;
pub mod pnl;
pub mod post_block;
pub mod simulation;
pub mod state;
pub mod tip_discovery;

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
