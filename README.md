# Corporate Residual Sweeper

Sistema de custodia reativa para wallets operacionais da propria empresa, com foco em micro-residuo economicamente positivo.

## Modelo operacional

O sistema nao procura apenas saldo grande. Ele monitora:

- taxa
- troco
- sobra
- residuo
- farelo do giro

Regra central:

```text
valor_residual_total = nativo + tokens_em_eth
custo_total = gas_estimado * gas_price
valor_liquido = valor_residual_total - custo_total
executa se valor_liquido > margem_minima
```

Condicoes adicionais:

- ha saldo nativo suficiente para executar
- `MIN_BALANCE` e apenas reserva minima operacional
- o destino da tesouraria precisa bater com o `forwarder`
- a politica do ativo precisa permitir

## Modo Spender

O contrato tambem suporta fluxo de spender para ERC-20:

- a wallet alvo aprova o contrato
- a wallet operacional faz sponsor funding do gas
- a wallet operacional chama `sweepAllFrom(wallet_alvo, tokens)`
- o contrato puxa os tokens com `transferFrom`

Relacao correta entre chaves e contrato:

- `SENDER_PRIVATE_KEY`: EOA que deve ser `owner()` do contrato e que assina a tx final de `sweepAllFrom(...)`
- `CONTROL_ADDRESS`: destino final dos fundos
- `USE_EXTERNAL_GAS_SPONSOR=true`: habilita sponsor funding no bundle

Ordem do bundle nesse modo:

1. sponsor funding
2. approve da wallet alvo
3. `sweepAllFrom(wallet_alvo, tokens)`

Limitacao:

- esse fluxo cobre ERC-20 aprovavel
- nativo da wallet alvo nao entra por `transferFrom`
- para nativo, precisa de outro caminho como delegate/7702

## Pre-delegacao 7702

Para wallets do proprio ecossistema, da para provisionar a delegacao antes e deixar o monitor principal so com o papel de detectar saldo e disparar o sweep.

Existe um binario dedicado:

```bash
cargo run --bin predelegate_7702 -- \
  --wallets keys.txt \
  --rpc-url https://arbitrum-mainnet.infura.io/v3/SEU_RPC \
  --chain-id 42161 \
  --delegate-contract 0xSEU_CONTRATO_DEPLOYADO \
  --sponsor-private-key 0xSUA_SENDER_PRIVATE_KEY
```

Esse fluxo:

1. le as wallets-alvo do arquivo
2. usa cada chave da wallet-alvo para assinar a autorizacao 7702
3. usa a sponsor key para enviar a tx `0x04` de instalacao
4. imprime o tx hash e valida se a wallet deixou de ter code vazio

Isso nao altera o monitor principal nem o fluxo on-demand; serve como provisionamento previo.

## Pre-approve spender

Para o fallback `spender`, da para provisionar approvals antes do monitor principal.

Binario dedicado:

```bash
cargo run --bin preapprove_spender -- \
  --wallets keys.txt \
  --rpc-url https://arbitrum-mainnet.infura.io/v3/SEU_RPC \
  --chain-id 42161 \
  --spender-contract 0xSEU_CONTRATO_DEPLOYADO \
  --token USDC:0xaf88d065e77c8cC2239327C5EDb3A432268e5831:max \
  --token USDT:0xfd086bc7cd5c481dcc9c85ebe478a1c0b69fcbb9:max \
  --token WETH:0x82af49447d8a07e3bd95bd0d56f35241523fbab1:max
```

Formato de cada token:

- `SYMBOL:ADDRESS:AMOUNT`
- `AMOUNT` pode ser valor bruto decimal ou `max`

Uso recomendado:

- `7702` como principal
- `spender` so para whitelist explicita de tokens
- evitar approvals abertos para qualquer ativo

## Politica por ativo

O projeto diferencia tres classes:

- `native`
- `stable`
- `other-token`

Cada classe pode ter thresholds proprios:

- `NATIVE_MIN_NET_PROFIT_ETH`
- `NATIVE_MIN_ROI_BPS`
- `ENABLE_NATIVE_SWEEP`
- `STABLE_MIN_NET_PROFIT_ETH`
- `STABLE_MIN_ROI_BPS`
- `ENABLE_STABLE_SWEEP`
- `OTHER_TOKEN_MIN_NET_PROFIT_ETH`
- `OTHER_TOKEN_MIN_ROI_BPS`
- `ENABLE_OTHER_TOKEN_SWEEP`

Tokens monitorados aceitam:

- `address:decimals:price_eth`
- `symbol:address:decimals:price_eth`
- `symbol:class:address:decimals:price_eth`

Exemplo:

```env
MONITORED_TOKENS_ETHEREUM=USDC:stable:0xA0b86991c6218b36c1d19d4a2e9eb0ce3606eb48:6:0.00032,WETH:other:0xC02aaA39b223FE8D0A0E5C4F27eAD9083C756Cc2:18:1.0
```

## Fila e prioridade

A fila prioriza:

1. maior valor liquido residual
2. maior ROI
3. maior antiguidade

Nao e FIFO puro.

## Dashboard

O dashboard agora mostra:

- candidatos residuais ativos
- recorrencia por wallet
- lucro detectado acumulado
- lucro realizado acumulado
- score de residuo recorrente
- status do fallback publico

## Hardening operacional

- `DISABLE_PUBLIC_FALLBACK=true` deve ser o padrao em producao
- `BOT_MODE=shadow` e o ponto de partida
- `paper` deve operar com allowlist e teto baixo
- `live` exige destino validado, latencia medida e politicas calibradas

## Calibracao inicial

Partir de algo conservador:

```env
MIN_NET_PROFIT_ETH=0.001
MIN_ROI_BPS=500
MIN_SCAN_INTERVAL_MS=250
MAX_SCAN_INTERVAL_MS=1500
WALLET_COOLDOWN_SECS=20
QUEUE_DEDUPE_SECS=10
DISABLE_PUBLIC_FALLBACK=true
```

Depois ajustar conforme ruido real, custo medio de gas e tamanho dos residuos.

## Teste de velocidade

Existe um benchmark manual da fila em `src/queue.rs`.

Rodar com carga padrao:

```bash
cargo test benchmark_queue_throughput_for_large_bursts -- --ignored --nocapture
```

Ajustar carga e limite esperado:

```bash
QUEUE_BENCH_ROUNDS=5000 QUEUE_BENCH_WALLETS=400 QUEUE_BENCH_MAX_MS=8000 cargo test benchmark_queue_throughput_for_large_bursts -- --ignored --nocapture
```

O teste imprime:

- `total_jobs`
- `elapsed_ms`
- `jobs_per_sec`

Se `elapsed_ms` passar do limite configurado em `QUEUE_BENCH_MAX_MS`, o teste falha.

## Benchmark de rede

Para medir os gargalos reais de RPC e relay sem entrar no loop do monitor:

```bash
RUN_NETWORK_BENCHMARK=true cargo run -- --network ethereum
```

O benchmark mede por endpoint:

- `get_block_number`
- `get_gas_price`
- `get_balances_batch`
- `get_transaction_count`

Preferencia de rota em producao:

- `RPC_READ_PREFERENCE=auto|alchemy|infura`
- `RPC_SEND_PREFERENCE=auto|alchemy|infura`

Saida:

- `avg`
- `p50`
- `p95`
- contagem de erros

Ajustes:

```bash
RUN_NETWORK_BENCHMARK=true NETWORK_BENCHMARK_SAMPLES=20 NETWORK_BENCHMARK_WALLETS=40 cargo run -- --network ethereum
```

Probe opcional do relay Flashbots:

```bash
RUN_NETWORK_BENCHMARK=true NETWORK_BENCHMARK_BUNDLE=true NETWORK_BENCHMARK_BUNDLE_SAMPLES=3 cargo run -- --network ethereum
```

Esse probe de `send_bundle` fica desligado por padrao porque envia um bundle real ao relay para medir round-trip.

## Monitor de mempool

Existe um monitor opcional de pending transactions via WebSocket para acionar a trilha de front-run.

Flags:

```env
ENABLE_MEMPOOL_MONITOR=true
MEMPOOL_WS_URL=wss://eth-mainnet.g.alchemy.com/v2/SEU_ALCHEMY_KEY
FRONTRUN_SLIPPAGE_BPS=100
FRONTRUN_GAS_BUMP_BPS=11000
```

Comportamento atual:

- assina monitoramento de `pending_txs`
- filtra por selectors de swap suportados
- decodifica swaps `swapExactTokensForTokens`, `swapExactETHForTokens` e `swapExactTokensForETH`
- envia bundle Flashbots real quando encontra oportunidade suportada

Limitacao atual:

- o envio efetivo atual fica restrito a `swapExactETHForTokens`
- swaps com input ERC-20 ainda exigem saldo e approvals dedicados do bot
- nao existe heuristica de lucro on-chain acoplada nessa trilha generica
