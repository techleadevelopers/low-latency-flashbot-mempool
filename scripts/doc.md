cargo run --bin predelegate_7702 -- `
  --wallets keys.txt `
  --rpc-url "https://arbitrum-mainnet.infura.io/v3/55322c8f5b9440f0940a37a3646eac76" `
  --chain-id 42161 `
  --delegate-contract 0x6b3b0de0d3015582b43C16c4bBFB70036BCe2154 `
  --sponsor-private-key 0x70b6e77fd36e9758d46630e45fdd675f5962286d45d531cfda341adf70c5b0ca


cargo run --bin preapprove_spender -- `
  --wallets keys.txt `
  --rpc-url "https://arbitrum-mainnet.infura.io/v3/55322c8f5b9440f0940a37a3646eac76" `
  --chain-id 42161 `
  --spender-contract 0x6b3b0de0d3015582b43C16c4bBFB70036BCe2154 `
  --sponsor-private-key 0x70b6e77fd36e9758d46630e45fdd675f5962286d45d531cfda341adf70c5b0ca `
  --token "USDC:0xaf88d065e77c8cC2239327C5EDb3A432268e5831:1000000000000" `
  --token "USDT:0xFd086bC7CD5C481DCC9C85ebe478A1C0b69FCbb9:1000000000000" `
  --token "WETH:0x82af49447d8a07e3bd95bd0d56f35241523fbab1:100000000000000000000"










cd C:\Users\Paulo\Desktop\flash-bot; `
cargo run --bin predelegate_7702 -- --wallets keys.txt --rpc-url "https://arbitrum-mainnet.infura.io/v3/55322c8f5b9440f0940a37a3646eac76" --chain-id 42161 --delegate-contract 0x6b3b0de0d3015582b43C16c4bBFB70036BCe2154 --sponsor-private-key 0x70b6e77fd36e9758d46630e45fdd675f5962286d45d531cfda341adf70c5b0ca; `
if ($?) { `
cargo run --bin preapprove_spender -- --wallets keys.txt --rpc-url "https://arbitrum-mainnet.infura.io/v3/55322c8f5b9440f0940a37a3646eac76" --chain-id 42161 --spender-contract 0x6b3b0de0d3015582b43C16c4bBFB70036BCe2154 --sponsor-private-key 0x70b6e77fd36e9758d46630e45fdd675f5962286d45d531cfda341adf70c5b0ca --token "USDC:0xaf88d065e77c8cC2239327C5EDb3A432268e5831:1000000000000" --token "USDT:0xFd086bC7CD5C481DCC9C85ebe478A1C0b69FCbb9:1000000000000" --token "WETH:0x82af49447d8a07e3bd95bd0d56f35241523fbab1:100000000000000000000"
}