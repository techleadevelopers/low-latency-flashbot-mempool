#![allow(dead_code)]

use clap::ValueEnum;
use ethers::prelude::*;
use ethers::utils::to_checksum;
use std::collections::HashSet;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum ExecutionMode {
    DryRun,
    Execute,
}

impl ExecutionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DryRun => "dry-run",
            Self::Execute => "execute",
        }
    }
}

pub fn parse_wei(value: &str, label: &str) -> Result<U256, Box<dyn Error>> {
    U256::from_dec_str(value.trim())
        .map_err(|e| format!("invalid {label} value '{value}': {e}").into())
}

pub fn parse_h256(value: &str, label: &str) -> Result<H256, Box<dyn Error>> {
    value
        .trim()
        .parse::<H256>()
        .map_err(|e| format!("invalid {label} hash '{value}': {e}").into())
}

pub fn parse_checksummed_address(value: &str, label: &str) -> Result<Address, Box<dyn Error>> {
    let parsed = value
        .trim()
        .parse::<Address>()
        .map_err(|e| format!("invalid {label} address '{value}': {e}"))?;
    let checksum = to_checksum(&parsed, None);
    if value.trim() != checksum {
        return Err(format!("{label} must be checksummed, expected {checksum}").into());
    }
    Ok(parsed)
}

pub fn load_address_allowlist(path: &Path) -> Result<HashSet<Address>, Box<dyn Error>> {
    let content = fs::read_to_string(path)?;
    let mut allowlist = HashSet::new();

    for (idx, raw_line) in content.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let address = parse_checksummed_address(line, &format!("allowlist line {}", idx + 1))?;
        allowlist.insert(address);
    }

    if allowlist.is_empty() {
        return Err(format!("allowlist at '{}' is empty", path.display()).into());
    }

    Ok(allowlist)
}

pub fn ensure_allowlisted(
    label: &str,
    candidate: Address,
    allowlist: &HashSet<Address>,
) -> Result<(), Box<dyn Error>> {
    if allowlist.contains(&candidate) {
        Ok(())
    } else {
        Err(format!("{label} {candidate:?} is not present in the startup allowlist").into())
    }
}

pub fn load_wallets(
    wallet_keys: &[String],
    wallets_file: Option<&PathBuf>,
    chain_id: u64,
) -> Result<Vec<LocalWallet>, Box<dyn Error>> {
    let mut raw_keys = Vec::new();
    raw_keys.extend(wallet_keys.iter().cloned());

    if let Some(path) = wallets_file {
        let content = fs::read_to_string(path)?;
        raw_keys.extend(
            content
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .map(str::to_owned),
        );
    }

    if raw_keys.is_empty() {
        return Err(
            "explicit wallet list required: pass --wallet-key and/or --wallets-file".into(),
        );
    }

    let mut seen = HashSet::new();
    let mut wallets = Vec::new();
    for raw in raw_keys {
        let normalized = normalize_private_key(&raw)?;
        if seen.insert(normalized.clone()) {
            wallets.push(normalized.parse::<LocalWallet>()?.with_chain_id(chain_id));
        }
    }

    if wallets.is_empty() {
        return Err("no valid wallets were loaded".into());
    }

    Ok(wallets)
}

pub fn normalize_private_key(value: &str) -> Result<String, Box<dyn Error>> {
    let trimmed = value.trim().strip_prefix("0x").unwrap_or(value.trim());
    if trimmed.len() != 64 || !trimmed.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err("invalid private key".into());
    }
    Ok(format!("0x{}", trimmed.to_lowercase()))
}
