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
