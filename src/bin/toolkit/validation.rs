#![allow(dead_code)]

use ethers::prelude::*;
use ethers::utils::keccak256;
use std::error::Error;

pub async fn ensure_contract_code(
    provider: &Provider<Http>,
    address: Address,
    label: &str,
) -> Result<Bytes, Box<dyn Error>> {
    let code = provider.get_code(address, None).await?;
    if code.as_ref().is_empty() {
        return Err(
            format!("{label} bytecode check failed: no contract code at {address:?}").into(),
        );
    }
    Ok(code)
}

pub fn extract_eip7702_delegate(code: &Bytes) -> Option<Address> {
    let raw = code.as_ref();
    if raw.len() >= 23 && raw.starts_with(&[0xef, 0x01, 0x00]) {
        Some(Address::from_slice(&raw[3..23]))
    } else {
        None
    }
}

pub fn code_hash(code: &Bytes) -> H256 {
    H256::from(keccak256(code.as_ref()))
}
