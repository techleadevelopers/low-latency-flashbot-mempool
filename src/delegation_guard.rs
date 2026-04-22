// src/delegation_guard.rs
use crate::config::Config;
use crate::rpc::RpcFleet;
use ethers::prelude::*;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

pub async fn start_delegation_guard(
    rpc_fleet: Arc<RpcFleet>,
    config: Arc<Config>,
    wallet: LocalWallet,
) -> Result<(), Box<dyn std::error::Error>> {
    let wallet_address = wallet.address();
    let our_contract = config.contract;
    
    info!("🛡️ Iniciando Delegation Guard para {:?}", wallet_address);
    info!("   Contrato protegido: {:?}", our_contract);
    
    let mut last_nonce = get_nonce(&rpc_fleet, wallet_address).await?;
    let mut last_delegated = get_delegated_contract(&rpc_fleet, wallet_address).await?;
    
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        
        let current_nonce = get_nonce(&rpc_fleet, wallet_address).await?;
        let current_delegated = get_delegated_contract(&rpc_fleet, wallet_address).await?;
        
        // Verifica se o nonce mudou (alguém fez transação)
        if current_nonce != last_nonce {
            info!("⚠️ Nonce mudou: {} -> {}", last_nonce, current_nonce);
            last_nonce = current_nonce;
            
            // Verifica se a delegação foi sobrescrita
            if current_delegated != our_contract {
                warn!("🚨 DELEGAÇÃO SOBRESCRITA!");
                warn!("   Antes: {:?}", last_delegated);
                warn!("   Agora: {:?}", current_delegated);
                warn!("   Tentando reassumir com nonce {}...", current_nonce + 1);
                
                // Reaplica sua delegação
                if let Err(e) = reapply_delegation(&rpc_fleet, &config, &wallet, current_nonce + 1).await {
                    error!("❌ Falha ao reaplicar delegação: {}", e);
                } else {
                    info!("✅ Delegação reaplicada com sucesso!");
                    // Atualiza cache
                    last_delegated = our_contract;
                }
            }
        }
        
        last_delegated = current_delegated;
    }
}

async fn get_nonce(rpc_fleet: &RpcFleet, address: Address) -> Result<u64, Box<dyn std::error::Error>> {
    let endpoint = rpc_fleet.read_endpoint();
    let nonce = endpoint.provider.get_transaction_count(address, None).await?;
    Ok(nonce.as_u64())
}

async fn get_delegated_contract(rpc_fleet: &RpcFleet, address: Address) -> Result<Address, Box<dyn std::error::Error>> {
    let endpoint = rpc_fleet.read_endpoint();
    let code = endpoint.provider.get_code(address, None).await?;
    
    if code.len() >= 20 && code.as_ref().starts_with(&[0xef, 0x01, 0x00]) {
        // Extrai o endereço delegado (bytes 3-22)
        let delegated_bytes = &code.as_ref()[3..23];
        Ok(Address::from_slice(delegated_bytes))
    } else {
        Ok(Address::zero())
    }
}

async fn reapply_delegation(
    rpc_fleet: &RpcFleet,
    config: &Config,
    wallet: &LocalWallet,
    target_nonce: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    // Usa o mesmo binário predelegate_7702
    let output = std::process::Command::new("cargo")
        .args(&[
            "run", "--bin", "predelegate_7702", "--",
            "--wallets", "keys.txt",
            "--rpc-url", &config.rpc_urls()[0].1,
            "--chain-id", &config.chain_id.to_string(),
            "--delegate-contract", &format!("{:?}", config.contract),
            "--sponsor-private-key", &config.sender_private_key,
            "--target-nonce", &target_nonce.to_string(),
        ])
        .output()?;
    
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).to_string().into())
    }
}