use ethers::providers::Middleware;
use ethers::types::{Address, BlockId, BlockNumber, Filter, Log, ValueOrArray, H256, U256, U64};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

ethers::contract::abigen!(
    UniswapV2PairSnapshotView,
    r#"[
        function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast)
    ]"#,
);

const SYNC_TOPIC: H256 = H256([
    0x1c, 0x41, 0x1e, 0x9a, 0x96, 0xe0, 0x71, 0x24, 0x1c, 0x2f, 0x21, 0xf7, 0x72, 0x6b, 0x17, 0xae,
    0x89, 0xe3, 0xca, 0xb4, 0xc7, 0x8b, 0xe5, 0x0e, 0x06, 0x25, 0xb0, 0x3a, 0x9f, 0xa3, 0x4f, 0x6c,
]);
const SWAP_TOPIC: H256 = H256([
    0xd7, 0x8a, 0xd9, 0x5f, 0xa4, 0x6c, 0x99, 0x4b, 0x65, 0x51, 0xd0, 0xda, 0x85, 0xfc, 0x27, 0x5f,
    0xe6, 0x13, 0xce, 0x37, 0x65, 0x7f, 0xb8, 0xd5, 0xe3, 0xd1, 0x30, 0x84, 0x01, 0x59, 0xd8, 0x22,
]);

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct MarketSnapshot {
    pub timestamp_ms: u64,
    pub block_number: u64,
    pub pool_address: Address,
    pub price: f64,
    pub reserve_in: U256,
    pub reserve_out: U256,
}

#[derive(Debug, Clone, Copy)]
pub struct PoolMetadata {
    pub pool_address: Address,
    pub token0: Address,
    pub token1: Address,
    pub trade_input_token: Address,
    pub trade_output_token: Address,
}

#[derive(Debug)]
pub struct MarketSnapshotCollector {
    pools: HashMap<Address, PoolMetadata>,
    snapshots: HashMap<Address, VecDeque<MarketSnapshot>>,
    capacity_per_pool: usize,
    last_timestamp_ms: u64,
}

impl MarketSnapshotCollector {
    pub fn new(capacity_per_pool: usize) -> Self {
        Self {
            pools: HashMap::new(),
            snapshots: HashMap::new(),
            capacity_per_pool: capacity_per_pool.max(8),
            last_timestamp_ms: 0,
        }
    }

    pub fn register_pool(&mut self, metadata: PoolMetadata) {
        self.pools.insert(metadata.pool_address, metadata);
        self.snapshots
            .entry(metadata.pool_address)
            .or_insert_with(|| VecDeque::with_capacity(self.capacity_per_pool));
    }

    pub fn pools(&self) -> Vec<PoolMetadata> {
        self.pools.values().copied().collect()
    }

    pub fn ingest_snapshots(&mut self, snapshots: Vec<MarketSnapshot>) -> usize {
        let mut recorded = 0usize;
        for snapshot in snapshots {
            self.push_snapshot(snapshot);
            recorded = recorded.saturating_add(1);
        }
        recorded
    }

    pub async fn collect_block_snapshots<M: Middleware + 'static>(
        provider: Arc<M>,
        pools: &[PoolMetadata],
        block_number: u64,
    ) -> Result<Vec<MarketSnapshot>, M::Error> {
        if pools.is_empty() {
            return Ok(Vec::new());
        }
        let pool_map = pools
            .iter()
            .map(|pool| (pool.pool_address, *pool))
            .collect::<HashMap<_, _>>();
        let addresses = pools
            .iter()
            .map(|pool| pool.pool_address)
            .collect::<Vec<_>>();
        let filter = Filter::new()
            .address(ValueOrArray::Array(addresses))
            .from_block(BlockNumber::Number(U64::from(block_number)))
            .to_block(BlockNumber::Number(U64::from(block_number)))
            .topic0(ValueOrArray::Array(vec![SWAP_TOPIC, SYNC_TOPIC]));
        let logs = provider.get_logs(&filter).await?;
        if logs.is_empty() {
            return Ok(Vec::new());
        }
        let block_timestamp_ms = provider
            .get_block(BlockId::Number(U64::from(block_number).into()))
            .await?
            .and_then(|block| block.timestamp.as_u64().checked_mul(1_000))
            .unwrap_or_else(unix_ms);
        let mut last_timestamp_ms = 0u64;
        let mut out = Vec::new();
        for log in logs {
            if log.topics.first().copied() != Some(SYNC_TOPIC) {
                continue;
            }
            let Some(metadata) = pool_map.get(&log.address).copied() else {
                continue;
            };
            let Some((reserve0, reserve1)) = decode_sync(&log) else {
                continue;
            };
            let (reserve_in, reserve_out) = if metadata.trade_input_token == metadata.token0
                && metadata.trade_output_token == metadata.token1
            {
                (reserve0, reserve1)
            } else if metadata.trade_input_token == metadata.token1
                && metadata.trade_output_token == metadata.token0
            {
                (reserve1, reserve0)
            } else {
                continue;
            };
            let Some(price) = ratio_to_f64(reserve_out, reserve_in) else {
                continue;
            };
            let timestamp_ms = monotonic_timestamp(
                &mut last_timestamp_ms,
                block_timestamp_ms,
                log.log_index.unwrap_or_default().as_u64(),
            );
            out.push(MarketSnapshot {
                timestamp_ms,
                block_number,
                pool_address: metadata.pool_address,
                price,
                reserve_in,
                reserve_out,
            });
        }
        Ok(out)
    }

    pub fn collect_markout_snapshots(
        &self,
        pool_address: Address,
        tx_timestamp_ms: u64,
    ) -> Vec<MarketSnapshot> {
        let mut out = Vec::with_capacity(4);
        let Some(buffer) = self.snapshots.get(&pool_address) else {
            return out;
        };
        for delta_ms in [100u64, 500, 1_000, 5_000] {
            let target = tx_timestamp_ms.saturating_add(delta_ms);
            if let Some(snapshot) = buffer
                .iter()
                .filter(|snapshot| snapshot.timestamp_ms >= target)
                .min_by_key(|snapshot| snapshot.timestamp_ms)
                .copied()
            {
                out.push(snapshot);
            }
        }
        out
    }

    pub async fn sample_pool_state<M: Middleware + 'static>(
        provider: Arc<M>,
        metadata: PoolMetadata,
        timestamp_ms: u64,
    ) -> Result<Option<MarketSnapshot>, M::Error> {
        let pair = UniswapV2PairSnapshotView::new(metadata.pool_address, provider.clone());
        let reserves = pair.get_reserves().call().await?;
        let block_number = provider
            .get_block_number()
            .await
            .map(|block| block.as_u64())
            .unwrap_or_default();
        let reserve0 = U256::from(reserves.0);
        let reserve1 = U256::from(reserves.1);
        let (reserve_in, reserve_out) = if metadata.trade_input_token == metadata.token0
            && metadata.trade_output_token == metadata.token1
        {
            (reserve0, reserve1)
        } else if metadata.trade_input_token == metadata.token1
            && metadata.trade_output_token == metadata.token0
        {
            (reserve1, reserve0)
        } else {
            return Ok(None);
        };
        let Some(price) = ratio_to_f64(reserve_out, reserve_in) else {
            return Ok(None);
        };
        Ok(Some(MarketSnapshot {
            timestamp_ms,
            block_number,
            pool_address: metadata.pool_address,
            price,
            reserve_in,
            reserve_out,
        }))
    }

    fn push_snapshot(&mut self, snapshot: MarketSnapshot) {
        self.last_timestamp_ms = self.last_timestamp_ms.max(snapshot.timestamp_ms);
        let buffer = self
            .snapshots
            .entry(snapshot.pool_address)
            .or_insert_with(|| VecDeque::with_capacity(self.capacity_per_pool));
        if buffer.len() == self.capacity_per_pool {
            buffer.pop_front();
        }
        buffer.push_back(snapshot);
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ExecutionPriceObservation {
    pub execution_price: f64,
    pub amount_in: U256,
    pub amount_out: U256,
}

pub fn extract_execution_price(
    receipt: &ethers::types::TransactionReceipt,
    pool_address: Address,
    token0: Address,
    token1: Address,
    trade_input_token: Address,
    trade_output_token: Address,
) -> Option<ExecutionPriceObservation> {
    let swap = receipt.logs.iter().rev().find(|log| {
        log.address == pool_address && log.topics.first().copied() == Some(SWAP_TOPIC)
    })?;
    let (amount0_in, amount1_in, amount0_out, amount1_out) = decode_swap(swap)?;
    let (amount_in, amount_out) = if trade_input_token == token0 && trade_output_token == token1 {
        (
            if amount0_in.is_zero() {
                amount1_out
            } else {
                amount0_in
            },
            if amount1_out.is_zero() {
                amount0_out
            } else {
                amount1_out
            },
        )
    } else if trade_input_token == token1 && trade_output_token == token0 {
        (
            if amount1_in.is_zero() {
                amount0_out
            } else {
                amount1_in
            },
            if amount0_out.is_zero() {
                amount1_out
            } else {
                amount0_out
            },
        )
    } else {
        return None;
    };
    let execution_price = ratio_to_f64(amount_out, amount_in)?;
    Some(ExecutionPriceObservation {
        execution_price,
        amount_in,
        amount_out,
    })
}

fn decode_sync(log: &Log) -> Option<(U256, U256)> {
    if log.data.0.len() < 64 {
        return None;
    }
    let reserve0 = U256::from_big_endian(&log.data.0[0..32]);
    let reserve1 = U256::from_big_endian(&log.data.0[32..64]);
    Some((reserve0, reserve1))
}

fn decode_swap(log: &Log) -> Option<(U256, U256, U256, U256)> {
    if log.data.0.len() < 128 {
        return None;
    }
    Some((
        U256::from_big_endian(&log.data.0[0..32]),
        U256::from_big_endian(&log.data.0[32..64]),
        U256::from_big_endian(&log.data.0[64..96]),
        U256::from_big_endian(&log.data.0[96..128]),
    ))
}

pub fn ratio_to_f64(numerator: U256, denominator: U256) -> Option<f64> {
    if denominator.is_zero() {
        return None;
    }
    let num = numerator.to_string().parse::<f64>().ok()?;
    let den = denominator.to_string().parse::<f64>().ok()?;
    if !num.is_finite() || !den.is_finite() || den <= 0.0 {
        return None;
    }
    Some(num / den)
}

fn monotonic_timestamp(last: &mut u64, block_timestamp_ms: u64, log_index: u64) -> u64 {
    let candidate = block_timestamp_ms.saturating_add(log_index.min(999));
    let monotonic = candidate.max(last.saturating_add(1));
    *last = monotonic;
    monotonic
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}
