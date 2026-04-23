use ethers::types::{Address, H256, U256};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

const DEFAULT_SEGMENT_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub sequence: u64,
    pub timestamp_ms: u64,
    pub event: StateEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum StateEvent {
    ExecutionEvent(ExecutionEventRecord),
    NonceReserved(NonceReserved),
    TxSigned(TxSigned),
    TxSubmitted(TxSubmitted),
    TxIncluded(TxFinalized),
    TxDropped(TxFinalized),
    TxReplaced(TxReplaced),
    TxCancelled(TxFinalized),
    RiskDecision(RiskDecision),
    InclusionTruthUpdate(InclusionTruthUpdate),
    MarketTruthUpdate(MarketTruthUpdate),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionEventRecord {
    pub opportunity_id: String,
    pub tx_hash: H256,
    pub wallet: Address,
    pub nonce: U256,
    pub target_block: u64,
    pub gas_price_wei: U256,
    pub tip_wei: U256,
    pub relay: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NonceReserved {
    pub wallet: Address,
    pub nonce: U256,
    pub generation: u64,
    pub chain_pending_nonce: U256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxSigned {
    pub opportunity_id: String,
    pub tx_hash: H256,
    pub wallet: Address,
    pub nonce: U256,
    pub target_block: u64,
    pub gas_price_wei: U256,
    pub tip_wei: U256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxSubmitted {
    pub tx_hash: H256,
    pub wallet: Address,
    pub nonce: U256,
    pub relay: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxFinalized {
    pub tx_hash: H256,
    pub wallet: Address,
    pub nonce: U256,
    pub block_number: Option<u64>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxReplaced {
    pub old_tx_hash: H256,
    pub new_tx_hash: H256,
    pub wallet: Address,
    pub nonce: U256,
    pub new_gas_price_wei: U256,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskDecision {
    pub opportunity_id: String,
    pub accepted: bool,
    pub reason: String,
    pub risk_score: u16,
    pub competition_score: u16,
    pub confidence_score: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InclusionTruthUpdate {
    pub tx_hash: H256,
    pub outcome: String,
    pub target_block: u64,
    pub included_block: Option<u64>,
    pub relay: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketTruthUpdate {
    pub tx_hash: H256,
    pub outcome: String,
    pub edge_real_value: f64,
    pub adverse_selection_score: f64,
    pub fill_quality_score: f64,
    pub execution_toxicity_index: f64,
    pub opportunity_consumed_ratio: f64,
    pub alpha_decay_estimate: f64,
    pub late_entry_probability: f64,
    pub competitor_capture_likelihood: f64,
    pub edge_survival_probability: f64,
    pub decay_velocity: f64,
    pub execution_viability_window_ms: u64,
    pub lost_alpha: f64,
    pub inefficiency_score: f64,
    pub missed_opportunity: f64,
}

#[derive(Debug)]
pub struct EventStore {
    dir: PathBuf,
    max_segment_bytes: u64,
    sequence: AtomicU64,
    unflushed_events: AtomicUsize,
    active: Mutex<ActiveSegment>,
}

#[derive(Debug)]
struct ActiveSegment {
    index: u64,
    bytes: u64,
    writer: BufWriter<File>,
}

impl EventStore {
    pub fn open(dir: impl AsRef<Path>) -> std::io::Result<Self> {
        let segment_bytes = std::env::var("MEV_EVENT_MAX_SEGMENT_SIZE")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_SEGMENT_BYTES);
        Self::open_with_segment_bytes(dir, segment_bytes)
    }

    pub fn open_with_segment_bytes(
        dir: impl AsRef<Path>,
        max_segment_bytes: u64,
    ) -> std::io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;
        let segments = list_segments(&dir)?;
        let index = segments.last().copied().unwrap_or(0);
        let path = segment_path(&dir, index);
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let bytes = file.metadata()?.len();
        let sequence = last_sequence(&dir, &segments)?.unwrap_or(0);

        Ok(Self {
            dir,
            max_segment_bytes: max_segment_bytes.max(1024),
            sequence: AtomicU64::new(sequence),
            unflushed_events: AtomicUsize::new(0),
            active: Mutex::new(ActiveSegment {
                index,
                bytes,
                writer: BufWriter::new(file),
            }),
        })
    }

    pub fn append(&self, event: StateEvent) -> std::io::Result<EventEnvelope> {
        let envelope = EventEnvelope {
            sequence: self.sequence.fetch_add(1, Ordering::SeqCst) + 1,
            timestamp_ms: unix_ms(),
            event,
        };
        let mut line = serde_json::to_vec(&envelope)?;
        line.push(b'\n');

        let mut active = self.active.lock().expect("event store lock");
        self.rotate_locked(&mut active, line.len() as u64, self.max_segment_bytes)?;
        active.writer.write_all(&line)?;
        active.bytes = active.bytes.saturating_add(line.len() as u64);
        self.unflushed_events.fetch_add(1, Ordering::Release);
        Ok(envelope)
    }

    pub fn replay_after(&self, sequence: u64) -> std::io::Result<Vec<EventEnvelope>> {
        replay_after_dir(&self.dir, sequence)
    }

    pub fn flush(&self) -> std::io::Result<()> {
        self.active
            .lock()
            .expect("event store lock")
            .writer
            .flush()?;
        self.unflushed_events.store(0, Ordering::Release);
        Ok(())
    }

    pub fn flush_if_needed(&self, event_threshold: usize) -> std::io::Result<bool> {
        if self.unflushed_events.load(Ordering::Acquire) < event_threshold.max(1) {
            return Ok(false);
        }
        self.flush()?;
        Ok(true)
    }

    pub fn rotate_if_needed(&self, max_segment_size: u64) -> std::io::Result<bool> {
        let mut active = self.active.lock().expect("event store lock");
        if active.bytes <= max_segment_size.max(1024) {
            return Ok(false);
        }
        self.rotate_locked(&mut active, 0, max_segment_size.max(1024))?;
        Ok(true)
    }

    pub fn current_sequence(&self) -> u64 {
        self.sequence.load(Ordering::SeqCst)
    }

    fn rotate_locked(
        &self,
        active: &mut ActiveSegment,
        incoming_bytes: u64,
        max_segment_bytes: u64,
    ) -> std::io::Result<()> {
        if active.bytes.saturating_add(incoming_bytes) <= max_segment_bytes.max(1024) {
            return Ok(());
        }
        active.writer.flush()?;
        self.unflushed_events.store(0, Ordering::Release);
        active.index = active.index.saturating_add(1);
        active.bytes = 0;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(segment_path(&self.dir, active.index))?;
        active.writer = BufWriter::new(file);
        Ok(())
    }
}

pub fn replay_after_dir(
    dir: impl AsRef<Path>,
    sequence: u64,
) -> std::io::Result<Vec<EventEnvelope>> {
    let dir = dir.as_ref();
    let mut out = Vec::new();
    for segment in list_segments(dir)? {
        let file = File::open(segment_path(dir, segment))?;
        for line in BufReader::new(file).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let envelope: EventEnvelope = serde_json::from_str(&line)?;
            if envelope.sequence > sequence {
                out.push(envelope);
            }
        }
    }
    out.sort_by_key(|event| (event.timestamp_ms, event.sequence));
    Ok(out)
}

fn last_sequence(dir: &Path, segments: &[u64]) -> std::io::Result<Option<u64>> {
    let mut last = None;
    for segment in segments {
        let file = File::open(segment_path(dir, *segment))?;
        for line in BufReader::new(file).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let envelope: EventEnvelope = serde_json::from_str(&line)?;
            last = Some(envelope.sequence);
        }
    }
    Ok(last)
}

fn list_segments(dir: &Path) -> std::io::Result<Vec<u64>> {
    let mut segments = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some(number) = name
            .strip_prefix("events-")
            .and_then(|value| value.strip_suffix(".jsonl"))
        else {
            continue;
        };
        if let Ok(index) = number.parse::<u64>() {
            segments.push(index);
        }
    }
    segments.sort_unstable();
    Ok(segments)
}

fn segment_path(dir: &Path, index: u64) -> PathBuf {
    dir.join(format!("events-{index:020}.jsonl"))
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}
