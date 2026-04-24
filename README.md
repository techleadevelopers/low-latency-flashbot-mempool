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

## Toolkit de custodia em `src/bin/`

Os binarios de `src/bin/` agora formam um toolkit de custodia deterministico. Eles nao sao estrategias MEV e nao executam loops autonomos.

Binarios atuais:

- `sweeper`: executor one-shot para varrer saldo nativo ou ERC-20 para um endereco de controle validado
- `preapprove`: provisionamento one-shot de approvals ERC-20 para spender explicitamente allowlisted
- `predelegate`: provisionamento one-shot de delegacao EIP-7702 para contrato explicitamente allowlisted

Regras operacionais do toolkit:

- `--mode dry-run` e o padrao em todos os binarios
- `--mode execute` e obrigatorio para enviar transacao real
- nao ha loop infinito
- nao ha scanner em background
- nao ha mempool listener dentro desses binarios
- nao ha retries implicitos
- cada execucao exige lista explicita de wallets via `--wallet-key` e/ou `--wallets-file`

Guardrails compartilhados:

- validacao de endereco checksummed no startup
- allowlist obrigatoria para `control_address`, `spender_address`, `delegate_contract` e tokens quando aplicavel
- `MAX_VALUE_PER_TX`
- gas sanity check
- `max_executions_per_run`
- `cooldown_per_wallet_seconds` opcional com arquivo de estado local
- logs estruturados em JSON com `wallet`, `token`, `value_detected_wei`, `estimated_gas`, `gas_cost_wei`, `net_profit_wei`, `roi_bps`, `action`, `reason` e `tx_hash` quando houver

### `sweeper`

O `sweeper` e um executor one-shot. Ele nao monitora nada por conta propria.

Politica de execucao:

```text
net_profit = value - gas_cost
executa somente se:
  net_profit > MIN_NET_PROFIT
  roi_bps > MIN_ROI_BPS
  gas estimate for valido
  destination bater com control_address allowlisted
  value <= MAX_VALUE_PER_TX
  --mode execute estiver presente
```

Exemplo de dry-run para saldo nativo:

```bash
cargo run --bin sweeper -- \
  --rpc-url https://arb1.arbitrum.io/rpc \
  --chain-id 42161 \
  --wallets-file keys.txt \
  --control-address 0x1234...ABCD \
  --control-allowlist control_allowlist.txt \
  --token native
```

Exemplo de execucao para ERC-20:

```bash
cargo run --bin sweeper -- \
  --mode execute \
  --rpc-url https://arb1.arbitrum.io/rpc \
  --chain-id 42161 \
  --wallets-file keys.txt \
  --control-address 0x1234...ABCD \
  --control-allowlist control_allowlist.txt \
  --token 0xA0b86991c6218b36c1d19d4a2e9eb0ce3606eb48 \
  --token-allowlist token_allowlist.txt \
  --quoted-value-wei 25000000000000000 \
  --max-value-per-tx-wei 50000000000000000 \
  --min-net-profit-wei 2000000000000000 \
  --min-roi-bps 800
```

Observacoes:

- para `native`, o valor detectado vem do saldo nativo disponivel menos gas
- para `ERC-20`, o binario exige `--quoted-value-wei` para aplicar guardrails economicos sem depender do core MEV
- se o valor economico nao justificar a execucao, a acao e `SKIP`

### `preapprove`

O `preapprove` provisiona approvals ERC-20 de forma one-shot e segura por padrao.

Regras:

- spender precisa ser checksummed e allowlisted
- token precisa ser checksummed e allowlisted
- approval ilimitado so e aceito com `--allow-unlimited-approval`
- sem `--mode execute`, apenas registra `SKIP` de dry-run
- nao faz sponsor funding implicito

Formato de token:

- `SYMBOL:CHECKSUMMED_TOKEN_ADDRESS:AMOUNT_WEI`
- `SYMBOL:CHECKSUMMED_TOKEN_ADDRESS:max`

Exemplo:

```bash
cargo run --bin preapprove -- \
  --mode execute \
  --rpc-url https://arb1.arbitrum.io/rpc \
  --chain-id 42161 \
  --wallets-file keys.txt \
  --spender-address 0x1234...ABCD \
  --spender-allowlist spender_allowlist.txt \
  --token-allowlist token_allowlist.txt \
  --token USDC:0xaf88d065e77c8cC2239327C5EDb3A432268e5831:1000000
```

Para approval ilimitado:

```bash
cargo run --bin preapprove -- \
  --mode execute \
  --rpc-url https://arb1.arbitrum.io/rpc \
  --chain-id 42161 \
  --wallets-file keys.txt \
  --spender-address 0x1234...ABCD \
  --spender-allowlist spender_allowlist.txt \
  --token-allowlist token_allowlist.txt \
  --allow-unlimited-approval \
  --token USDC:0xaf88d065e77c8cC2239327C5EDb3A432268e5831:max
```

### `predelegate`

O `predelegate` instala delegacao EIP-7702 em modo one-shot.

Regras:

- contrato delegado precisa ter bytecode
- contrato delegado precisa ser checksummed e allowlisted
- `--expected-code-hash` pode ser usado para travar a versao esperada do contrato
- sponsor paga o gas, mas o binario nao faz funding implicito das wallets alvo
- apos execucao, o binario verifica se a delegacao instalada bate com o contrato esperado

Exemplo:

```bash
cargo run --bin predelegate -- \
  --mode execute \
  --rpc-url https://arb1.arbitrum.io/rpc \
  --chain-id 42161 \
  --wallets-file keys.txt \
  --delegate-contract 0x1234...ABCD \
  --delegate-allowlist delegate_allowlist.txt \
  --sponsor-private-key 0xSUA_CHAVE \
  --expected-code-hash 0xHASH_DO_BYTECODE
```

Uso recomendado:

- `predelegate` como caminho principal para wallets sob controle da propria operacao
- `preapprove` apenas para tokens explicitamente allowlisted
- `sweeper` como executor manual ou disparado por scheduler externo
- nunca tratar esses binarios como bots autonomos

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
- `src/mev/inclusion.rs`: estrategia adaptativa de inclusao com tip dinamico, probabilidade de inclusao, ranking de relays e feedback de falhas.

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
MEV_BUILDER_RELAYS=https://relay.flashbots.net
MEV_INCLUSION_MIN_EV_USD=1.0
MEV_INCLUSION_BASE_TIP_BPS=800
MEV_INCLUSION_MAX_TIP_BPS=3500
```

O fluxo atomico V2 agora e:

1. detectar swap grande
2. ler pair/reserves
3. simular estado pos-victim
4. calcular tamanho dinamico por ROI
5. codificar chamada `MevExecutor.startV2FlashSwap(...)`
6. assinar tx para `MEV_EXECUTOR_ADDRESS`
7. executar preflight de bundle
8. passar pelo inclusion gate: EV ajustado por probabilidade de inclusao precisa superar o minimo
9. enviar bundle `[victim_tx, mev_executor_tx]`

Inclusao adaptativa:

- competicao alta aumenta tip
- EV baixo corta tip de forma agressiva
- relays sao priorizados por taxa de sucesso e latencia
- bundle e bloqueado se `adjusted_ev = (profit - gas - tip) * inclusion_probability` ficar abaixo de `MEV_INCLUSION_MIN_EV_USD`
- retries sao limitados por `MEV_INCLUSION_MAX_ATTEMPTS`

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

## Arquitetura MEV senior: estado atual, garantias e limites

Esta secao documenta a camada MEV atual do projeto como um sistema de execucao financeira adversarial. O objetivo nao e "mandar mais transacoes"; e preservar capital, executar apenas quando ha evidencia suficiente e reconstruir o estado de forma deterministica apos falha.

Principio operacional:

```text
survival > correctness local > inclusion > profit teorico > volume
```

Em outras palavras:

- detectar oportunidade nao significa executar
- simular oportunidade nao significa lucrar
- incluir bundle nao significa sucesso economico
- sucesso real e lucro realizado/markout favoravel depois de gas, slippage, competicao e latencia

### Visao em camadas

A trilha MEV em `src/mev/` esta organizada em camadas independentes:

- `amm/`: matematica deterministica de AMMs.
- `simulation/`: simulacao de estado pos-victim e preflight de bundle.
- `execution/`: payload, assinatura, nonce, lifecycle, replacement policy e finalizacao.
- `competition/`: sinais de competicao por bloco, mempool pending e pressure map.
- `inclusion/`: estrategia adaptativa de tip, relay e probabilidade de inclusao.
- `inclusion_truth/`: verdade operacional de receipt/inclusao.
- `state/`: event sourcing, snapshots, recovery, daemon de durabilidade e drift checking.
- `market_truth/`: verdade economica pos-trade, markout, toxicidade e replay.
- `feedback/`, `tip_discovery/`, `post_block/`: aprendizado leve baseado em outcomes.

Cada camada tem uma responsabilidade. A camada de truth economico nao decide trades. A camada de recovery nao altera risco. A camada de competicao nao deve enviar transacao. Essa separacao existe para evitar que um ajuste de learning mude o comportamento de execucao sem controle.

### Fluxo MEV end-to-end

Fluxo atual de uma oportunidade backrun:

1. `backrun.rs` assina pending txs via WebSocket.
2. Decodifica swap grande e ignora ruido pequeno.
3. Calcula score conservador inicial.
4. Passa pelo meta-decision gate.
5. Se possivel, constroi payload V2 deterministico.
6. `ExecutionEngine` aplica gates live:
   - `ALLOW_SEND`
   - `survival_mode`
   - private-only/public mempool policy
   - payload obrigatorio
   - competition forecast
   - inclusion EV gate
   - bundle preflight
7. Reserva nonce local.
8. Assina transacao.
9. Envia bundle privado.
10. Registra lifecycle + event store.
11. `block_loop.rs` reconcilia receipt.
12. `InclusionTruthEngine` classifica inclusao operacional.
13. `ExecutionFinalizer` libera nonce/lifecycle.
14. `Market Truth Layer` classifica outcome economico.
15. Feedback/tip/competition/inclusion aprendem com resultado real.

O sistema foi desenhado para rejeitar agressivamente. Um bot pequeno nao ganha por volume; ganha por evitar transacoes ruins.

## Camada de competicao e inteligencia adversarial

O sistema tem duas visoes de competicao: reativa e preditiva.

### Sinais por bloco

Modulo:

- `src/mev/competition/signal_extractor.rs`

Responsabilidades:

- extrair swaps concorrentes em blocos confirmados
- detectar interacoes AMM similares
- estimar agressividade por ator com base em tip/value observavel
- alimentar `CompetingTxSignal` para classificacao de `OUTBID`

Esse modulo nao assume intencao privada. Ele usa apenas dados observaveis no bloco.

### Pressao competitiva

Modulo:

- `src/mev/competition/pressure_map.rs`

Mantem:

- heatmap rolling por pool/router alvo
- congestionamento de mempool
- estimativa marginal de tip
- pressao historica
- pressao forward-looking

Essa pressao afeta:

- tip scaling
- frequencia de execucao
- seletividade minima

Mas ela nao substitui simulacao nem risk/capital gates.

### Inteligencia pre-block

Modulo:

- `src/mev/competition/mempool_intel.rs`

Responsabilidades:

- observar pending transactions recebidas pelo proprio WebSocket
- agrupar por `router + token_in + token_out + selector`
- estimar densidade pre-block
- estimar intents similares
- calcular `likely_outbid`
- produzir `CompetitionForecast`

Limitacao importante:

- isso ve apenas o mempool que o seu provedor mostra
- fluxo privado direto builder/relay nao aparece
- portanto, `likely_outbid=false` nunca deve ser lido como "sem competicao"

## Inclusion strategy e builder feedback

Modulo:

- `src/mev/inclusion.rs`

O inclusion engine calcula:

```text
adjusted_ev = (expected_profit - gas - tip) * inclusion_probability
```

Ele rejeita se:

- EV ajustado fica abaixo do minimo
- probabilidade de inclusao fica ruim demais
- competicao torna o tip economicamente injustificavel

Tip dinamico considera:

- lucro esperado
- score de competicao
- confianca
- urgencia
- falhas recentes
- performance agregada de relay
- pressure map e forecast pre-block

Estado atual:

- ja existe estrutura de relay stats e adaptive thresholds
- ainda falta inferencia completa de builder identity
- ainda falta mapa por builder/relay de bundle competition

Proxima evolucao natural:

```text
src/mev/builder/
  builder_intel.rs
  relay_router.rs
  bundle_competition_map.rs
```

Objetivo:

- saber qual relay ignora bundles
- saber qual builder exige mais tip por classe de oportunidade
- parar de mandar bundle para endpoint ruim
- ajustar tip por builder, nao por media global

## Controle real de execucao

O gargalo operacional mais perigoso era perda de estado em memoria. Foram adicionadas camadas de controle de execucao para evitar nonce corrompido, lifecycle perdido e duplicacao.

### NonceManager

Modulo:

- `src/mev/execution/nonce_manager.rs`

Responsabilidades:

- reservar nonce local por wallet
- reconciliar com pending nonce da chain
- registrar nonce submetido
- liberar reserva se assinatura falha antes de envio
- finalizar nonce quando receipt/truth chega
- snapshot/restore para recovery

Regra conservadora:

```text
se chain pending nonce divergir, chain truth vence
```

Isso evita reutilizar nonce errado. O sistema prefere pular para o nonce seguro do que arriscar colisao.

### TxLifecycleManager

Modulo:

- `src/mev/execution/tx_lifecycle.rs`

Estados:

```text
Built
Signed
Submitted
Included
Dropped
Outbid
Replaced
Cancelled
Reverted
```

Garantias:

- transicoes invalidas sao ignoradas
- replay e idempotente
- snapshot/restore preserva lifecycle
- indexacao por `tx_hash` e `(wallet, nonce)`

Isso transforma a execucao em uma state machine, nao em logs soltos.

### Replacement policy

Modulo:

- `src/mev/execution/replacement_engine.rs`

Estado atual:

- policy conservadora de bump
- limite de tentativas
- cap de gas

Ainda nao faz rebroadcast/cancel autonomo em runtime. O modulo existe para evitar que replacement futuro vire espiral de gas.

## Recovery, snapshot e durabilidade

A camada de durabilidade vive em:

- `src/mev/state/event_store.rs`
- `src/mev/state/snapshot.rs`
- `src/mev/state/recovery.rs`
- `src/mev/state/snapshot_daemon.rs`
- `src/mev/state/drift_checker.rs`

### Event Store

Modulo:

- `src/mev/state/event_store.rs`

Formato:

- append-only JSONL
- segmentado
- sequencia monotonia
- timestamp em ms
- replay incremental por sequence

Eventos criticos:

- `ExecutionEvent`
- `NonceReserved`
- `TxSigned`
- `TxSubmitted`
- `TxIncluded`
- `TxDropped`
- `TxReplaced`
- `TxCancelled`
- `RiskDecision`
- `InclusionTruthUpdate`
- `MarketTruthUpdate`

Regras:

- nunca muta evento antigo
- rotaciona segmento quando passa do limite
- flush controlado por threshold
- replay ordenado por `(timestamp_ms, sequence)`

`MarketTruthUpdate` e o evento economico pos-trade. Ele nao participa da decisao de executar; ele so registra a verdade economica observada depois do receipt. Campos atuais:

- `tx_hash`
- `outcome`
- `edge_real_value`
- `adverse_selection_score`
- `fill_quality_score`
- `execution_toxicity_index`
- `opportunity_consumed_ratio`
- `alpha_decay_estimate`
- `late_entry_probability`
- `competitor_capture_likelihood`
- `edge_survival_probability`
- `decay_velocity`
- `execution_viability_window_ms`
- `lost_alpha`
- `inefficiency_score`
- `missed_opportunity`

Essa extensao separa tres verdades diferentes:

```text
Inclusion Truth     = a transacao/bundle entrou ou nao entrou
Market Truth        = a execucao foi economicamente boa ou toxica
Opportunity Reality = a oportunidade ainda existia ou ja tinha sido consumida
```

Um bundle pode ser incluido e ainda assim virar `IncludedToxicFill`. Um bundle pode falhar inclusao e ainda assim virar `MissedOpportunity` se o replay observar alpha capturavel. Essa distincao e proposital.

Configuracao por env:

```env
MEV_EVENT_MAX_SEGMENT_SIZE=8388608
MEV_EVENT_FLUSH_THRESHOLD=256
```

### Snapshot Store

Modulo:

- `src/mev/state/snapshot.rs`

Snapshot contem:

- nonce state por wallet
- pending nonces
- pending executions
- lifecycle state map
- active positions placeholder
- risk summary
- ultimo bloco processado
- ultimo event sequence

Snapshot nao guarda historico bruto. Ele guarda estado compacto para acelerar boot. O historico fica no event store.

Save e atomico:

```text
write snapshot.tmp -> rename snapshot.json
```

Isso reduz chance de arquivo parcial apos crash.

### Recovery Engine

Modulo:

- `src/mev/state/recovery.rs`

Boot sequence:

1. carrega ultimo snapshot
2. aplica snapshot nos managers
3. replay do event store apos `last_event_sequence`
4. reconstrucao de `NonceManager`
5. reconstrucao de `TxLifecycleManager`
6. instala event store nos managers
7. sistema retoma sem duplicar nonce/lifecycle

Regra central:

```text
estado apos recovery == estado antes do crash, do ponto de vista da state machine
```

### Snapshot Daemon

Modulo:

- `src/mev/state/snapshot_daemon.rs`

Responsabilidades:

- rodar off-path em Tokio task
- salvar snapshot a cada intervalo
- flush do event store por threshold
- rotacao de segmento
- snapshot forcado via canal bounded
- tolerar falhas silenciosamente e tentar novamente no ciclo seguinte

Configuracao:

```env
MEV_SNAPSHOT_INTERVAL_MS=30000
MEV_EVENT_FLUSH_THRESHOLD=256
MEV_EVENT_MAX_SEGMENT_SIZE=8388608
```

Default e conservador: nao agressivo demais para nao competir com hot path.

### Drift Checker

Modulo:

- `src/mev/state/drift_checker.rs`

Compara snapshot e live state:

- mismatch de nonce por wallet
- divergencia de lifecycle
- pending execution ausente
- desvio de risk summary

Output:

```rust
StateDriftReport {
    drift_detected: bool,
    severity: Low | Medium | High,
    mismatches: Vec<String>,
}
```

Se drift e `High` no startup:

- nao para execucao
- loga warning
- salva snapshot forcado de resync

Isso e self-healing de estado, nao motor de decisao.

## Inclusion Truth vs Market Truth

Antes, o sistema sabia principalmente:

```text
included / not included / reverted / outbid / late
```

Agora existe uma camada adicional:

```text
economic outcome truth
```

Principio:

```text
Execution success is not inclusion.
Execution success is economic outcome.
```

### InclusionTruthEngine

Modulo:

- `src/mev/inclusion_truth.rs`

Classifica outcome operacional:

- `Included`
- `NotIncluded`
- `Outbid`
- `Reverted`
- `LateInclusion`

Base:

- receipt
- block inclusion
- gas real
- competing signals
- target block

Isso ainda nao diz se o trade foi bom economicamente.

### Market Truth Layer

Modulos:

- `src/mev/market_truth/execution_outcome_real.rs`
- `src/mev/market_truth/markout_engine.rs`
- `src/mev/market_truth/competition_reality.rs`
- `src/mev/market_truth/edge_survival.rs`
- `src/mev/market_truth/execution_replay_engine.rs`
- `src/mev/market_truth/truth_pipeline.rs`

Essa camada responde perguntas que inclusion truth nao responde:

- fizemos dinheiro ou so fomos incluidos?
- o fill foi toxico?
- entramos tarde em alpha ja consumido?
- competidor capturou edge antes da nossa execucao?
- a inclusao foi economicamente irrelevante?

Regra de design:

```text
Market Truth nao decide trade.
Market Truth nao altera risk.
Market Truth nao altera estrategia.
Market Truth so classifica resultado economico depois do fato.
```

O objetivo e criar inteligencia adversarial pos-mortem:

- quando o sistema estava certo mas perdeu dinheiro
- quando estava errado mas ganhou por acaso
- quando o alpha ja estava consumido antes da execucao
- quando inclusao foi inutil economicamente
- quando o fill foi toxico apesar de lucro bruto

### ExecutionOutcomeReal

Enum:

```rust
pub enum ExecutionOutcomeReal {
    IncludedProfit,
    IncludedLoss,
    IncludedAdverseSelection,
    IncludedToxicFill,
    IncludedLateFill,
    IncludedPartialCapture,
    MissedOpportunity,
    NotIncluded,
    Reverted,
}
```

Derivado apenas de:

- resultado de execucao
- entry price
- exit/markout price
- fill quality
- slippage
- latency
- post-trade price movement

Nao usa estrategia nem score original.

Classificacao atual:

- `IncludedProfit`: incluido e economicamente favoravel.
- `IncludedLoss`: incluido com valor liquido/markout negativo.
- `IncludedAdverseSelection`: incluido, mas markout indica selecao adversa.
- `IncludedToxicFill`: fill ruim/toxico pelo indice de toxicidade.
- `IncludedLateFill`: entrou tarde em relacao ao target.
- `IncludedPartialCapture`: captura parcial ou dados economicos insuficientes para chamar de profit real.
- `MissedOpportunity`: nao entrou, mas sinais pos-trade indicam oportunidade consumida por competidor.
- `NotIncluded`: nao entrou e nao ha evidencia economica suficiente de alpha perdido.
- `Reverted`: receipt indica revert.

Ponto importante:

```text
IncludedProfit exige evidencia economica.
Sem price series/markout, o sistema nao inventa profit.
```

### Markout Engine

Modulo:

- `src/mev/market_truth/markout_engine.rs`

Formula:

```text
M(t, delta) = P(t + delta) - P(entry)
```

Deltas:

- `100ms`
- `500ms`
- `1s`
- `5s`

Metricas:

- `edge_real_value`
- `adverse_selection_score`
- `fill_quality_score`
- `execution_toxicity_index`

Contrato publico:

```rust
pub struct MarkoutResult {
    pub markout_100ms: f64,
    pub markout_500ms: f64,
    pub markout_1s: f64,
    pub markout_5s: f64,
    pub edge_real_value: f64,
    pub adverse_selection_score: f64,
    pub fill_quality_score: f64,
    pub execution_toxicity_index: f64,
}
```

Regra:

- usa somente dados de mercado apos execucao
- deterministico
- replayable

Interpretacao:

- markout positivo apos execucao sugere edge preservada
- markout negativo rapido sugere toxic fill ou adverse selection
- fill quality mede distancia entre preco esperado/entry e preco efetivo
- toxicity combina selecao adversa e qualidade de fill

Sem snapshots pos-execucao suficientes, o resultado padrao e neutro/conservador. Isso evita contaminar aprendizado com lucro ficticio.

### Competition Reality

Modulo:

- `src/mev/market_truth/competition_reality.rs`

Modelo de consumo da oportunidade:

- `opportunity_consumed_ratio`
- `alpha_decay_estimate`
- `late_entry_probability`
- `competitor_capture_likelihood`

Opera por cluster:

```text
pool + token_in + token_out
```

Nao assume intencao. Infere apenas de dados observaveis.

Contrato publico:

```rust
pub struct CompetitionReality {
    pub opportunity_consumed_ratio: f64,
    pub alpha_decay_estimate: f64,
    pub late_entry_probability: f64,
    pub competitor_capture_likelihood: f64,
}
```

Leitura operacional:

- `opportunity_consumed_ratio`: quanto da oportunidade parece ter sido consumida.
- `alpha_decay_estimate`: diferenca observavel entre alpha antes/depois.
- `late_entry_probability`: chance de termos entrado tarde.
- `competitor_capture_likelihood`: probabilidade de competidor ter capturado a edge.

Isso e pos-trade intelligence. Nao bloqueia trade no fluxo atual.

### Edge Survival

Modulo:

- `src/mev/market_truth/edge_survival.rs`

Metricas:

- `survival_probability`
- `decay_velocity`
- `execution_viability_window_ms`

Incorpora:

- competition pressure
- mempool congestion
- historical markout degradation
- latency risk

Contrato publico:

```rust
pub struct EdgeSurvival {
    pub survival_probability: f64,
    pub decay_velocity: f64,
    pub execution_viability_window_ms: u64,
}
```

Leitura:

- `survival_probability` baixo indica que oportunidades similares decaem rapido.
- `decay_velocity` alto indica que latencia/competicao/mempool estao consumindo alpha.
- `execution_viability_window_ms` estima a janela economica para trades similares.

Esse dado deve alimentar analise e calibracao futura off-path, nao o decision flow atual.

### Truth Pipeline

Modulo:

- `src/mev/market_truth/truth_pipeline.rs`

Fluxo:

```text
InclusionTruth
  -> MarkoutEngine
  -> CompetitionRealityEngine
  -> EdgeSurvivalEngine
  -> ExecutionReplayEngine
  -> ExecutionOutcomeReal
  -> EventStore::MarketTruthUpdate
```

Integracao atual:

- hook pos-receipt em `block_loop.rs`
- append-only em `EventStore`
- nao altera decision flow
- nao altera risk
- nao altera strategy scoring

Contrato de entrada:

```rust
pub struct MarketTruthInput {
    pub truth: InclusionTruth,
    pub entry_timestamp_ms: u64,
    pub entry_price: f64,
    pub execution_price: f64,
    pub net_execution_value: f64,
    pub slippage_bps: f64,
    pub fill_ratio: f64,
    pub market_snapshots: Vec<MarketSnapshot>,
    pub competition: CompetitionRealityInput,
    pub survival: EdgeSurvivalInput,
    pub expected_execution_value: f64,
    pub observed_best_execution_value: f64,
}
```

Contrato de saida:

```rust
pub struct MarketTruthReport {
    pub tx_hash: H256,
    pub outcome: ExecutionOutcomeReal,
    pub markout: MarkoutResult,
    pub competition: CompetitionReality,
    pub survival: EdgeSurvival,
    pub replay: ReplayResult,
}
```

O pipeline e deterministico para o mesmo conjunto de inputs. Ele nao acessa RPC live e nao usa estrategia como sinal.

Limitacao honesta:

- o hook atual ainda nao recebe feed real de snapshots de mercado
- se nao ha snapshots de mercado, o sistema nao inventa markout
- nesse caso, outcome incluido tende a ser conservador/partial capture ate que dados reais sejam conectados

Proximo passo ideal:

```text
src/mev/market_data/
  pool_price_snapshots.rs
  markout_sampler.rs
  replay_snapshot_loader.rs
```

## Replay offline deterministico

Modulo:

- `src/mev/market_truth/execution_replay_engine.rs`

Objetivo:

- reproduzir execucoes passadas sem depender de RPC live
- comparar execucao real contra melhor execucao hipotetica observavel
- calcular alpha perdido
- gerar mapa de ineficiencia

Outputs:

- `lost_alpha_per_trade`
- `execution_inefficiency_map`
- `missed_opportunity_heatmap`

Contrato resumido usado no pipeline online:

```rust
pub struct ReplayResult {
    pub lost_alpha: f64,
    pub inefficiency_score: f64,
    pub missed_opportunity: f64,
}
```

O replay compara:

```text
actual_execution
vs
best_possible_execution observado na serie de mercado
```

Metricas:

- `lost_alpha`: diferenca positiva entre melhor execucao observada e execucao real.
- `inefficiency_score`: alpha perdido normalizado.
- `missed_opportunity`: intensidade da oportunidade perdida quando nao houve inclusao.

Isso e essencial para calibrar o sistema sem autoengano. O replay deve responder:

```text
o trade era bom?
entramos tarde?
o fill foi ruim?
o lucro teorico existiu mesmo apos markout?
```

## Configuracao de durabilidade recomendada

Producao conservadora:

```env
MEV_SNAPSHOT_INTERVAL_MS=30000
MEV_EVENT_FLUSH_THRESHOLD=128
MEV_EVENT_MAX_SEGMENT_SIZE=8388608
```

Ambiente mais agressivo:

```env
MEV_SNAPSHOT_INTERVAL_MS=10000
MEV_EVENT_FLUSH_THRESHOLD=32
MEV_EVENT_MAX_SEGMENT_SIZE=4194304
```

Trade-off:

- flush menor reduz perda potencial apos crash, mas aumenta I/O
- snapshot frequente melhora recovery, mas pode competir por disco
- segmento menor facilita rotacao/replay parcial, mas cria mais arquivos

O daemon roda off-path, mas disco lento ainda pode afetar o processo se o sistema inteiro estiver sob pressao. Em producao real, usar SSD local e separar storage de logs pesados.

## O que esta protegido hoje

Protegido:

- nonce reservado em memoria e reconstruivel
- tx signed/submitted reconstruivel por event replay
- lifecycle replay idempotente
- event store append-only
- snapshot compacto
- drift check no boot
- daemon de snapshot/flush/rotation
- inclusion truth por receipt
- market truth append-only
- survival-mode hard gate ja existe no executor

Parcialmente protegido:

- realized PnL positivo ainda precisa de balance deltas reais por token
- markout real depende de feed de snapshots de mercado
- builder identity ainda nao e inferido plenamente
- chain pending nonce reconciliation existe na recovery layer, mas precisa ser chamado com wallets conhecidas no boot para maxima seguranca

Nao protegido ainda:

- crash no meio de uma chamada RPC antes do event append pode deixar uma acao externa sem evento local se o evento ainda nao foi gravado
- orderflow privado de competidores nao e observavel
- relay/builder simulation real ainda nao substitui completamente preflight local
- pool/tick cache completo ainda nao existe

## Onde o sistema ainda perde dinheiro

Mesmo com as camadas atuais, o sistema ainda pode perder em:

- latencia de pending tx lookup
- estado AMM stale
- builder nao incluindo bundle
- competidor com orderflow privado
- fill toxico que parece lucrativo no bloco mas degrada no markout
- gas/tip repricing entre simulacao e inclusao
- missing market data para classificacao economica completa

O ponto mais importante:

```text
uma execucao incluida e operacionalmente correta, mas pode ser economicamente ruim
```

Por isso a Market Truth Layer existe.

### Logica atual da Market Truth no runtime

No runtime atual, o hook de `block_loop.rs` roda depois de `InclusionTruthEngine` e depois da finalizacao de lifecycle/nonce. Ele monta um `MarketTruthInput` com os dados observaveis disponiveis naquele momento.

Estado atual dos dados:

- `InclusionTruth`: real, vindo de receipt/reconcile.
- `latency_ms`: real, derivado do tempo entre submit e reconcile.
- `gas_used` e `effective_gas_price`: reais quando receipt existe.
- `competition_score`: observavel do fluxo de inclusion/competition.
- `market_snapshots`: ainda vazio no hook online.
- `entry_price`/`execution_price`: ainda nao conectados a feed real de mercado.
- `observed_best_execution_value`: placeholder conservador baseado no esperado, ate haver replay com serie real.

Consequencia:

```text
o sistema ja grava MarketTruthUpdate,
mas ainda nao deve tratar IncludedProfit como prova economica forte
sem price snapshots e balance deltas reais.
```

Essa escolha e intencional. E melhor classificar como parcial/insuficiente do que ensinar o sistema com lucro ficticio.

Para ativar verdade economica forte, faltam dois feeds:

1. price snapshots pos-execucao por pool/rota
2. balance delta real por token depois da execucao

Quando esses feeds forem conectados, a mesma camada passa a produzir:

- profit/loss economico real
- toxic fill real
- markout real
- lost alpha real
- opportunity decay real

## Roadmap senior recomendado

Ordem recomendada para evoluir sem aumentar risco:

1. Conectar snapshots reais de mercado ao `markout_engine`.
2. Persistir `MarketTruthUpdate` tambem em snapshot summary.
3. Implementar balance delta real por token para PnL positivo.
4. Adicionar builder identity inference.
5. Criar `relay_router.rs` por classe de oportunidade.
6. Implementar pool/tick cache por logs.
7. Fazer replay deterministico diario e recalibrar thresholds off-path.
8. Adicionar transaction replacement real apenas apos PnL/markout confiaveis.

Nao recomendado agora:

- trocar Rust por Go no hot path
- aumentar volume antes de markout real
- abrir public mempool fallback
- reduzir thresholds para "ver mais oportunidades"
- confiar em lucro estimado sem balance delta

## Resumo honesto do nivel atual

O sistema hoje esta no nivel:

```text
MEV research/execution engine com:
  - execucao conservadora
  - awareness adversarial
  - recovery deterministico
  - inclusion truth
  - market truth scaffold
  - durability daemon
```

Ainda nao e "top-tier institutional searcher" porque faltam:

- visao completa de mempool privado
- builder/relay intelligence por contraparte
- market snapshots reais integrados ao markout
- balance-delta accounting completo
- cache local completo de pools/ticks
- co-location/network stack ultra otimizada

Mas a base correta foi estabelecida:

```text
decidir pouco,
executar com cuidado,
persistir tudo,
reconstruir deterministicamente,
e julgar resultado por verdade economica, nao por inclusao.
```
