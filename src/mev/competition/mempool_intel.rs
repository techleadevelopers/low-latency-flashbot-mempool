use ethers::abi::{self, ParamType, Token};
use ethers::types::{Address, Transaction, H256, U256};
use std::collections::{HashMap, VecDeque};

const SWAP_EXACT_TOKENS_FOR_TOKENS: [u8; 4] = [0x38, 0xed, 0x17, 0x39];
const SWAP_EXACT_ETH_FOR_TOKENS: [u8; 4] = [0x7f, 0xf3, 0x6a, 0xb5];
const SWAP_EXACT_TOKENS_FOR_ETH: [u8; 4] = [0x18, 0xcb, 0xaf, 0xe5];
const SWAP_EXACT_TOKENS_FOR_TOKENS_SUPPORTING_FEE: [u8; 4] = [0x5c, 0x11, 0xd7, 0x95];
const SWAP_EXACT_ETH_FOR_TOKENS_SUPPORTING_FEE: [u8; 4] = [0xb6, 0xf9, 0xde, 0x95];
const SWAP_EXACT_TOKENS_FOR_ETH_SUPPORTING_FEE: [u8; 4] = [0x79, 0x1a, 0xc9, 0x47];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IntentClusterKey {
    pub router: Address,
    pub token_in: Address,
    pub token_out: Address,
    pub selector: [u8; 4],
}

#[derive(Debug, Clone, Copy)]
pub struct PendingSwapIntent {
    pub tx_hash: H256,
    pub actor: Address,
    pub key: IntentClusterKey,
    pub path_len: u8,
    pub value_wei: U256,
    pub effective_tip_wei: U256,
    pub observed_ms: u64,
    pub aggressiveness: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct MempoolCompetitionForecast {
    pub density: f64,
    pub similar_intents: u32,
    pub avg_aggressiveness: f64,
    pub max_tip_wei: U256,
    pub likely_outbid: bool,
    pub tip_multiplier: f64,
}

#[derive(Debug)]
pub struct MempoolIntel {
    intents: VecDeque<PendingSwapIntent>,
    clusters: HashMap<IntentClusterKey, ClusterStats>,
    capacity: usize,
}

#[derive(Debug, Clone, Copy)]
struct ClusterStats {
    count: u32,
    aggressiveness_ewma: f64,
    max_tip_wei: U256,
}

impl MempoolIntel {
    pub fn new(capacity: usize) -> Self {
        Self {
            intents: VecDeque::with_capacity(capacity),
            clusters: HashMap::with_capacity(capacity),
            capacity,
        }
    }

    pub fn observe_transaction(
        &mut self,
        tx: &Transaction,
        observed_ms: u64,
    ) -> Option<PendingSwapIntent> {
        let intent = extract_pending_intent(tx, observed_ms)?;
        self.record(intent);
        Some(intent)
    }

    pub fn forecast(
        &self,
        router: Address,
        token_in: Address,
        token_out: Address,
        selector: [u8; 4],
    ) -> MempoolCompetitionForecast {
        let key = IntentClusterKey {
            router,
            token_in,
            token_out,
            selector,
        };
        let exact = self.clusters.get(&key).copied();
        let mut similar = exact.map(|stats| stats.count).unwrap_or(0);
        let mut max_tip = exact.map(|stats| stats.max_tip_wei).unwrap_or_default();
        let mut aggression = exact
            .map(|stats| stats.aggressiveness_ewma)
            .unwrap_or_default();

        if similar == 0 {
            for (cluster, stats) in self.clusters.iter().take(8) {
                if cluster.router == router
                    && (cluster.token_in == token_in || cluster.token_out == token_out)
                {
                    similar = similar.saturating_add(stats.count.min(4));
                    if stats.max_tip_wei > max_tip {
                        max_tip = stats.max_tip_wei;
                    }
                    aggression = aggression.max(stats.aggressiveness_ewma * 0.75);
                }
            }
        }

        let density = (self.intents.len() as f64 / self.capacity.max(1) as f64).clamp(0.0, 1.0);
        let similar_factor = (similar as f64 / 10.0).clamp(0.0, 1.0);
        let likely_outbid = similar >= 3 || aggression >= 0.72;
        let tip_multiplier =
            (1.0 + density * 0.30 + similar_factor * 0.75 + aggression * 0.65).clamp(1.0, 3.0);

        MempoolCompetitionForecast {
            density,
            similar_intents: similar,
            avg_aggressiveness: aggression.clamp(0.0, 1.0),
            max_tip_wei: max_tip,
            likely_outbid,
            tip_multiplier,
        }
    }

    fn record(&mut self, intent: PendingSwapIntent) {
        if self.intents.len() == self.capacity {
            if let Some(old) = self.intents.pop_front() {
                decrement_cluster(&mut self.clusters, old.key);
            }
        }
        self.intents.push_back(intent);
        let entry = self.clusters.entry(intent.key).or_insert(ClusterStats {
            count: 0,
            aggressiveness_ewma: 0.0,
            max_tip_wei: U256::zero(),
        });
        entry.count = entry.count.saturating_add(1);
        entry.aggressiveness_ewma = if entry.count == 1 {
            intent.aggressiveness
        } else {
            entry.aggressiveness_ewma * 0.82 + intent.aggressiveness * 0.18
        };
        if intent.effective_tip_wei > entry.max_tip_wei {
            entry.max_tip_wei = intent.effective_tip_wei;
        }
    }
}

pub fn extract_pending_intent(tx: &Transaction, observed_ms: u64) -> Option<PendingSwapIntent> {
    let selector = selector(tx)?;
    if !is_swap_selector(selector) {
        return None;
    }
    let router = tx.to?;
    let path = decode_path(selector, tx.input.as_ref().get(4..)?)?;
    let token_in = *path.first()?;
    let token_out = *path.last()?;
    let effective_tip = tx
        .max_priority_fee_per_gas
        .or(tx.gas_price)
        .unwrap_or_default();
    Some(PendingSwapIntent {
        tx_hash: tx.hash,
        actor: tx.from,
        key: IntentClusterKey {
            router,
            token_in,
            token_out,
            selector,
        },
        path_len: path.len().min(u8::MAX as usize) as u8,
        value_wei: tx.value,
        effective_tip_wei: effective_tip,
        observed_ms,
        aggressiveness: aggressiveness(effective_tip, tx.value),
    })
}

fn decrement_cluster(
    clusters: &mut HashMap<IntentClusterKey, ClusterStats>,
    key: IntentClusterKey,
) {
    if let Some(stats) = clusters.get_mut(&key) {
        stats.count = stats.count.saturating_sub(1);
        if stats.count == 0 {
            clusters.remove(&key);
        }
    }
}

fn decode_path(selector: [u8; 4], args: &[u8]) -> Option<Vec<Address>> {
    let decoded = if matches!(
        selector,
        SWAP_EXACT_ETH_FOR_TOKENS | SWAP_EXACT_ETH_FOR_TOKENS_SUPPORTING_FEE
    ) {
        abi::decode(
            &[
                ParamType::Uint(256),
                ParamType::Array(Box::new(ParamType::Address)),
                ParamType::Address,
                ParamType::Uint(256),
            ],
            args,
        )
        .ok()?
    } else {
        abi::decode(
            &[
                ParamType::Uint(256),
                ParamType::Uint(256),
                ParamType::Array(Box::new(ParamType::Address)),
                ParamType::Address,
                ParamType::Uint(256),
            ],
            args,
        )
        .ok()?
    };
    let path_idx = if matches!(
        selector,
        SWAP_EXACT_ETH_FOR_TOKENS | SWAP_EXACT_ETH_FOR_TOKENS_SUPPORTING_FEE
    ) {
        1
    } else {
        2
    };
    token_as_address_vec(decoded.get(path_idx)?)
}

fn selector(tx: &Transaction) -> Option<[u8; 4]> {
    let input = tx.input.as_ref();
    (input.len() >= 4).then(|| [input[0], input[1], input[2], input[3]])
}

fn is_swap_selector(selector: [u8; 4]) -> bool {
    matches!(
        selector,
        SWAP_EXACT_TOKENS_FOR_TOKENS
            | SWAP_EXACT_ETH_FOR_TOKENS
            | SWAP_EXACT_TOKENS_FOR_ETH
            | SWAP_EXACT_TOKENS_FOR_TOKENS_SUPPORTING_FEE
            | SWAP_EXACT_ETH_FOR_TOKENS_SUPPORTING_FEE
            | SWAP_EXACT_TOKENS_FOR_ETH_SUPPORTING_FEE
    )
}

fn token_as_address(token: &Token) -> Option<Address> {
    match token {
        Token::Address(value) => Some(*value),
        _ => None,
    }
}

fn token_as_address_vec(token: &Token) -> Option<Vec<Address>> {
    match token {
        Token::Array(values) => values.iter().map(token_as_address).collect(),
        _ => None,
    }
}

fn aggressiveness(tip: U256, value: U256) -> f64 {
    let tip_gwei = tip.to_string().parse::<f64>().unwrap_or(0.0) / 1e9;
    let value_eth = value.to_string().parse::<f64>().unwrap_or(0.0) / 1e18;
    ((tip_gwei / 18.0) + (value_eth / 150.0)).clamp(0.0, 1.0)
}
