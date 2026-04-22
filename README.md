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

## MEV engine low-capital

O projeto agora tambem tem uma trilha separada de MEV low-capital em `src/mev/`.
Ela reaproveita:

- runtime Rust/Tokio
- `RpcFleet` com roteamento por latencia e failover
- modos `shadow`, `paper` e `live`
- dashboard/eventos/telemetria
- provider WebSocket do mempool
- Flashbots relay como caminho privado

Estrutura:

- `src/mev/backrun.rs`: estrategia inicial, ultra-filtrada, para detectar swaps grandes e candidatos de backrun
- `src/mev/opportunity.rs`: score estrito de lucro, custo, ROI, risco, competicao e confianca
- `src/mev/capital.rs`: capital atual, PnL diario, drawdown, stop-loss, cooldown e limite de gas por janela
- `src/mev/execution.rs`: executor para `shadow`, `paper` e `live`

Ativacao conservadora:

```env
MEV_ENGINE_ENABLED=true
MEV_STRATEGY=backrun
BOT_MODE=shadow
ALLOW_SEND=false
MEV_ALLOW_PUBLIC_MEMPOOL=false
```

Exemplo completo: `.env.mev-low-capital.example`.

### Gates de oportunidade

Uma oportunidade so passa se todos os filtros forem verdadeiros:

- lucro liquido ajustado por slippage >= `MEV_MIN_NET_PROFIT_ETH`
- ROI >= `MEV_MIN_ROI_BPS`
- risco <= `MEV_MAX_RISK_SCORE`
- competicao <= `MEV_MAX_COMPETITION_SCORE`
- confianca >= `MEV_MIN_CONFIDENCE_SCORE`
- idade da pending tx <= `MEV_MAX_PENDING_AGE_MS`
- capital manager aprova alocacao, gas, cooldown e stop-loss

### Comportamento live

O modo `live` e intencionalmente restritivo. Ele nao envia transacao publica por padrao e bloqueia qualquer oportunidade sem `execution_payload` assinado. Isso evita que uma heuristica incompleta queime gas real. Para producao, o proximo passo e acoplar um construtor de payload de backrun com simulacao de estado pos-swap e validacao via relay/sim antes de anexar o payload ao `MevOpportunity`.

### Onde esse sistema perde dinheiro

- A heuristica atual de edge e conservadora, mas nao substitui simulacao real de pools. Sem simulacao, o sistema pode superestimar residual arbitrage.
- Competidores com orderflow privado, builders melhores e colocacao mais rapida vencem backruns obvios.
- Swaps grandes demais atraem competicao. O score penaliza isso, mas nao mede o mempool inteiro nem o builder market em tempo real.
- Tokens sem preco configurado em `MONITORED_TOKENS_*` sao ignorados para evitar falso notional.
- Gas repricing entre deteccao e envio pode transformar lucro pequeno em perda.
- `paper` nao prova fill real; ele so exercita gates, capital e telemetria.

### Como reduzir risco antes de live

- Rodar `shadow` por varios dias e comparar oportunidades detectadas contra estado de pools historico.
- Implementar simulacao pos-swap por pool/router especifico antes de gerar payload.
- Exigir bundle simulation positiva antes de `send_bundle`.
- Comecar com `MEV_MAX_DAILY_LOSS_ETH` e `MEV_MAX_GAS_SPEND_WINDOW_ETH` baixos.
- Manter `MEV_ALLOW_PUBLIC_MEMPOOL=false`.
- Subir `MEV_MIN_CONFIDENCE_SCORE` e baixar `MEV_MAX_COMPETITION_SCORE` quando houver muito ruido.

## Upgrade deterministico de execucao

A trilha MEV agora contem os modulos exigidos para separar sinal de execucao:

- `src/mev/amm/uniswap_v2.rs`: math inteiro de Uniswap V2 com `x*y=k`, fee de 0,3%, atualizacao de reservas e impacto de preco.
- `src/mev/amm/uniswap_v3.rs`: simulador de `sqrtPriceX96`, liquidez ativa e ticks inicializados para swaps exatos dentro/atraves de ranges.
- `src/mev/simulation/state_simulator.rs`: aplica a transacao da vitima sobre o estado AMM e retorna estado pos-swap, preco efetivo e slippage.
- `src/mev/execution/payload_builder.rs`: calcula tamanho dinamico por ROI, gera calldata real de router com `amountOutMin` e rejeita lucro/impacto/liquidez ruins.
- `src/mev/simulation/bundle_simulator.rs`: preflight deterministico de bundle antes de envio. O live path recusa qualquer payload sem simulacao positiva.
- `src/mev/pnl/tracker.rs`: estrutura `ExecutionResult` e reconciliacao por receipt: gas pago, tokens recebidos/gastos e lucro realizado.
- `src/mev/analytics/missed_opportunities.rs`: `MissReason` para medir perdas por lucro baixo, competicao, simulacao, capital, latencia e payload/pool indisponivel.
- `contracts/MevExecutor.sol`: executor atomico com flashswap Uniswap V2, swaps multi-step, safe approvals, non-reentrancy e enforcement on-chain de lucro.
- `src/mev/execution/contract_encoder.rs`: ABI encoder para chamadas ao `MevExecutor`.
- `src/mev/execution/flashloan_builder.rs`: builder de chamada `startV2FlashSwap(...)`.
- `src/mev/execution/bundle_sender.rs`: helper de bundle privado com payload assinado.

Politica estrita atual:

- `live` nao envia sem `ExecutionPayload`.
- `live` nao envia payload sem bytes assinados.
- `live` nao envia sem `BundleSimulator::deterministic_preflight(...)` positivo.
- fallback publico continua bloqueado salvo `MEV_ALLOW_PUBLIC_MEMPOOL=true`.
- o contrato reverte se `finalBalance <= initialBalance + minProfit` com `NO_PROFIT`.

Configuracao low-capital adicional:

```env
MEV_MIN_PROFIT_USD=2.0
MEV_MAX_GAS_PER_TX=260000
MEV_MAX_DAILY_LOSS_ETH=0.01
MEV_MIN_CONFIDENCE_SCORE=80
MEV_MAX_PRICE_IMPACT_BPS=250
MEV_SLIPPAGE_PROTECTION_BPS=50
MEV_EXECUTOR_ADDRESS=0xSEU_MEV_EXECUTOR_DEPLOYADO
MEV_UNISWAP_V2_FACTORY=0x5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f
```

O fluxo atomico V2 agora e:

1. detectar swap grande
2. ler pair/reserves
3. simular estado pos-victim
4. calcular tamanho dinamico por ROI
5. codificar chamada `MevExecutor.startV2FlashSwap(...)`
6. assinar tx para `MEV_EXECUTOR_ADDRESS`
7. executar preflight de bundle
8. enviar bundle `[victim_tx, mev_executor_tx]`

O contrato garante lucro on-chain:

```solidity
require(finalBalance > initialBalance + params.minProfit, "NO_PROFIT");
```

Teste Foundry minimo foi adicionado em `test/MevExecutor.t.sol`. Neste ambiente `forge` nao estava instalado, entao o teste Solidity nao foi executado localmente. O build Rust foi validado com `cargo check`.

### Analise critica restante

Onde ainda perde dinheiro:

- Se o pool mudar entre mempool detectado e builder inclusion, a simulacao local fica stale.
- V3 em producao exige leitura completa e atualizada dos ticks relevantes; tick cache incompleto cria falso lucro.
- Sem contrato executor/flashloan, muitos backruns nao sao financiaveis com baixo capital.
- Bundle preflight local nao substitui relay/builder simulation assinada.

Gargalos de latencia:

- `eth_getTransactionByHash` em pending tx.
- leitura de factory/pair/reserves por oportunidade.
- tick loading V3 quando a rota cruza muitos ranges.
- relay simulation antes do envio.

Por que competidores ganham:

- orderflow privado e hints de builders.
- co-locacao e RPC dedicado.
- cache local de pools/ticks em memoria.
- searchers com contratos executores ja auditados e capital/flashloan routes melhores.

Como reduzir risco:

- manter cache local de reserves/ticks atualizado por logs.
- usar relay simulation real alem do preflight local.
- adicionar executor contract com callbacks de flashloan e asserts on-chain de lucro minimo.
- exigir margem maior que gas variance e builder tip.
- registrar todos os `MissReason` e ajustar thresholds por dados, nao por intuicao.
