// src/bin/guardian.rs
use ethers::prelude::*;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("🛡️ GUARDIAN - Monitor de Delegação EIP-7702");

    // ATUALIZADO: wallet alvo correta
    let wallet_address: Address = "0x9B4ae8Ec925a0c3cDc9Ac66b6336e84e7ec97e91".parse()?;
    let our_contract: Address = "0x6b3b0de0d3015582b43C16c4bBFB70036BCe2154".parse()?;
    let rpc_url = "https://arb1.arbitrum.io/rpc";
    let provider = Provider::<Http>::try_from(rpc_url)?;

    let mut last_nonce = provider
        .get_transaction_count(wallet_address, None)
        .await?
        .as_u64();

    println!("📊 Monitorando: {:?}", wallet_address);
    println!("🎯 Meu contrato: {:?}", our_contract);
    println!("⏱️  Nonce atual: {}", last_nonce);

    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;

        let current_nonce = provider
            .get_transaction_count(wallet_address, None)
            .await?
            .as_u64();
        let code = provider.get_code(wallet_address, None).await?;

        // Verifica se a delegação ainda é nossa
        let is_our_delegation =
            if code.len() >= 23 && code.as_ref().starts_with(&[0xef, 0x01, 0x00]) {
                let delegated = Address::from_slice(&code.as_ref()[3..23]);
                delegated == our_contract
            } else {
                false
            };

        if current_nonce != last_nonce {
            println!("⚠️ Nonce mudou: {} -> {}", last_nonce, current_nonce);

            if !is_our_delegation {
                println!("🚨 DELEGAÇÃO PERDIDA!");
                println!("🎯 Reaplicando com nonce {}...", current_nonce + 1);

                // Chama o predelegate_7702 com a wallet correta
                let status = std::process::Command::new("cargo")
                    .args(&[
                        "run",
                        "--bin",
                        "predelegate_7702",
                        "--",
                        "--wallets",
                        "keys.txt",
                        "--rpc-url",
                        "https://arb1.arbitrum.io/rpc",
                        "--chain-id",
                        "42161",
                        "--delegate-contract",
                        &format!("{:?}", our_contract),
                        "--sponsor-private-key",
                        "0x70b6e77fd36e9758d4....",
                        "--target-nonce",
                        &(current_nonce + 1).to_string(),
                    ])
                    .status()?;

                if status.success() {
                    println!("✅ Delegação reaplicada!");
                } else {
                    println!("❌ Falha ao reaplicar!");
                }
            }
        }

        last_nonce = current_nonce;
    }
}
