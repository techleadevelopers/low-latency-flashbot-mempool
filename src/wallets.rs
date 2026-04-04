use crate::config::WalletEntry;
use ethers::prelude::*;
use std::collections::HashSet;
use std::fs;
use std::path::Path;

#[derive(Debug)]
pub struct LoadedWallets {
    pub wallets: Vec<LocalWallet>,
    pub total_read: usize,
    pub unique: usize,
    pub duplicates: usize,
    pub invalid: usize,
}

pub fn load_wallets(
    path: &Path,
    chain_id: u64,
) -> Result<LoadedWallets, Box<dyn std::error::Error>> {
    match path.extension().and_then(|value| value.to_str()) {
        Some("json") => load_from_json(path, chain_id),
        _ => load_from_keys_txt(path, chain_id),
    }
}

fn load_from_keys_txt(
    path: &Path,
    chain_id: u64,
) -> Result<LoadedWallets, Box<dyn std::error::Error>> {
    let content = fs::read_to_string(path)?;
    let mut seen = HashSet::new();
    let mut wallets = Vec::new();
    let mut total_read = 0usize;
    let mut duplicates = 0usize;
    let mut invalid = 0usize;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        total_read += 1;
        let normalized = match normalize_private_key(trimmed) {
            Some(value) => value,
            None => {
                invalid += 1;
                continue;
            }
        };

        if !seen.insert(normalized.clone()) {
            duplicates += 1;
            continue;
        }

        match normalized.parse::<LocalWallet>() {
            Ok(wallet) => wallets.push(wallet.with_chain_id(chain_id)),
            Err(_) => invalid += 1,
        }
    }

    let unique = wallets.len();
    Ok(LoadedWallets {
        wallets,
        total_read,
        unique,
        duplicates,
        invalid,
    })
}

fn load_from_json(path: &Path, chain_id: u64) -> Result<LoadedWallets, Box<dyn std::error::Error>> {
    let content = fs::read_to_string(path)?;
    let entries: Vec<WalletEntry> = serde_json::from_str(&content)?;
    let mut seen = HashSet::new();
    let mut wallets = Vec::new();
    let mut duplicates = 0usize;
    let mut invalid = 0usize;

    for entry in entries {
        let normalized = match normalize_private_key(&entry.private_key) {
            Some(value) => value,
            None => {
                invalid += 1;
                continue;
            }
        };

        if !seen.insert(normalized.clone()) {
            duplicates += 1;
            continue;
        }

        match normalized.parse::<LocalWallet>() {
            Ok(wallet) => wallets.push(wallet.with_chain_id(chain_id)),
            Err(_) => invalid += 1,
        }
    }

    let unique = wallets.len();
    Ok(LoadedWallets {
        wallets,
        total_read: unique + duplicates + invalid,
        unique,
        duplicates,
        invalid,
    })
}

fn normalize_private_key(value: &str) -> Option<String> {
    let trimmed = value.trim().strip_prefix("0x").unwrap_or(value.trim());
    if trimmed.len() != 64 || !trimmed.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("0x{}", trimmed.to_lowercase()))
}
