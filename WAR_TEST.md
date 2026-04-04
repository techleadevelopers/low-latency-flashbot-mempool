## War Test

Use this flow to measure real reaction time, queue behavior, RPC degradation, and the complete execution path before live mode.

### Hard mode prerequisites

1. Use the real delegated contract, not `0x1111...`.
2. Keep at least one real delegated wallet in `keys.txt`.
3. Keep `BOT_MODE=shadow` first.
4. Open the dashboard at `http://127.0.0.1:8787`.
5. Record the latency pipeline after every test round.

### Base hard-mode config

```env
NETWORK=arbitrum
CHAIN_ID=42161
MIN_BALANCE=0.01
MIN_SCAN_INTERVAL_MS=250
MAX_SCAN_INTERVAL_MS=1500
SCAN_CONCURRENCY=12
BATCH_SCAN_SIZE=12
MAX_INFURA_ENDPOINTS=2
RPC_RATE_LIMIT_COOLDOWN_SECS=180
WALLET_COOLDOWN_SECS=20
QUEUE_DEDUPE_SECS=10
BOT_MODE=shadow
ALLOW_SEND=false
```

### Synthetic backend-only hard mode

Use this when you want to stress the backend even with a mock contract.

```env
BOT_MODE=shadow
ALLOW_SEND=false
MOCK_CONTRACT_MODE=true
MOCK_HOT_WALLET_COUNT=3
MOCK_HOT_BALANCE_ETH=0.011
```

This injects synthetic hot wallets into the monitor and skips on-chain contract reads during shadow execution.

### Scenario 1: Controlled hot wallet

Goal: measure raw reaction time for one delegated wallet.

1. Choose one delegated wallet from `keys.txt`.
2. Send `0.011 ETH` to that wallet.
3. Record:
   - time until it appears in `Wallets Over Threshold`
   - `enqueue_latency`
   - `queue_wait`
   - `tx_prepare`
   - `bundle_attempt`

Pass signal:
- wallet appears quickly
- `scan_cycle` stays under `1500 ms`
- `tx_prepare` stays under `500 ms`

### Scenario 2: Multi-hit

Goal: verify queue behavior under burst load.

1. Choose 3 to 5 delegated wallets.
2. Send `0.011 ETH` to all of them with as little spacing as possible.
3. Observe:
   - if all appear in `Wallets Over Threshold`
   - if `queue_wait` grows too much
   - if `scan_cycle` spikes
   - if sweep attempts are serialized cleanly

Pass signal:
- queue stays ordered
- no wallet is silently skipped
- `queue_wait` stays controlled

### Scenario 2B: Internal burst across 85 wallets

Goal: simulate your own ecosystem depositing fee residue into any of the 85 managed wallets with minimum reaction time.

1. Keep exactly 85 authorized wallets loaded.
2. Run the bot in `shadow`.
3. Trigger deposits into:
   - one random wallet
   - 10 wallets at once
   - all 85 wallets in one burst
4. Record for each burst:
   - first appearance time in the dashboard
   - p50, p95 and max `scan_cycle`
   - p50, p95 and max `enqueue_latency`
   - p50, p95 and max `queue_wait`
   - total time until all 85 candidates are queued
   - total time until all 85 attempts are prepared

Pass signal:
- first wallet is detected within one scan window
- no candidate is lost during burst
- queue order still follows residual profit priority
- `scan_cycle` remains stable enough to keep reaction usable under 85-wallet load

Recommended local automation:

```powershell
cargo test burst -- --nocapture
cargo test stress_burst_85_wallets_reports_local_queue_time -- --ignored --nocapture
```

### Scenario 3: RPC degradation

Goal: verify endpoint selection under stress.

1. Keep only `Alchemy + 1 Infura` or `Alchemy + 2 Infuras`.
2. Run the bot long enough to accumulate real `429` and cooldown events.
3. Watch:
   - `429` counters
   - cooldowns
   - stale endpoints
   - whether `Alchemy` becomes the preferred healthy route

Pass signal:
- rate-limited endpoints enter cooldown
- the bot keeps scanning through the healthiest endpoint
- `scan_cycle` remains usable

### Scenario 4: Paper mode

Goal: test the full execution path with real sends limited to a safe wallet set.

Recommended config:

```env
BOT_MODE=paper
ALLOW_SEND=true
TEST_WALLET_ALLOWLIST=0xWALLET_1,0xWALLET_2
MAX_SWEEP_VALUE_ETH=0.005
```

1. Use the real contract.
2. Keep the allowlist tiny.
3. Use small value only.
4. Observe:
   - `tx_prepare`
   - `bundle_attempt`
   - sweep success/failure path
   - public fallback behavior if relay fails

Pass signal:
- only allowlisted wallets are allowed through
- values above the cap are blocked
- send path behaves exactly as expected

### Target numbers

- `block_fetch`: under `300 ms`
- `scan_cycle`: under `1500 ms`
- `enqueue_latency`: low and consistent
- `queue_wait`: low under one wallet, acceptable under burst
- `tx_prepare`: under `500 ms`
- `bundle_attempt`: as low and stable as possible

### What means trouble

- many `429` on Infura
- `scan_cycle` above `3000 ms`
- stale endpoints dominating selection
- hot wallet appearing late after deposit
- queue growing and staying high
- paper mode sending outside the allowlist

### Promotion path

1. `shadow` with one hot wallet
2. `shadow` with 3 to 5 hot wallets
3. `shadow` under RPC degradation
4. `paper` with tiny allowlist and tiny max value
5. `live` only after stable latency and low RPC error rate
